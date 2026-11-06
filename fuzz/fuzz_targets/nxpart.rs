//! `cargo fuzz run nxpart` – see fuzz/README.md and `teamcraft::fuzz_harness::nxpart`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, nxpart, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(nxpart, data));
