//! `cargo fuzz run simmesh` – see fuzz/README.md and `teamcraft::fuzz_harness::simmesh`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, simmesh};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(simmesh, data));
