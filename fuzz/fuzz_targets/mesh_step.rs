//! `cargo fuzz run mesh_step` – see fuzz/README.md and `teamcraft::fuzz_harness::mesh_step`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, mesh_step, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(mesh_step, data));
