//! `cargo fuzz run resultscan` – see fuzz/README.md and `teamcraft::fuzz_harness::resultscan`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, resultscan};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(resultscan, data));
