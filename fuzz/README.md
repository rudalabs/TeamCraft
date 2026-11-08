# Fuzz targets

`cargo fuzz` targets for every reader of untrusted files – one file per target
in `fuzz_targets/`, all calling the entry points in `src/fuzz_harness.rs`.
Seeds are in `seeds/<target>/`, the NIST CAD files in `tests/fixtures/cad`
are added by the workflow.

How to run a target, what the weekly *Fuzzing* workflow does and what to do
with a crash: [docs/src/dev/fuzzing.md](../docs/src/dev/fuzzing.md).

This crate has its own workspace and needs nightly Rust; the server build
never compiles it.
