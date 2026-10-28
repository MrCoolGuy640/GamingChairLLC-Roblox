//! Tovek Luau decompiler, embedded in this crate.
//!
//! Turns Luau **bytecode** back into readable Luau source, in-process. No
//! executable, server or Python involved.
//!
//! ```ignore
//! mod decompiler;
//!
//! let source = decompiler::decompile(&bytecode)?;
//! ```
//!
//! See `README.md` next to this folder for setup, options and examples.
//!
//! The decompiler itself lives in the five vendored crates under
//! `engine/` and is pulled in with a single `tovek = { path = ... }` line in
//! your `Cargo.toml`. This file only gives them a home inside your own module
//! tree.

#[allow(unused_imports)] // part of the module's public surface
pub use tovek::embed::{
    DecompileError, Decompiler, DecompilerBuilder, EncodeKey, PLAIN_KEY, ROBLOX_KEY, decompile,
    decompile_roblox,
};

/// Upstream Tovek's own low-level API (`DecompileOptions`,
/// `try_decompile_bytecode_with_options`, `decompile_batch`, ...), for anything
/// the friendlier API above does not cover.
#[allow(unused_imports)] // part of the module's public surface
pub use tovek as raw;
