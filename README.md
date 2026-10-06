# Overview

A model-based testing (MBT) tool for mCRL2 linear process specifications: a synchronous WebSocket
client that maintains a symbolic state set over an LPS and checks IOCO conformance against a test
adapter, speaking the mCRL2 MBT <-> Adapter Protocol.

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
