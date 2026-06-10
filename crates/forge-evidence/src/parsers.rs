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
//! **Scope (v0):** `cargo test` (libtest summary), `cargo clippy`, and
//! `python -m unittest` (the unittest `TextTestRunner` summary) only. `pytest` is
//! explicitly out of scope: its summary line has many variants (passed/failed/
//! skipped/xfailed/xpassed/errors, plugin output, ANSI color) and is not a trivial
//! single-regex parser — deferred to a follow-up (NER-258). A `python3` invocation
//! that does NOT name `-m unittest` (e.g. `python3 script.py`, `python3 -m mymodule`)
//! has no registered parser and degrades gracefully to `None` (exit-code verdict).
//!
//! The outcome is intentionally **numeric-only** — string fields (test names, file
//! paths) could carry secrets and would have to route through the redactor, so they
//! are out of scope. Selection by argv does NOT authenticate that the named tool
//! actually ran (a PATH-shimmed `cargo`/`python3` or `cargo test --no-run` is trusted
//! the same way `exit_code` is) — see the plan's Scope Boundaries.

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
    if is_python_interpreter(prog) && invokes_unittest(args) {
        return parse_python_unittest(stdout, stderr);
    }
    None
}

/// A python interpreter basename: `python`, `python3`, or `python3.<minor>`
/// (e.g. `python3.11`). Matched case-insensitively so a `PYTHON3` shim still routes.
fn is_python_interpreter(prog: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^python(3(\.\d+)?)?$").expect("valid python interpreter regex")
    });
    re.is_match(prog)
}

/// True when the argv invokes unittest via an adjacent `["-m", "unittest"]` pair.
/// `python3 -m mymodule` or `python3 script.py` do NOT match (no parser → `None`).
fn invokes_unittest(args: &[String]) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "-m" && pair[1] == "unittest")
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

fn unittest_ran_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // unittest's TextTestRunner footer, e.g. "Ran 5 tests in 0.001s".
    RE.get_or_init(|| Regex::new(r"^Ran (\d+) tests? in ").expect("valid unittest ran regex"))
}

fn unittest_ok_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "OK", "OK (skipped=2)", "OK (expected failures=1)", possibly several groups.
    RE.get_or_init(|| Regex::new(r"^OK(?: \(([^)]*)\))?\s*$").expect("valid unittest ok regex"))
}

fn unittest_failed_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "FAILED (failures=2)", "FAILED (failures=1, errors=1)", etc.
    RE.get_or_init(|| Regex::new(r"^FAILED \(([^)]*)\)").expect("valid unittest failed regex"))
}

/// Parse the named integer from a unittest paren body like
/// `failures=1, errors=2, skipped=3`. Returns 0 when the key is absent.
///
/// The KV regex anchors each key on a `,`/`(`/start boundary (not a bare `\w`
/// boundary) so a multi-word key like `unexpected successes=1` is matched by its
/// full phrase (`unexpected successes`) rather than collapsing to its trailing word
/// (`successes`) — see [`unittest_failed_count`], which queries `unexpected successes`
/// explicitly. Without the boundary anchor, `(\w+)=(\d+)` would match `successes=1`
/// and silently drop the count.
fn unittest_count(body: &str, key: &str) -> u64 {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Capture the full key phrase up to `=`: any run of non-`=`, non-`,` chars
    // (so it spans the space in `unexpected successes`), trimmed of surrounding
    // whitespace before comparison.
    let re = RE.get_or_init(|| Regex::new(r"([^=,]+)=(\d+)").expect("valid unittest kv regex"));
    for caps in re.captures_iter(body) {
        if caps[1].trim() == key {
            return caps[2].parse::<u64>().unwrap_or(0);
        }
    }
    0
}

/// The number of outcomes a `FAILED (…)` body reports as a genuine test failure:
/// `failures` (assertion failures), `errors` (non-assertion exceptions), and
/// `unexpected successes` (a `@unittest.expectedFailure` test that passed — a real
/// test-quality regression, and the reason unittest exits non-zero). `skipped`,
/// `expected failures` are deliberately excluded — they are not failures.
fn unittest_failed_count(body: &str) -> u64 {
    unittest_count(body, "failures")
        + unittest_count(body, "errors")
        + unittest_count(body, "unexpected successes")
}

