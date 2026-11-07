//! `cargo fuzz run mesh` – see fuzz/README.md and `teamcraft::fuzz_harness::mesh`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, mesh, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(mesh, data));
