//! Pluggable, tool-specific structured result parsers (NER-136 §U5).
//!
//! These extract machine-readable **numeric** outcomes from the *full* captured
//! output (not the 4096-byte excerpt — a cargo summary line frequently sits past the
//! cap) so the Phase 4 check engine can evaluate a structured gate ("0 failing
//! tests") on parsed counts rather than a bare exit code. Dispatch is by
//! `(program, args)` identity, confined to this module so the gate engine never
//! names a tool. When no parser matches or parsing fails, the result is `None` and
//! the gate degrades to the exit-code verdict.
//!
//! **Scope (v0):** `cargo test` (libtest summary) and `cargo clippy` only. The
//! outcome is intentionally **numeric-only** — string fields (test names, file
//! paths) could carry secrets and would have to route through the redactor, so they
//! are out of scope. Selection by argv does NOT authenticate that the named tool
//! actually ran (a PATH-shimmed `cargo` or `cargo test --no-run` is trusted the same
//! way `exit_code` is) — see the plan's Scope Boundaries.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A numeric, tool-agnostic structured outcome. All fields optional so a parser only
/// populates what it measured (`cargo test` → passed/failed/ignored; `cargo clippy` →
/// findings). Persisted as `evidence.structured_json` and folded into the digest.
///
/// NOTE: only `failed` is consumed by the Phase 4 gate engine today (the "0 failing
/// tests" structured gate reads `failed`); `passed`/`ignored`/`findings` are persisted
/// (and hashed, so tamper-evident) for future gates/compare-rank but have no live
/// reader yet. A `--require-tests-pass "cargo clippy"` gate therefore resolves to
/// `missing` (clippy emits no `failed` count) — clippy structured gates are not v0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StructuredOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<u64>,
}

/// Dispatch to a tool-specific parser by `(program, args)` identity. Returns `None`
/// when no parser matches or parsing fails (the gate then degrades to exit code).
pub fn parse_structured(
    program: &str,
    args: &[String],
    stdout: &str,
    stderr: &str,
) -> Option<StructuredOutcome> {
    let prog = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if prog == "cargo" {
        match args.first().map(String::as_str) {
            Some("test") => return parse_cargo_test(stdout, stderr),
            Some("clippy") => return parse_cargo_clippy(stdout, stderr),
            _ => {}
        }
    }
    None
}

fn cargo_test_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // libtest summary, one per test binary: sum across all of them.
        Regex::new(r"test result:\s+\w+\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored")
            .expect("valid cargo-test summary regex")
    })
}

fn parse_cargo_test(stdout: &str, stderr: &str) -> Option<StructuredOutcome> {
    let re = cargo_test_regex();
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut ignored = 0u64;
    let mut found = false;
    for haystack in [stdout, stderr] {
        for caps in re.captures_iter(haystack) {
            found = true;
            passed += caps[1].parse::<u64>().unwrap_or(0);
            failed += caps[2].parse::<u64>().unwrap_or(0);
            ignored += caps[3].parse::<u64>().unwrap_or(0);
        }
    }
    // No summary line at all (a compile error before tests ran, a timeout, or a
    // format this parser doesn't recognize) degrades to None — never a spoofable
    // `failed: 0`.
    found.then_some(StructuredOutcome {
        passed: Some(passed),
        failed: Some(failed),
        ignored: Some(ignored),
        findings: None,
    })
}

fn clippy_summary_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"generated (\d+) warnings?").expect("valid clippy summary regex"))
}

fn parse_cargo_clippy(stdout: &str, stderr: &str) -> Option<StructuredOutcome> {
    let re = clippy_summary_regex();
    let mut findings = 0u64;
    let mut found = false;
    for haystack in [stdout, stderr] {
        for caps in re.captures_iter(haystack) {
            found = true;
            findings += caps[1].parse::<u64>().unwrap_or(0);
        }
    }
    if found {
        return Some(StructuredOutcome {
            findings: Some(findings),
            ..Default::default()
        });
    }
    // A clean clippy run emits no warnings summary but does Finish; treat that as 0
    // findings. Anything that did not Finish (compile error, timeout) degrades.
    if stdout.contains("Finished") || stderr.contains("Finished") {
        return Some(StructuredOutcome {
            findings: Some(0),
            ..Default::default()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn parses_passing_cargo_test_summary() {
        let out = "running 12 tests\n......\ntest result: ok. 12 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.42s";
        let outcome = parse_structured("cargo", &args(&["test"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(12));
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, Some(1));
    }

    #[test]
    fn parses_failing_cargo_test_summary() {
        let out = "test result: FAILED. 10 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s";
        let outcome = parse_structured("cargo", &args(&["test"]), out, "").expect("parsed");
        assert_eq!(outcome.failed, Some(2));
    }

    #[test]
    fn sums_across_multiple_test_binaries() {
        let out = "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n\
                   test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.2s";
        let outcome = parse_structured("cargo", &args(&["test"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(8));
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn compile_error_before_tests_degrades_to_none() {
        let stderr =
            "error[E0432]: unresolved import\nerror: could not compile `forge` due to 1 error";
        assert!(parse_structured("cargo", &args(&["test"]), "", stderr).is_none());
    }

    #[test]
    fn zero_tests_is_distinct_from_none() {
        let out =
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.0s";
        let outcome = parse_structured("cargo", &args(&["test"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(0));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn unknown_tool_degrades_to_none() {
        assert!(parse_structured("./my_script", &args(&["--run"]), "FAILED 3 tests", "").is_none());
        assert!(parse_structured("cargo", &args(&["build"]), "Finished", "").is_none());
    }

    #[test]
    fn parses_clippy_findings_and_clean() {
        let with = parse_structured(
            "cargo",
            &args(&["clippy"]),
            "",
            "warning: `forge-store` (lib) generated 3 warnings",
        )
        .expect("parsed");
        assert_eq!(with.findings, Some(3));
        let clean = parse_structured("cargo", &args(&["clippy"]), "", "Finished in 1.0s")
            .expect("clean clippy parses");
        assert_eq!(clean.findings, Some(0));
    }
}
