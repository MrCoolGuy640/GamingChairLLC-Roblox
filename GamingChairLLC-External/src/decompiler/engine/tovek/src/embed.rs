//! In-process embedding API for the Tovek decompiler.
//!
//! This module is **not part of upstream Tovek**. It was added so the
//! decompiler can be called as a plain Rust library from another program
//! instead of being run as the `luau-lifter` CLI or the `web-server`.
//!
//! It takes care of the process-level details the upstream binaries handle in
//! their `main()`:
//!
//! * **Big stacks.** The decompiler recurses once per `elseif` arm, and a stack
//!   overflow aborts the whole process (it cannot be caught). Every decompile
//!   therefore runs on a dedicated rayon pool whose threads have
//!   [`DECOMPILE_STACK_BYTES`](crate::DECOMPILE_STACK_BYTES) stacks. Your own
//!   threads and your own global rayon pool are never touched.
//! * **Scoped panic silencing.** Upstream deliberately panics inside individual
//!   functions it cannot lift and recovers with `catch_unwind`. Upstream's
//!   binaries install a global "ignore all panics" hook; that would be rude in
//!   a library. Here the hook is chained: it only mutes panics raised on the
//!   decompiler's own worker threads and forwards everything else to whatever
//!   hook was installed before.
//! * **Key handling.** Roblox client bytecode has its opcodes scrambled with key
//!   `203`; plain `luau-compile` output uses `1`. See [`EncodeKey`].
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let bytecode: Vec<u8> = Vec::new();
//! let source = tovek::embed::decompile(&bytecode)?;
//! println!("{source}");
//! # Ok(()) }
//! ```

use std::sync::{Arc, Once, OnceLock};

use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::{
    BatchInput, ControlFlowOutputPolicy, DECOMPILE_STACK_BYTES, DecompileOptions,
    decompile_batch_with_options, try_decompile_bytecode_with_options,
};

// The decompiler relies on `catch_unwind` to survive the occasional function it
// cannot lift. With `panic = "abort"` every such recovery would instead kill
// the host process, so refuse to build rather than fail at runtime.
#[cfg(panic = "abort")]
compile_error!(
    "Tovek's decompiler needs `panic = \"unwind\"` (the default). Remove \
     `panic = \"abort\"` from the Cargo profile you are building with."
);

/// Opcode key of Roblox client bytecode (e.g. from `getscriptbytecode`).
pub const ROBLOX_KEY: u8 = 203;
/// Opcode key of unencoded Luau bytecode (e.g. from `luau-compile --binary`).
pub const PLAIN_KEY: u8 = 1;

/// Name prefix of the decompiler's worker threads (used by the panic filter).
const WORKER_PREFIX: &str = "tovek-decompile";

/// How the opcodes of the input bytecode are encoded (`op = op * key % 256`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EncodeKey {
    /// Try plain Luau bytecode (key `1`); if that cannot even be deserialized,
    /// retry as Roblox client bytecode (key `203`). Same behaviour as the
    /// `luau-lifter <file>` CLI. This is the default.
    #[default]
    Auto,
    /// Roblox client bytecode, key `203`. Skips the plain attempt.
    Roblox,
    /// Plain Luau bytecode, key `1`.
    Plain,
    /// Any other key.
    Custom(u8),
}

/// Why a script could not be decompiled.
///
/// The text is the decompiler's own message, e.g. `deserialize: ...` for
/// malformed or unsupported bytecode, or a description of the function it could
/// not structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecompileError {
    message: String,
}

impl DecompileError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    /// The decompiler's error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for DecompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DecompileError {}

// ---------------------------------------------------------------------------
// Worker pool
// ---------------------------------------------------------------------------

/// Chain a panic hook that silences only the decompiler's worker threads.
fn install_scoped_panic_filter() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = std::thread::current()
                .name()
                .is_some_and(|name| name.starts_with(WORKER_PREFIX));
            if !ours {
                previous(info);
            }
        }));
    });
}

fn build_pool(threads: usize) -> Result<ThreadPool, DecompileError> {
    install_scoped_panic_filter();
    let mut builder = ThreadPoolBuilder::new()
        .stack_size(DECOMPILE_STACK_BYTES)
        .thread_name(|index| format!("{WORKER_PREFIX}-{index}"));
    if threads != 0 {
        builder = builder.num_threads(threads);
    }
    builder
        .build()
        .map_err(|error| DecompileError::new(format!("could not start decompiler threads: {error}")))
}

/// The process-wide pool shared by every [`Decompiler`] that did not ask for
/// its own thread count. Created on first use, one worker per logical CPU.
fn default_pool() -> Result<Arc<ThreadPool>, DecompileError> {
    static POOL: OnceLock<Arc<ThreadPool>> = OnceLock::new();
    if let Some(pool) = POOL.get() {
        return Ok(Arc::clone(pool));
    }
    let pool = Arc::new(build_pool(0)?);
    Ok(Arc::clone(POOL.get_or_init(|| pool)))
}

