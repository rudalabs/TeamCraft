# Fuzzing the file readers

Every reader of untrusted files has a fuzz target. Users upload CAD files,
BOM tables and simulation results, and TeamCraft opens them on the server (previews, references, text index, BOM
import). The readers must return an
error or a valid result for any bytes. They must never panic, hang or
allocate what a file only claims.

## The pieces

| Piece                        | Where                                                                                                                                              | Runs                                                                                               |
|------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| Entry points, one per reader | `src/fuzz_harness.rs` (`TARGETS`)                                                                                                                  | shared by both below                                                                               |
| `cargo fuzz` targets         | `fuzz/` (own crate and workspace, never part of the server build)                                                                                  | weekly and on demand: *Fuzzing* workflow (`.github/workflows/fuzz.yml`), nightly Rust, Linux       |
| Seeds                        | `fuzz/seeds/<target>/` (small text samples), the NIST CAD files in `tests/fixtures/cad` and the synthetic HTML reports in `tests/fixtures/reports` | both                                                                                               |
| Corpus test                  | `tests/parser_corpus.rs`                                                                                                                           | every `cargo test` (stable): seeds and matching fixtures whole, cut and with a few bytes corrupted |
| Damage test                  | `tests/parser_robustness.rs`                                                                                                                       | every `cargo test`: small hand-made samples, every prefix and hundreds of corruptions              |

The targets are `stepbom`, `swpart`, `nxpart`, `cadrefs`, `bomtable`,
`thumbnail`, `textindex`, `htmltext` (HTML reports and the charset detection of text files), `mesh` (STL, OBJ, PLY, OFF,
3MF, AMF, glTF, 3DS,
points, LAS, VTK legacy and XML, CalculiX FRD) and `mesh_step` (the STEP
tessellation through `truck`, kept apart because it is slow), `cadformat` (the
format detection over a file's first 1024 bytes) and `resultscan` (the
extraction templates and the exchange file) and `solversummary` (the solver
summary of Fluent, Nastran and STAR-CCM+ output).

Two readers catch panics of a library on purpose: the STEP kernel in
`mesh::step` (its preview conversion also runs in a child process with a
time limit) and `gltf-json` in `mesh::gltf`. libFuzzer's own panic hook
would abort on those too. The targets therefore run through
`fuzz_harness::guarded` with a hook that only prints. A panic counts as a
crash only when it reaches the harness.

## No panics by construction

Every reader of untrusted files denies `clippy::unwrap_used`, `expect_used`,
`indexing_slicing`, `panic` and `unreachable` with `#![deny(...)]` at the
top of the module. These are `swpart`, `mesh` with its submodules,
`simmesh`, `stepbom`, `nxpart`, `thumbnail`, `cadrefs`, `bomtable`,
`textindex`, `htmltext`, `resultscan` and `cadformat`, and `config` too. Clippy in CI therefore
refuses a new `data[at]`, `data[a..b]` or `.unwrap()` there. Read through the
bounded helpers in `mesh` (`bytes_at`, `u16_le`, `u32_le`, `f32_le`, `range`,
`quads`) or `get`, slice patterns (`let [x, y, z] = *v.as_slice() else { … }`),
`windows` and `zip`. A file that is shorter than it claims then becomes an
error, not a panic. Tests may still unwrap and index (`clippy.toml`). An
exception needs `#[allow(clippy::indexing_slicing)]` on the smallest item,
with a comment why no file can reach it (example: `mesh::compose`,
fixed-size arrays).

The lint does not see every cut of a `&str`. Cut strings with `get` as well,
and never at a byte position computed on another string. A lower-cased copy
can be longer: the Kelvin sign `K` is three bytes, its lower case `k` is one.

## Running a target locally

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
cargo +nightly fuzz run stepbom fuzz/corpus/stepbom fuzz/seeds/stepbom tests/fixtures/cad/nist -- -max_total_time=60
```

The first corpus directory collects new inputs. It is git-ignored, like
`fuzz/artifacts` and `fuzz/target`. `cargo fuzz` needs a sanitizer-capable
nightly toolchain: Linux and macOS work. On Windows run the corpus test
instead (`cargo test --test parser_corpus`). The fuzz workflow does the rest.

The workflow pins `--target x86_64-unknown-linux-gnu`. The prebuilt
`cargo-fuzz` it installs is a musl binary, and `cargo fuzz` otherwise defaults
the target to its own build triple (musl), which is not installed on the
runner and whose static libc the AddressSanitizer refuses. A local
`cargo install cargo-fuzz` builds a host (GNU) binary, so local runs need no
flag; only CI does.

## When a target crashes

1. Download the artifact *fuzz-crash-&lt;target&gt;* of the failed run (or take
   the file from `fuzz/artifacts/<target>/`).
2. Minimise it: `cargo +nightly fuzz tmin <target> <crash file>`.
3. Add the minimised bytes as a regression unit test next to the reader (as
   a byte string literal if it is small). The test checks that the reader
   returns an error or a sensible result. Then fix the reader.
4. Put the input into `fuzz/seeds/<target>/` as well when it is small and
   says something about the format.
