//! `cargo fuzz run textindex` – see fuzz/README.md and `teamcraft::fuzz_harness::textindex`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, quiet_panics, textindex};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(textindex, data));