// ---------------------------------------------------------------------------
// Decompiler
// ---------------------------------------------------------------------------

/// A configured decompiler. Cheap to clone; `Send + Sync`, so one instance can
/// be shared between threads (calls simply queue on its worker pool).
///
/// ```no_run
/// use tovek::embed::{Decompiler, EncodeKey};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let bytecode: Vec<u8> = Vec::new();
/// let decompiler = Decompiler::builder()
///     .encode_key(EncodeKey::Roblox)
///     .compact_style(true)
///     .build()?;
/// let source = decompiler.decompile_named(&bytecode, "ReplicatedStorage/Events")?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Decompiler {
    /// `None`: the shared default pool.
    pool: Option<Arc<ThreadPool>>,
    key: EncodeKey,
    options: DecompileOptions,
}

impl Default for Decompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Decompiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decompiler")
            .field("key", &self.key)
            .field("options", &self.options)
            .field("dedicated_pool", &self.pool.is_some())
            .finish()
    }
}

impl Decompiler {
    /// Default settings: [`EncodeKey::Auto`], default options, shared pool.
    pub fn new() -> Self {
        Self { pool: None, key: EncodeKey::default(), options: DecompileOptions::default() }
    }

    /// Start configuring a decompiler.
    pub fn builder() -> DecompilerBuilder {
        DecompilerBuilder::default()
    }

    /// Decompile one script's bytecode to Luau source.
    pub fn decompile(&self, bytecode: &[u8]) -> Result<String, DecompileError> {
        self.decompile_inner(bytecode, None)
    }

    /// Like [`decompile`](Self::decompile), with the script's name or path
    /// (`"ReplicatedStorage/Events"`, `"Widget/init.lua"`). The name feeds
    /// naming heuristics, for example module scripts are named after it.
    pub fn decompile_named(&self, bytecode: &[u8], script_name: &str) -> Result<String, DecompileError> {
        self.decompile_inner(bytecode, Some(script_name))
    }

    /// Decompile many scripts in parallel. The results are in input order, and
    /// one failing script never affects the others. Identical scripts are
    /// decompiled once.
    pub fn decompile_many<I, B>(&self, scripts: I) -> Vec<Result<String, DecompileError>>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let owned: Vec<B> = scripts.into_iter().collect();
        let items: Vec<(Option<&str>, &[u8])> = owned.iter().map(|b| (None, b.as_ref())).collect();
        self.decompile_batch(&items)
    }

    /// [`decompile_many`](Self::decompile_many) with a name for each script.
    pub fn decompile_many_named<'a, I, B>(&self, scripts: I) -> Vec<Result<String, DecompileError>>
    where
        I: IntoIterator<Item = (&'a str, B)>,
        B: AsRef<[u8]>,
    {
        let owned: Vec<(&str, B)> = scripts.into_iter().collect();
        let items: Vec<(Option<&str>, &[u8])> =
            owned.iter().map(|(name, b)| (Some(*name), b.as_ref())).collect();
        self.decompile_batch(&items)
    }

    // -- internals ----------------------------------------------------------

    /// Run `work` on the worker pool, i.e. on a big-stack thread.
    fn run<R: Send>(&self, work: impl FnOnce() -> R + Send) -> Result<R, DecompileError> {
        match &self.pool {
            Some(pool) => Ok(pool.install(work)),
            None => Ok(default_pool()?.install(work)),
        }
    }

    fn decompile_inner(&self, bytecode: &[u8], name: Option<&str>) -> Result<String, DecompileError> {
        let (key, options) = (self.key, self.options);
        self.run(move || {
            let attempt = |key: u8| try_decompile_bytecode_with_options(bytecode, key, name, options);
            let result = match key {
                EncodeKey::Roblox => attempt(ROBLOX_KEY),
                EncodeKey::Plain => attempt(PLAIN_KEY),
                EncodeKey::Custom(key) => attempt(key),
                EncodeKey::Auto => {
                    let first = attempt(PLAIN_KEY);
                    if is_deserialize_failure(&first) { attempt(ROBLOX_KEY).or(first) } else { first }
                }
            };
            result.map_err(DecompileError::new)
        })?
    }

    fn decompile_batch(&self, items: &[(Option<&str>, &[u8])]) -> Vec<Result<String, DecompileError>> {
        let (key, options) = (self.key, self.options);
        let outcome = self.run(|| {
            let batch = |key: u8, indices: &[usize]| -> Vec<Result<String, String>> {
                let inputs: Vec<BatchInput<'_>> = indices
                    .iter()
                    .map(|&i| BatchInput { bytecode: items[i].1, encode_key: key, script_name: items[i].0 })
                    .collect();
                decompile_batch_with_options(&inputs, options)
            };
            let all: Vec<usize> = (0..items.len()).collect();
            let results = match key {
                EncodeKey::Roblox => batch(ROBLOX_KEY, &all),
                EncodeKey::Plain => batch(PLAIN_KEY, &all),
                EncodeKey::Custom(key) => batch(key, &all),
                EncodeKey::Auto => {
                    let mut results = batch(PLAIN_KEY, &all);
                    let retry: Vec<usize> =
                        all.iter().copied().filter(|&i| is_deserialize_failure(&results[i])).collect();
                    if !retry.is_empty() {
                        for (i, second) in retry.iter().copied().zip(batch(ROBLOX_KEY, &retry)) {
                            if second.is_ok() {
                                results[i] = second;
                            }
                        }
                    }
                    results
                }
            };
            results.into_iter().map(|r| r.map_err(DecompileError::new)).collect::<Vec<_>>()
        });
        match outcome {
            Ok(results) => results,
            // The worker threads could not be started: every script fails alike.
            Err(error) => items.iter().map(|_| Err(error.clone())).collect(),
        }
    }
}

