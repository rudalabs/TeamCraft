//! `cargo fuzz run stepbom` – see fuzz/README.md and `teamcraft::fuzz_harness::stepbom`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, stepbom};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(stepbom, data));
