//! `cargo fuzz run invpart` – see fuzz/README.md and `teamcraft::fuzz_harness::invpart`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, invpart};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(invpart, data));