fn is_deserialize_failure(result: &Result<String, String>) -> bool {
    matches!(result, Err(message) if message.starts_with("deserialize:"))
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`Decompiler`]. Every option has the same default as the
/// upstream CLI's single-file mode.
#[derive(Clone, Debug, Default)]
pub struct DecompilerBuilder {
    key: EncodeKey,
    threads: Option<usize>,
    options: DecompileOptions,
}

impl DecompilerBuilder {
    /// How the input's opcodes are encoded. Default: [`EncodeKey::Auto`].
    pub fn encode_key(mut self, key: EncodeKey) -> Self {
        self.key = key;
        self
    }

    /// Use a dedicated pool of `n` worker threads instead of the shared
    /// process-wide pool (`0` = one per logical CPU, but still dedicated).
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = Some(n);
        self
    }

    /// Do not reuse generated local names across functions of one script.
    pub fn dont_reuse_var(mut self, on: bool) -> Self {
        self.options.dont_reuse_var = on;
        self
    }

    /// Disable terminal-helper synthesis (diagnostic / ablation switch).
    pub fn no_synth_helpers(mut self, on: bool) -> Self {
        self.options.no_synth_helpers = on;
        self
    }

    /// Allow relational negation flips without proving the operands are not
    /// NaN. Cleaner output, but behaviour differs if an operand is NaN.
    pub fn assume_no_nan(mut self, on: bool) -> Self {
        self.options.assume_no_nan = on;
        self
    }

    /// Write folded constants as `math.pi`, `math.huge`, `Vector3.new(...)`,
    /// assuming no script replaces those libraries.
    pub fn assume_standard_libraries(mut self, on: bool) -> Self {
        self.options.assume_standard_libraries = on;
        self
    }

    /// Write a scalar select as `if c then a else b` instead of branch
    /// statements (upstream's `--style compact`).
    pub fn compact_style(mut self, on: bool) -> Self {
        self.options.compact_style = on;
        self
    }

    /// Experimental: present fixed-count arithmetic sequences as loops. The
    /// original source loop is unknown, so this is only an equivalent form.
    pub fn synthesize_arithmetic_loops(mut self, on: bool) -> Self {
        self.options.synthesize_arithmetic_loops = on;
        self
    }

    /// `true`: fail instead of ever emitting the synthetic state-machine
    /// dispatcher for control flow that cannot be structured (upstream's batch
    /// folder mode). `false` (default): allow that semantics-preserving
    /// fallback, so you get output rather than an error.
    pub fn strict_control_flow(mut self, on: bool) -> Self {
        self.options.control_flow_policy = if on {
            ControlFlowOutputPolicy::StrictNoSyntheticControl
        } else {
            ControlFlowOutputPolicy::AllowCertifiedDispatcher
        };
        self
    }

    /// Replace all decompile options at once with upstream's raw struct.
    pub fn options(mut self, options: DecompileOptions) -> Self {
        self.options = options;
        self
    }

    /// Finish. Only fails if a dedicated worker pool was requested and the
    /// operating system refused to start its threads.
    pub fn build(self) -> Result<Decompiler, DecompileError> {
        let pool = match self.threads {
            Some(n) => Some(Arc::new(build_pool(n)?)),
            None => None,
        };
        Ok(Decompiler { pool, key: self.key, options: self.options })
    }
}

// ---------------------------------------------------------------------------
// Convenience functions
// ---------------------------------------------------------------------------

/// Decompile with default settings ([`EncodeKey::Auto`]).
pub fn decompile(bytecode: &[u8]) -> Result<String, DecompileError> {
    Decompiler::new().decompile(bytecode)
}

/// Decompile Roblox client bytecode (key `203`) with default settings.
pub fn decompile_roblox(bytecode: &[u8]) -> Result<String, DecompileError> {
    Decompiler::builder().encode_key(EncodeKey::Roblox).build()?.decompile(bytecode)
}
