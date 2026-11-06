//! `cargo fuzz run pdftext` – see fuzz/README.md and `teamcraft::fuzz_harness::pdftext`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft::fuzz_harness::{guarded, pdftext, quiet_panics};

fuzz_target!(init: quiet_panics(), |data: &[u8]| guarded(pdftext, data));
