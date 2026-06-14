//! Conformance gate against the official YAML test suite (vendored under
//! `tests/yaml_test_suite/data`, see that directory's README for provenance).
//!
//! For each case we check parse success/failure: a case with an `error` marker
//! must be rejected; any other case must parse. This is a *ratcheting* gate:
//!
//!   * A non-skipped case that behaves wrongly fails the test (a regression).
//!   * A skipped (known-failing) case that starts behaving correctly ALSO fails
//!     the test, with a message to remove it from `KNOWN_FAILURES` — so the
//!     skip-list can only shrink, never silently hide a fix.
//!   * No case may panic, ever (panics are failures regardless of the skip-list).
//!
//! Current baseline: 359/402 (89.3%). The entries below are documented gaps to
//! be driven down by follow-on work (scalar edge cases, flow-context plain
//! scalars, indentation corner cases, etc.).

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// Cases we knowingly get wrong today (over-accept or under-reject). Names are
/// the case directory path relative to `data/`, using `/` separators.
const KNOWN_FAILURES: &[&str] = &[
    // -- over-accepted: malformed input we wrongly parse --
    "5LLU", "9C9N", "9KBC", "CXX2", "DK4H", "DK95/01", "G9HC", "H7J7", "QB6E", "S98Z", "VJP3/00",
    "W9L4", "Y79Y/000", "Y79Y/003", "Y79Y/004", "Y79Y/005", "ZXT5",
    // -- under-rejected: valid YAML we wrongly reject --
    "2EBW", "2SXE", "4WA9", "58MP", "5T43", "6CA3", "AB8U", "AZW3", "CT4Q", "D83L", "DBG4",
    "DK95/00", "F6MC", "FBC9", "KK5P", "LX3P", "M2N8/01", "M5C3", "M5DY", "Q5MG", "Q9WF", "S7BG",
    "UKK6/01", "V9D5", "W5VH", "Z67P",
];

fn data_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/yaml_test_suite/data")
}

/// Collects every case directory (one containing `in.yaml`), keyed by its path
/// relative to `data/` with `/` separators.
fn collect_cases(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    if dir.join("in.yaml").exists() {
        let name = dir
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        out.push((name, dir.to_path_buf()));
    }
    let mut children: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    children.sort();
    for child in children {
        collect_cases(&child, root, out);
    }
}

#[test]
fn yaml_test_suite_conformance() {
    let root = data_root();
    assert!(
        root.join("229Q").exists(),
        "vendored suite missing at {}",
        root.display()
    );

    let mut cases = Vec::new();
    collect_cases(&root, &root, &mut cases);
    cases.sort();
    assert!(
        cases.len() > 300,
        "expected the full suite, found {}",
        cases.len()
    );

    let known: std::collections::HashSet<&str> = KNOWN_FAILURES.iter().copied().collect();

    let mut regressions = Vec::new(); // not skipped, but wrong now
    let mut newly_passing = Vec::new(); // skipped, but correct now (ratchet)
    let mut panics = Vec::new();
    let mut correct = 0usize;

    for (name, dir) in &cases {
        let should_error = dir.join("error").exists();
        let input = match fs::read(dir.join("in.yaml")) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => continue, // encoding cases are out of scope for &str input
            },
            Err(_) => continue,
        };

        let parsed = catch_unwind(AssertUnwindSafe(|| yaml2::parse_documents(&input)));
        let panicked = parsed.is_err();
        if panicked {
            panics.push(name.clone());
        }
        let accepted = matches!(parsed, Ok(Ok(_)));
        let is_correct = if should_error { !accepted } else { accepted };

        let is_known = known.contains(name.as_str());
        if is_correct {
            correct += 1;
            if is_known {
                newly_passing.push(name.clone());
            }
        } else if !is_known {
            regressions.push(name.clone());
        }
    }

    eprintln!(
        "yaml-test-suite: {correct}/{} correct ({:.1}%), {} known gaps",
        cases.len(),
        100.0 * correct as f64 / cases.len() as f64,
        KNOWN_FAILURES.len(),
    );

    let mut failures = Vec::new();
    if !panics.is_empty() {
        failures.push(format!("PANICKED ({}): {}", panics.len(), panics.join(" ")));
    }
    if !regressions.is_empty() {
        failures.push(format!(
            "REGRESSED — newly wrong, add a fix or (last resort) to KNOWN_FAILURES ({}): {}",
            regressions.len(),
            regressions.join(" ")
        ));
    }
    if !newly_passing.is_empty() {
        failures.push(format!(
            "NOW PASSING — remove from KNOWN_FAILURES ({}): {}",
            newly_passing.len(),
            newly_passing.join(" ")
        ));
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
