# Vendored third-party code

Code of other projects kept in this repository because it is not available
from crates.io in the form TeamCraft needs. Every folder keeps its own licence
file; nothing here is under TeamCraft's MPL-2.0.

## `openskp/` – SketchUp file reader

* **What:** the pure-Rust port of OpenSKP (crate `openskp` 1.5.0, no
  dependencies): a reader and writer of SketchUp `.skp` files.
* **Origin:** <https://github.com/iamahsanmehmood/openskp>, folder
  `packages/rust` (`Cargo.toml`, `LICENSE`, `src/`) at commit
  `e7b467452be72b105ca4f7a96559dd2d342cbb47` (2026-11-07). The files are
  **unchanged** copies; the tests and examples of that folder are left out.
* **Licence:** MIT, Copyright (c) 2024-2026 Ahsan Mehmood – see
  [`openskp/LICENSE`](openskp/LICENSE).
* **Used for:** the 3D preview of SketchUp files of version 3 to 2020 (the
  classic MFC container), see `src/mesh/sketchup.rs` and
  `docs/src/research/sketchup-preview.md`. Files of SketchUp 2021 and later
  are read by TeamCraft's own reader.
* **Not to be confused with** the crates.io package `openskp` 0.5.0
  (`hew3d/openskp`, GPL-3.0), which TeamCraft does not use.
* **Updating:** replace `openskp/src`, `openskp/Cargo.toml` and
  `openskp/LICENSE` with the files of a newer commit, update the commit
  above and the version in TeamCraft's `Cargo.toml`, then run
  `cargo test --test cad_fixtures`.
