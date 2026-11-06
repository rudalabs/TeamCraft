//! `cargo fuzz run swpart` – see fuzz/README.md and `teamcraft::fuzz_harness::swpart`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, swpart};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(swpart, data));
