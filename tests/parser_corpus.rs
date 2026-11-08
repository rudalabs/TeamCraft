//! The fuzz corpus on stable `cargo test`: every seed in `fuzz/seeds/<target>`
//! and every NIST fixture of a type the target reads goes through the same
//! entry point as `cargo fuzz` (`teamcraft::fuzz_harness`) – whole, cut at a
//! few lengths and with a few bytes corrupted (deterministic). No reader may
//! panic through to the harness. `tests/parser_robustness.rs` does the same
//! byte by byte with small hand-made samples; this test adds the real files
//! and keeps the seeds of the fuzz targets valid input. No database needed.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use teamcraft::fuzz_harness::TARGETS;

/// Fixture types (lower-case extensions) each target reads; `None` = all.
fn fixture_types(target: &str) -> Option<&'static [&'static str]> {
    Some(match target {
        "stepbom" | "textindex" => &["stp", "step", "txt", "tsv"],
        "htmltext" => &["html", "htm", "txt", "tsv"],
        "swpart" => &["sldprt", "sldasm", "slddrw"],
        "nxpart" => &["prt"],
        "invpart" => &["ipt", "iam"],
        "thumbnail" => &[
            "ipt", "iam", "sldprt", "sldasm", "prt", "catpart", "par", "asm",
        ],
        "bomtable" => &["tsv", "csv"],
        "resultscan" | "solversummary" => &["csv", "tsv", "txt"],
        // The STEP tessellation is slow in a debug build: only small files.
        "mesh_step" => &["stp"],
        "mesh" => &[],
        _ => return None,
    })
}

/// Largest fixture the slow STEP tessellation gets here (the fuzz job
/// covers the large ones).
const MESH_STEP_MAX: u64 = 64 * 1024;

fn files_in(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_in(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The inputs of a target: its seeds, then the matching fixtures.
fn inputs(target: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut seeds = Vec::new();
    files_in(&root.join("fuzz/seeds").join(target), &mut seeds);
    let mut fixtures = Vec::new();
    files_in(&root.join("tests/fixtures/cad"), &mut fixtures);
    files_in(&root.join("tests/fixtures/reports"), &mut fixtures);
    fixtures.retain(|p| {
        let small = p.metadata().map(|m| m.len()).unwrap_or(0) <= MESH_STEP_MAX;
        match fixture_types(target) {
            None => !matches!(extension(p).as_str(), "md" | "txt" | "tsv"),
            Some(types) => {
                types.contains(&extension(p).as_str()) && (target != "mesh_step" || small)
            }
        }
    });
    seeds.sort();
    fixtures.sort();
    seeds.extend(fixtures);
    seeds
}

/// xorshift64: the same variants on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// The input whole, cut at a few lengths, and a few copies with bytes
/// replaced – near the start (where the headers are) and anywhere.
fn variants(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let len = data.len();
    let mut out = vec![("whole".to_string(), data.to_vec())];
    let mut cuts = vec![0, 1, 64, 4096, len / 7, len / 2, len.saturating_sub(1)];
    cuts.retain(|&n| n < len);
    cuts.sort_unstable();
    cuts.dedup();
    for n in cuts {
        out.push((format!("first {n} bytes"), data[..n].to_vec()));
    }
    if len == 0 {
        return out;
    }
    let mut rng = Rng(0x2545_F491_4F6C_DD1D ^ len as u64);
    const SPECIAL: [u8; 6] = [0x00, 0xFF, 0x7F, 0x80, b'(', b'9'];
    let flips = if len > 1024 * 1024 { 3 } else { 8 };
    for round in 0..flips {
        let mut copy = data.to_vec();
        let window = if round % 2 == 0 { len.min(4096) } else { len };
        let mut changed = Vec::new();
        for _ in 0..1 + rng.next() % 4 {
            let at = (rng.next() % window as u64) as usize;
            copy[at] = SPECIAL[(rng.next() % SPECIAL.len() as u64) as usize];
            changed.push(at);
        }
        out.push((format!("bytes {changed:?} replaced"), copy));
    }
    out
}

#[test]
fn the_corpus_never_panics_a_reader() {
    // A reader's own caught panics (STEP kernel, gltf-json) are fine; only
    // a panic that reaches the harness fails – reported with its input.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let mut runs = 0usize;
    for (name, target) in TARGETS {
        let inputs = inputs(name);
        assert!(
            !inputs.is_empty(),
            "{name}: no seeds in fuzz/seeds/{name} and no fixtures"
        );
        for path in inputs {
            let data = std::fs::read(&path).unwrap();
            for (variant, bytes) in variants(&data) {
                runs += 1;
                if panic::catch_unwind(AssertUnwindSafe(|| target(&bytes))).is_err() {
                    failures.push(format!("{name}: {} ({variant})", path.display()));
                }
            }
        }
    }
    panic::set_hook(hook);
    assert!(
        failures.is_empty(),
        "{} of {runs} runs panicked:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_target_has_seeds_or_fixtures_of_its_own() {
    for (name, _) in TARGETS {
        let n = inputs(name).len();
        assert!(n >= 1, "{name}: {n} inputs");
    }
    // The containers are fed with real files.
    for (target, ext) in [
        ("swpart", "sldasm"),
        ("nxpart", "prt"),
        ("invpart", "ipt"),
        ("thumbnail", "ipt"),
    ] {
        assert!(
            inputs(target).iter().any(|p| extension(p) == ext),
            "{target}: no .{ext} fixture"
        );
    }
}
