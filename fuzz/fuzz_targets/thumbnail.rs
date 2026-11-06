//! `cargo fuzz run thumbnail` – see fuzz/README.md and `teamcraft::fuzz_harness::thumbnail`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, thumbnail};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(thumbnail, data));
