//! `cargo fuzz run solversummary` – see fuzz/README.md and `teamcraft::fuzz_harness::solversummary`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, solversummary};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(solversummary, data));
