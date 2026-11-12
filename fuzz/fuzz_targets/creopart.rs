//! `cargo fuzz run creopart` – see fuzz/README.md and `teamcraft::fuzz_harness::creopart`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{creopart, guarded, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(creopart, data));
