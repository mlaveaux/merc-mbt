# `merc_mbt` — remaining TODOs

Phases 0–6 of the original implementation plan (Cargo/workspace wiring, the wire protocol layer,
the action/partition layer, the model layer, the session event loop, the CLI, and the test suite)
are implemented in `crates/merc_mbt/` and `mbt/`. This file now tracks only what is left.

This repository split off from [mlaveaux/merc](https://github.com/mlaveaux/merc)
(`tools/mcrl2/crates/merc_mbt` and `tools/mcrl2/mbt`, history preserved via `git filter-repo`); it
depends on that repo's `mcrl2`/`merc_explore`/`merc_lps`/`merc_lts`/`merc_utilities`/`merc_io`/
`merc_tools`/`merc_unsafety` crates as pinned git dependencies (see README.md) rather than vendoring
them, since this crate's own code is plain safe Rust with no `unsafe` blocks.

- [ ] **Run the `MCRL2_PATH`-gated tests against a real mCRL2 toolset.** `tests/model_test.rs` and
      `tests/session_test.rs` compile and skip cleanly without `MCRL2_PATH` set, but have never
      actually executed against `mcrl22lps`. Once a toolset build is available, run
      `MCRL2_PATH=<path> cargo nextest run -E 'test(mcrl2)'` and fix whatever the real
      grammar/enumerator disagrees with.

- [ ] **Decide on TLS support (`wss://`).** Currently unsupported by design; `tungstenite::connect`
      fails cleanly with a clear error naming the missing feature. If needed, add
      `features = ["rustls-tls-webpki-roots"]` to the `tungstenite` dependency — this pulls in
      `rustls`/`webpki-roots`.

- [ ] **Decide on `hello.lps.hash`.** Currently omitted (the field is optional on the wire). If an
      adapter is expected to verify it, add `sha2` to `[workspace.dependencies]` and compute it in
      `mbt/src/main.rs`.

- [ ] **Confirm the tau-closure depth accumulation behaviour with the spec owner.** Implemented
      literally per the spec: `ModelState` stores `current` already tau-closed, and every accepted
      observation re-closes it, so the effective depth grows by `k` per observation
      (`τ*ₖ(τ*ₖ(S)) = τ*₂ₖ(S)`). Worth flagging before it surprises someone.

- [ ] **Confirm the mixed-multi-action policy with the spec owner.**
      `ActionPartition::validate_against_lps` rejects any summand whose multi-action mixes input-
      and output-classified actions, at startup. If a real model needs `in|out` composites, IOCO
      itself needs extending first.

- [ ] **Measure single-threaded latency vs. heartbeats on a realistic model**, with `--timings`. A
      long tau-closure blocks the socket read and can push the tool past its own
      `heartbeat_interval_ms`, letting the adapter declare it lost. `--max-tau-closure-depth` and
      `--max-state-set-size` are partial mitigations already in place; this needs measuring on
      something bigger than the test fixtures before deciding whether more is needed.

- [ ] **Phase 7 — conditional partition rules** (`w -> a(w,v)` guards), currently parsed but
      rejected at load time.
      - [x] The three FFI functions this needs (`mcrl2_lps_data_specification`,
            `mcrl2_data_parse_variables`, `mcrl2_data_parse_data_expression`) are implemented and
            tested in a local `~/mCRL2-sys` checkout, but **not yet committed or upstreamed** to
            `MERCorg/mCRL2-sys`.
      - [ ] Push/PR those changes upstream, then bump `mcrl2-sys`'s pinned `rev` in
            [mlaveaux/merc's `tools/mcrl2/Cargo.toml`](https://github.com/mlaveaux/merc/blob/main/tools/mcrl2/Cargo.toml),
            and in turn bump *this* repo's `rev` pins (see README.md) once that lands.
      - [ ] Wrap the new FFI in `mlaveaux/merc`'s `crates/mcrl2/src/data.rs`/`lps.rs`.
      - [ ] Extend `ActionPattern` (in `crates/merc_mbt/src/partition.rs`) with
            `guard: Option<DataExpression>` and `binders: Vec<DataVariable>`, make rules
            order-sensitive (first match wins), and re-check homogeneity dynamically since static
            completeness can no longer assume one rule per action.

- [ ] *(optional)* Add a `cargo xtask test-tools`-style entry driving `merc-mbt` against a scripted
      mock adapter. No such pattern exists in `mlaveaux/merc`'s own `xtask` either, so there's
      nothing to mirror today — only worth doing if that changes, or if this repo grows enough
      tools to want one itself.
