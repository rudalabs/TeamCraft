//! `cargo fuzz run cadformat` – see fuzz/README.md and `teamcraft::fuzz_harness::cadformat`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, cadformat};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(cadformat, data));
