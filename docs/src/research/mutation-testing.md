# Mutation testing with cargo-mutants

**Question:** line coverage says which code the tests *run*, not which
behaviour they *check*. A test that calls a function and asserts nothing, or
asserts what the code happens to return today, keeps the coverage figure high
and catches nothing. How does TeamCraft find such tests, at a cost the CI and a
single developer machine can carry? Investigated on 2026-11-07.

## What mutation testing does

A mutation tester makes small, deliberate changes to the source (a *mutant*), builds it and runs the tests. If a test
fails, the mutant is **caught**: some test depends on that piece of behaviour. If every test still
passes, the mutant is **missed**: the code could be wrong in exactly that way
and nobody would notice. A missed mutant is either a gap in the tests or an *equivalent* mutant (a change that does not
alter observable behaviour, e.g.
`>=` → `>` where the equal case cannot occur).

[cargo-mutants](https://mutants.rs) (Martin Pool, source at
[github.com/sourcefrog/cargo-mutants](https://github.com/sourcefrog/cargo-mutants))
is the maintained tool for Rust. It needs no compiler plugin or nightly
toolchain: it rewrites the source text in a scratch copy of the tree (or in
place), then runs plain `cargo build` / `cargo test`. Mutations include:

* replacing a function body with a value of its return type (`Default::default()`,
  `0`, `1`, `true`/`false`, `String::new()`, `"xyzzy".into()`, `Ok(Default::default())`,
  `None`, `vec![]`, …);
* swapping binary operators (`==` ↔ `!=`, `<` → `<=`/`>`/`==`, `&&` ↔ `||`,
  `+` → `-`/`*`, bit operators) and deleting unary `!` / `-`;
* deleting match arms (when a wildcard arm exists) and struct-literal fields
  that have a default.

Outcomes per mutant: *caught*, *missed*, *unviable* (the mutant does not
compile – no information, cheap because only the build runs) and *timeout*
(the mutant made a test hang – usually counts as caught, but shows a loop
whose bound matters).

## Licence

**MIT** (repository `LICENSE`, crate metadata). It is a developer tool, not a
dependency of TeamCraft: nothing is linked into the binary and `deny.toml`
does not see it. Installing it with `cargo install cargo-mutants --locked`
or the prebuilt binaries of `taiki-e/install-action` is the same as for
`cargo-fuzz` and `cargo-llvm-cov`.

The optional `mutants` crate (attribute `#[mutants::skip]`, also MIT) would be
a new regular dependency of the server. TeamCraft does **not** add it:
exclusions go into `.cargo/mutants.toml` (`exclude_re` on the mutant name,
`exclude_globs` on files) where they are reviewed in one place.

## Other tools

| Tool                                            | Notes                                                                                                                                                  |
|-------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| [`mutagen`](https://github.com/llogiq/mutagen)  | compiles all mutations into the binary behind a switch (fast), needs nightly and an attribute on every function; unmaintained since 2021               |
| [`cargo-mutants`](https://mutants.rs)           | source rewriting, stable toolchain, no code changes, `--in-diff` and sharding for CI; actively maintained (27.0.0 on crates.io at the time of writing) |
| line coverage (`cargo llvm-cov`, already in CI) | complementary: cheap and broad, but blind to missing assertions                                                                                        |
| fuzzing (`cargo fuzz`, already weekly)          | complementary: finds panics and hangs on hostile input, not missing assertions on correct input                                                        |

## How it fits TeamCraft

TeamCraft is one big library crate (`src/`, about 87 000 lines in 168 files) with three
kinds of tests:

* **unit tests** in the modules (`cargo test --lib`, no database, seconds);
* **integration tests** in `tests/` that need PostgreSQL (CI has a service
  container; locally `docker compose up -d db`) and take minutes;
* **desktop tests** in their own Cargo project.

Every mutant costs one incremental rebuild of the test target plus the test
run. With the full suite (minutes, PostgreSQL) per mutant a run over the whole
crate would take days. The pure modules – `domain`, the parsers and readers (`httprange`, `pagecontext`, `htmltext`,
`resultscan`, `bomtable`,
`cadformat`, …) – are exactly where the rules live that unit tests are meant
to pin down, and their unit tests run in seconds. TeamCraft therefore starts
with:

* **`--lib` only** (`additional_cargo_args = ["--lib"]`): no PostgreSQL, no
  integration tests, no service container in the workflow. It has to be a *cargo* argument, not a test argument
  (`additional_cargo_test_args`): the
  build step is `cargo test --no-run`, which otherwise compiles all ~70 test
  binaries for every mutant – the first trial took 2.5 minutes per mutant
  that way, against 30 s with `--lib` on both steps;
* **`examine_globs`** listing those pure modules; services, handlers and
  templates stay out until a second phase decides whether DB tests are worth
  the runtime (they would need the PostgreSQL service and one database per
  test run, which `tests/common` already creates);
* a **`mutants` build profile** (`[profile.mutants]`, inherits `test`, no
  debug info), as the cargo-mutants book suggests: less to write and link for
  every mutant.

### Runtime

Measured on the developer machine (12 threads, 15 GB, `-j 1`): baseline
23 s incremental build + 19 s for the lib tests; then **8–10 s build + 18 s
tests per mutant, about 30 s**. The lib tests are more than half of it. The
whole configuration selects ~1 900 mutants: ~16 hours on one machine. On a
4-core GitHub runner a mutant takes longer; 16 shards of ~120 mutants each
stay within a few hours per job. Details per module: [first runs](#first-runs).

### Pull requests: `--in-diff`

`cargo mutants --in-diff git.diff` only tests mutants whose span overlaps a
changed hunk of the diff (`git diff origin/<base>...HEAD`). Together with
`examine_globs` a pull request that does not touch a pure module tests
nothing and finishes after the baseline. Pitfalls found in other projects'
workflows: the diff must have the `a/`/`b/` prefixes of `git diff` (no
`--no-prefix`), the checkout needs the base branch (`fetch-depth: 0`), and an
empty selection is not an error. Exit code 5 means the diff does not match
the tree.

### Full runs: sharding

`--shard k/n` splits the mutant list deterministically; every shard must use
the same arguments and the same `n`. The book recommends at least ~10
mutants per shard and 8–32 shards. `--baseline=skip` saves the unmutated test
run per shard but then needs an explicit `--timeout` and a job that proved
the baseline green (the CI workflow already does). `--in-place` mutates the
checkout itself instead of copying it, which is fine on a throw-away runner
and incompatible with `-j`.

### Timeouts

The default test timeout is 5 × the baseline test time, at least 20 s;
with a skipped baseline or `--in-place` it is 300 s. A build timeout is off by
default (`build_timeout_multiplier`). Every workflow job also gets a
`timeout-minutes`, as in the other workflows.

### Exclusions

* `exclude_re` on the mutant's name (`src/x.rs:12:5: replace f -> bool with true`),
  e.g. `impl Display` / `fmt` bodies and `Debug` output, which no test should
  pin down;
* `exclude_globs` for generated or trivial files.

Every exclusion carries a comment in `.cargo/mutants.toml` saying why the
mutants are equivalent or not worth a test.

### Exit codes

0 all caught, 2 missed mutants, 3 timeouts, 4 the unmutated tests already
fail, 5/6 a bad `--in-diff` file, 1 usage, 70 internal. The pull request job
is report-only at first (`continue-on-error`), so code 2 shows up as a warning
and in the step summary, not as a red check.

## How other projects run it

* **cargo-mutants itself** – sharded full runs on every push (`.github/workflows/tests.yml`), `--in-diff` on pull
  requests, artifacts
  `mutants.out`.
* **[vodozemac](https://github.com/matrix-org/vodozemac)** (Matrix
  cryptography) – `--in-diff` on pull requests to `main`, path filter on Rust
  sources, `--in-place`.
* **ebman** – PR diff limited to `src/**/*.rs`, `-- --lib`, 120 s timeout,
  missed mutants block the merge.
* **safe-chains** – guards against the silent empty selection: checks the
  diff prefixes and counts the selected mutants with `--list --in-diff`
  before the run.

## Decision

* **cargo-mutants** (MIT) as a CI tool, no new crate dependency.
* `.cargo/mutants.toml`: `examine_globs` over the pure, unit-tested modules,
  `--lib` tests, `mutants` profile, empty `exclude_re` (rules are added when a report shows one is needed).
* Workflow `mutants.yml`: pull requests run `--in-diff` over the touched
  examined files, **report-only** (missed mutants in the step summary and the
  artifact); a weekly and manual full run sharded over a matrix. No PostgreSQL
  service while only `--lib` tests run.
* Policy: a missed mutant in code a pull request touches should be fixed with
  a test that checks the behaviour, or listed as equivalent with a reason.
  When the examined modules are clean, the pull request job becomes blocking
  and the next modules join (`docs/OPEN_WORK.md`).

## First runs

cargo-mutants 27.1.0, local, `-j 1`, `--lib`, 2026-11-07:

| Module                                                            | Mutants | Caught  | Missed | Unviable | Time                                  | Gap found                                                                                                                                                                                                                               |
|-------------------------------------------------------------------|---------|---------|--------|----------|---------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `httprange`                                                       | 13      | 12      | 0      | 1        | 10 min (with the field mutants below) | none – its tests already pin every boundary                                                                                                                                                                                             |
| `util`                                                            | 40      | 37 → 40 | 3 → 0  | 0        | 22 min                                | `format_bytes` was never tested beyond MiB: making the unit loop run one step further (`<=`, `len() + 1`, `len() / 1`) went unnoticed and would index past the unit table (a panic for ≥ 1 PiB); test `byte_sizes_stop_at_tib`          |
| `resultscan` (the 11 field mutants of `TemplateInput::validated`) | 11      | 0 → 7   | 7 → 0  | 4        | with the above                        | no test checked what `validated` *returns*: dropping the trimmed name, file pattern, method, pick, unit, load case or solver from the result (so `..self` stored the raw form input) passed; test `template_input_is_stored_normalised` |

The tool did what it promises on the first try: two modules with good line
coverage, ten missed mutants, two real gaps – a latent panic and an unnormalised
stored value. Quirks of 27.1 noted on the way: `--file` adds to `examine_globs`
instead of narrowing it (use `-F`), and field-deletion mutants ignore `-F`/`-E`.

## Sources

* cargo-mutants book: <https://mutants.rs> (configuration, CI, `--in-diff`,
  sharding, timeouts, exit codes)
* example workflow: `examples/workflows/in-diff.yml` in the cargo-mutants
  repository
* vodozemac, ebman, safe-chains: their `.github/workflows/mutants.yml`
