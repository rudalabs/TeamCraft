# Mutation testing

Line coverage ([coverage map](coverage.md)) says which code the tests run.
Mutation testing says which behaviour they **check**: cargo-mutants makes a
small change to the code – a `>=` becomes `<`, a function returns `""`, a `!`
disappears – rebuilds and runs the unit tests. If no test fails, the change
went unnoticed: the tests restate the code instead of pinning it down.
Background, licence and the decision: [research note](../research/mutation-testing.md).

## What is examined

`.cargo/mutants.toml` lists the files (`examine_globs`): the pure modules
whose rules the unit tests are meant to pin down – `src/domain/**`,
`httprange`, `pagecontext`, `util`, `htmltext`, `resultscan`, `bomtable`,
`cadformat`, `cadrefs` and `stepbom`. Only `cargo test --lib` runs per
mutant: no PostgreSQL, no integration tests in `tests/`. A rule that only an
integration test checks therefore shows up as *missed* here – the fix is a
unit test next to the rule, which is the point.

Services, handlers, templates and the 3D readers are not examined yet
(*What to cover next* in `docs/OPEN_WORK.md`).

## Running it locally

```sh
cargo install cargo-mutants --locked     # the CI version: CARGO_MUTANTS_VERSION in .github/workflows/mutants.yml

cargo mutants --list | wc -l             # what the configuration selects
cargo mutants -F '^src/util\.rs:'        # one module (a regex on the mutant name)
cargo mutants -F 'format_bytes'          # one function
git diff origin/main... > my.diff && cargo mutants --in-diff my.diff   # what the pull request job runs
```

`-f <file>` does **not** narrow the run: cargo-mutants 27.1 adds the file to
the `examine_globs` of the configuration. Use `-F` with a regex instead. The
eleven field-deletion mutants of `resultscan::TemplateInput::validated`
ignore `-F` and `-E` (a tool quirk) and run along – about five minutes more.

Each mutant is an incremental rebuild of the lib test target (profile
`mutants`, no debug info, `--lib` also for the build step) plus the unit
tests. Measured on a 12-thread machine with `-j 1`: 8–10 s build + 18 s
tests, **about 30 s per mutant** – `httprange` (13 mutants) in 10 minutes
with the baseline, `util` (40) in about 25. The whole configuration (~1 900
mutants, ~16 h at `-j 1`) is a job for the weekly CI run, not a laptop.
The lib tests are more than half of each mutant's time, so a slow unit test
costs twice here.

The results land in `target/mutants.out/` (`output` in the configuration):

| File | Meaning |
|---|---|
| `caught.txt` | a test failed – good |
| `missed.txt` | every test passed with the change – a gap in the tests, or an equivalent mutant |
| `timeout.txt` | the tests hung (a loop bound depends on the changed code); counts as caught but is slow |
| `unviable.txt` | the change did not compile – no information |
| `outcomes.json` | everything, machine-readable; `log/` has the build and test output per mutant |

`cargo mutants --iterate` reruns only what was missed or new since the last
run in the same output directory – the quick loop while writing the tests.

## Reading a missed mutant

```text
src/util.rs:84:35: replace < with <= in format_bytes
```

File, line and column, then the change. Ask what wrong behaviour the change
would cause and write the test that shows it – here a value of a few PiB
that must stay in TiB instead of indexing past the unit table. Do not test
the mutation ("`<` is not `<=`"); test the behaviour a user or caller would
see.

Some mutants cannot be caught because they do not change behaviour
(*equivalent mutants*): `x >= 0` on an unsigned value, a capacity hint, a
cache that only saves time. Leave them missed and say so in the pull request,
or – when the same pattern keeps coming back – add an `exclude_re` to
`.cargo/mutants.toml` **with a comment** saying why. TeamCraft does not use
`#[mutants::skip]`: that would add the `mutants` crate as a dependency, and
the exclusions belong in one reviewed place.

## In CI

Workflow *Mutation testing* (`.github/workflows/mutants.yml`):

* **Pull requests** that touch `src/**`: `cargo mutants --in-diff` over the
  pull request's diff – only mutants in changed lines of examined files. No
  such lines, no run. **Report-only for now**: missed mutants appear as
  warnings on the changed lines, in the job summary and in the artifact
  *mutants-pr*; the check stays green. A broken run (the unmutated tests
  fail, a diff that does not match, a tool error) fails the job.
* **Weekly and by hand** (Actions → Mutation testing → Run workflow): every
  examined file, split over 16 shards (`--shard k/16`), each shard with its
  own baseline, at most 8 shards at a time (a single run at a time, weekly or
  manual); the *mutants report* job merges the shards' `missed.txt` into
  one summary, counted per file.

## Policy

* A missed mutant in code your pull request touches should be fixed in the
  same pull request: a unit test that checks the behaviour, or a sentence in
  the description why the mutant is equivalent.
* A new pure module with unit tests joins `examine_globs`.
* When the weekly run of the examined modules is clean (missed only where
  documented), the pull request job becomes blocking.
