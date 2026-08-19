# merc-mbt

A model-based testing (MBT) tool for mCRL2 linear process specifications: a synchronous WebSocket
client that maintains a symbolic state set over an LPS and checks IOCO conformance against a test
adapter, speaking the mCRL2 MBT <-> Adapter Protocol. See `docs/merc-mbt-implementation-plan.md`
for the remaining work.

## Layout

- `crates/merc_mbt/` — the library: wire protocol, action/partition classification, the IOCO model
  layer, and the synchronous session event loop. Plain safe Rust, no `unsafe` blocks.
- `mbt/` — the `merc-mbt` CLI binary.

## Relationship to `mlaveaux/merc`

This repository was split out of [mlaveaux/merc](https://github.com/mlaveaux/merc)
(`tools/mcrl2/crates/merc_mbt` and `tools/mcrl2/mbt`), with history preserved via `git filter-repo`.
It builds on that repo's mCRL2/LPS layer — `mcrl2`, `merc_explore`, `merc_lps`, `merc_lts`,
`merc_utilities`, `merc_io`, `merc_tools`, `merc_unsafety` — but depends on those crates as pinned
git dependencies (`[workspace.dependencies]` in the root `Cargo.toml`) rather than vendoring them,
since none of `merc_mbt`'s own code touches the mCRL2 FFI directly.

Cargo resolves a `{ git = "...", rev = "..." }` dependency by package name across the *entire*
cloned repository, not just its root workspace — so `mcrl2` and `merc_lps` (which live in
`mlaveaux/merc`'s separate `tools/mcrl2` sub-workspace) resolve with no `path =` needed, the same as
`merc_explore`/`merc_lts`/etc. from the root workspace.

**Updating the pinned `rev`:** bump every `{ git = "https://github.com/mlaveaux/merc", rev = "..." }`
entry in the root `Cargo.toml` to the same new commit, then `cargo update` and re-run the test suite.
The `[patch.crates-io]` table (needed because `merc_lps` pulls in a forked `oxidd` for LDD support,
and Cargo only honours `[patch]` from the workspace actually being built — a dependency's own
`[patch]` table is ignored) should be kept in sync with
[`mlaveaux/merc`'s own](https://github.com/mlaveaux/merc/blob/main/tools/mcrl2/Cargo.toml).

## Building

The mCRL2 C++ sources are pulled in transitively (via the `mcrl2` crate's own dependency on
`mcrl2-sys`), so the first build compiles the mCRL2 toolset from source and can take several
minutes. A C++ compiler is required (GCC/Clang on Linux, AppleClang on macOS, MSVC on Windows).

```bash
cargo build
cargo nextest run --no-fail-fast -- --include-ignored
```

Tests whose name contains `mcrl2` compare against the upstream mCRL2 toolset and are gated on the
`MCRL2_PATH` environment variable (pointing at a `mcrl22lps`/`lps2lts` build); they skip cleanly
when it is unset.

## Formatting and linting

```bash
cargo +nightly fmt --all   # rustfmt.toml uses nightly-only options (imports_granularity)
cargo clippy --all-targets
```