/// Parse a `python -m unittest` run's structured outcome (NER-253).
///
/// CPython's `TextTestRunner` writes the `Ran N tests…` footer and the trailing
/// `OK`/`FAILED (…)` status to **STDERR** (not stdout), but we scan both streams
/// defensively in case a wrapper merges or redirects them — same contract as the
/// cargo parsers. Recognition is anchored on the `Ran N tests` footer: we only look
/// for the status line *after* that footer (within a small window), so a test fixture
/// whose own output happens to contain the word `OK` cannot trip a false positive. If
/// the footer is absent (a crash/import error before the runner finished, or a format
/// we don't recognize) we return `None` and the gate degrades to the exit code — never
/// a spoofable `failed: 0`. A unittest *error* (a non-assertion exception) counts as a
/// failure, so `failed = failures + errors`.
fn parse_python_unittest(stdout: &str, stderr: &str) -> Option<StructuredOutcome> {
    for haystack in [stderr, stdout] {
        if let Some(outcome) = parse_unittest_stream(haystack) {
            return Some(outcome);
        }
    }
    None
}

fn parse_unittest_stream(text: &str) -> Option<StructuredOutcome> {
    let lines: Vec<&str> = text.lines().collect();
    // Anchor on the *last* "Ran N tests" footer; the status line is the next non-blank
    // line after it (unittest separates them by a single blank line). We use the LAST
    // footer, not the first: when a test spawns a child `python -m unittest` whose
    // stderr is inherited (a common meta-/integration-test pattern), the child writes
    // its own "Ran N tests … FAILED/OK" footer to the shared stderr *before* the real
    // runner appends its footer last. Anchoring on the first footer would read the
    // child's verdict and falsely block a green suite (NER-253 code review). Searching
    // only AFTER the chosen footer also keeps generic "OK"/"FAILED" text earlier in the
    // output from false-matching.
    let ran_re = unittest_ran_regex();
    let ran_idx = lines
        .iter()
        .rposition(|line| ran_re.is_match(line.trim_end()))?;
    let total: u64 = ran_re.captures(lines[ran_idx].trim_end())?[1]
        .parse()
        .unwrap_or(0);

    // Status line within a small window after the footer (blank line, then OK/FAILED).
    let ok_re = unittest_ok_regex();
    let failed_re = unittest_failed_regex();
    let end = (ran_idx + 6).min(lines.len());
    for line in &lines[ran_idx + 1..end] {
        let line = line.trim_end();
        if let Some(caps) = ok_re.captures(line) {
            let skipped = caps
                .get(1)
                .map(|body| unittest_count(body.as_str(), "skipped"))
                .unwrap_or(0);
            return Some(StructuredOutcome {
                failed: Some(0),
                passed: Some(total.saturating_sub(skipped)),
                ignored: (skipped > 0).then_some(skipped),
                findings: None,
            });
        }
        if let Some(caps) = failed_re.captures(line) {
            let body = &caps[1];
            let failed = unittest_failed_count(body);
            let skipped = unittest_count(body, "skipped");
            return Some(StructuredOutcome {
                failed: Some(failed),
                passed: Some(total.saturating_sub(failed).saturating_sub(skipped)),
                ignored: (skipped > 0).then_some(skipped),
                findings: None,
            });
        }
    }
    // Footer present but no recognized OK/FAILED status line (truncated/garbled).
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

    // ---- python -m unittest (NER-253) ----

    /// unittest writes its footer/status to STDERR — feed it there in most cases.
    #[test]
    fn parses_passing_unittest_verbose_summary_on_stderr() {
        let stderr = "test_add (test_stats.StatsTest) ... ok\n\
                      ----------------------------------------------------------------------\n\
                      Ran 3 tests in 0.001s\n\n\
                      OK\n";
        let outcome = parse_structured(
            "python3",
            &args(&["-m", "unittest", "test_stats"]),
            "",
            stderr,
        )
        .expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.passed, Some(3));
        assert_eq!(outcome.ignored, None);
    }

    #[test]
    fn parses_passing_unittest_non_verbose_summary() {
        // Non-verbose run: just dots, then the footer and OK.
        let stderr = "...\n\
                      ----------------------------------------------------------------------\n\
                      Ran 3 tests in 0.000s\n\n\
                      OK\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.passed, Some(3));
    }

    #[test]
    fn parses_unittest_ok_with_skipped_populates_ignored() {
        let stderr = "Ran 4 tests in 0.002s\n\nOK (skipped=1)\n";
        let outcome = parse_structured(
            "python3",
            &args(&["-m", "unittest", "test_stats"]),
            "",
            stderr,
        )
        .expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, Some(1));
        assert_eq!(outcome.passed, Some(3)); // 4 ran - 1 skipped
    }

    #[test]
    fn parses_unittest_failed_failures_only() {
        let stderr = "Ran 5 tests in 0.003s\n\nFAILED (failures=2)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(2));
        assert_eq!(outcome.passed, Some(3));
    }

    #[test]
    fn parses_unittest_failed_counts_errors_as_failures() {
        // A unittest "error" (a non-assertion exception) must count toward `failed`.
        let stderr = "Ran 4 tests in 0.003s\n\nFAILED (failures=1, errors=1)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(2));
    }

    #[test]
    fn unittest_zero_tests_is_distinct_from_none() {
        let stderr = "Ran 0 tests in 0.000s\n\nOK\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.passed, Some(0));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn unittest_without_summary_footer_degrades_to_none() {
        // An import error before the runner finished: no "Ran N tests" footer.
        let stderr = "Traceback (most recent call last):\n  ImportError: no module named x\n";
        assert!(parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).is_none());
    }

    #[test]
    fn unittest_footer_without_status_line_degrades_to_none() {
        // Truncated output: footer present but the OK/FAILED line never arrived.
        let stderr = "Ran 3 tests in 0.001s\n";
        assert!(parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).is_none());
    }

    #[test]
    fn fixture_output_containing_ok_does_not_false_match() {
        // A test that prints "OK" in its own output (before the footer) must NOT be
        // read as the summary status — the status is anchored after "Ran N tests".
        let stderr = "OK this is just some test output\n\
                      Ran 2 tests in 0.001s\n\n\
                      FAILED (failures=1)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn unittest_summary_on_stdout_also_parses() {
        // Defensive: some wrappers merge/redirect streams onto stdout.
        let stdout = "Ran 2 tests in 0.001s\n\nOK\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), stdout, "").expect("parsed");
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn python3_minor_version_basename_is_accepted() {
        let stderr = "Ran 1 test in 0.001s\n\nOK\n";
        let outcome = parse_structured(
            "/usr/bin/python3.11",
            &args(&["-m", "unittest", "test_stats"]),
            "",
            stderr,
        )
        .expect("parsed");
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn child_subprocess_footer_does_not_shadow_parent_verdict() {
        // NER-253 code review (adversarial/high): a meta-test that spawns a child
        // `python -m unittest` with inherited stderr writes the child's
        // "Ran N … FAILED" footer BEFORE the real runner appends its own "Ran N … OK"
        // footer. The parser must anchor on the LAST footer so a green suite (exit 0)
        // is not falsely blocked by the child's failure.
        let stderr = "test_sub (test_meta.MetaTest) ... \
                      test_fails (test_child.ChildFails) ... FAIL\n\n\
                      ----------------------------------------------------------------------\n\
                      Ran 1 test in 0.000s\n\n\
                      FAILED (failures=1)\n\
                      ok\n\n\
                      ----------------------------------------------------------------------\n\
                      Ran 1 test in 0.048s\n\n\
                      OK\n";
        let outcome = parse_structured(
            "python3",
            &args(&["-m", "unittest", "test_meta"]),
            "",
            stderr,
        )
        .expect("parsed");
        assert_eq!(
            outcome.failed,
            Some(0),
            "parent verdict OK must win over child FAILED footer"
        );
        assert_eq!(outcome.passed, Some(1));
    }

    #[test]
    fn unexpected_successes_counts_as_a_failure() {
        // NER-253 code review (adversarial/medium): `FAILED (unexpected successes=1)`
        // is a real test-quality failure (a @unittest.expectedFailure that passed) and
        // unittest exits non-zero for it. The multi-word key must not collapse to its
        // trailing word and silently report `failed=0`.
        let stderr = "Ran 1 test in 0.000s\n\nFAILED (unexpected successes=1)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.passed, Some(0)); // 1 ran - 1 failed
    }

    #[test]
    fn unexpected_successes_combine_with_failures_and_errors() {
        // A mixed footer: assertion failures, errors, and an unexpected success all
        // count toward `failed`; `skipped`/`expected failures` do not.
        let stderr = "Ran 6 tests in 0.000s\n\n\
                      FAILED (failures=1, errors=1, unexpected successes=1, skipped=1, expected failures=1)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(3)); // 1 failure + 1 error + 1 unexpected success
        assert_eq!(outcome.ignored, Some(1)); // skipped
        assert_eq!(outcome.passed, Some(2)); // 6 ran - 3 failed - 1 skipped
    }

    #[test]
    fn python3_without_unittest_module_degrades_to_none() {
        // `python3 script.py` and `python3 -m mymodule` have no registered parser.
        let summary = "Ran 1 test in 0.001s\n\nOK\n";
        assert!(parse_structured("python3", &args(&["script.py"]), "", summary).is_none());
        assert!(parse_structured("python3", &args(&["-m", "mymodule"]), "", summary).is_none());
    }
}
