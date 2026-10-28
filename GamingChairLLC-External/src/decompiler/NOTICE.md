# Notice

This folder embeds **Tovek**, a Luau decompiler by Kiet1308 (<https://github.com/Kiet1308/Tovek>,
version 2.6.1), itself a fork of **medal** by Jujhar Singh and Mathias Pedersen
(<https://github.com/Stefanuk12/medal-decompiler>). Tovek is MIT licensed; the licence text is in
`LICENSE.txt` and **must stay with the code**.

`engine/petgraph/` is a vendored copy of the `jujhar16/petgraph` fork
(branch `ensure_len_resize_with`, commit `b40d963`, petgraph 0.6.3), which upstream
Tovek depends on. It is dual licensed MIT OR Apache-2.0
(`engine/petgraph/LICENSE-MIT`, `engine/petgraph/LICENSE-APACHE`).

## What is different from upstream

The decompiler algorithms are untouched. Changes are limited to packaging and to
removing the need for a nightly compiler:

1. **Only the library is included.** From upstream's workspace this folder contains
   `ast`, `cfg`, `restructure` and the `luau-lifter` *library*. The CLI
   (`main.rs`, `batch.rs`, `decompile_core.rs`, `decompile_cache.rs`, `validate.rs`), the
   web server, the Lua 5.1 lifter, the WebAssembly worker, the Python/C++ scripts, tests,
   fixtures and docs are not included. `luau-lifter` is renamed `tovek`.
2. **Manifests rewritten** so the crates stand alone (no workspace inheritance), are
   renamed `tovek-*` (library names stay `ast`, `cfg`, `restructure`, `petgraph`),
   no longer pull in CLI-only dependencies (`clap`, `mimalloc`, `dhat`, `walkdir`, `fs2`,
   `lazy_static`, `either`, `base64`), and have `test = false` so `cargo test --workspace` in your
   project does not run their ~1,500 upstream tests.
3. **Builds on stable Rust (>= 1.95).** Upstream pins `nightly-2026-06-15` for four unstable
   features. `let_chains` and `if_let_guard` are stable since 1.88 / 1.95, `iter_order_by` was
   enabled but never used, and the remaining two were rewritten without changing behaviour:
   * `box_patterns` (13 patterns): `ast/src/{binary,formatter,select_value,type_system,unary}.rs`,
     `cfg/src/ssa/inline.rs`; a `box x` pattern became a plain binding plus an explicit deref.
   * `box_into_inner` (1 call): `cfg/src/ssa/inline.rs`; `Box::into_inner(b)` became `*b`.
4. **Petgraph vendored** instead of fetched from GitHub by branch name, so builds are
   reproducible and need no git access.
5. **`engine/tovek/src/embed.rs` is new** (plus one `pub mod embed;` line in `lib.rs`): the
   in-process API (`Decompiler`, worker pool with large stacks, scoped panic filter).
6. `mod.rs` is new: the thin module you declare in your own crate.

To update to a newer Tovek, diff upstream's `ast/`, `cfg/`, `restructure/` and
`luau-lifter/src/` against these folders and re-apply the items above.
