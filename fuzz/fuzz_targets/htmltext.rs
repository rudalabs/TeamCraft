//! `cargo fuzz run htmltext` – see fuzz/README.md and `teamcraft::fuzz_harness::htmltext`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, htmltext, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(htmltext, data));
