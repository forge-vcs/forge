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
//! **Scope (v0):** `cargo test` (libtest summary), `cargo clippy`,
//! `python -m unittest` (the unittest `TextTestRunner` summary), and `pytest`
//! (the bracketed `==== N passed, M failed … ====` summary line, NER-258) —
//! either as the `pytest` basename or as `python -m pytest`. The pytest summary
//! has many variants (passed/failed/skipped/xfailed/xpassed/errors, plugin
//! output, ANSI color); it is parsed numeric-only by stripping ANSI codes and
//! anchoring on the LAST bracketed summary line. A `python3` invocation that
//! does NOT name `-m unittest`/`-m pytest` (e.g. `python3 script.py`,
//! `python3 -m mymodule`) has no registered parser and degrades gracefully to
//! `None` (exit-code verdict).
//!
//! [`has_structured_parser`] exposes the dispatch identity as a pure predicate
//! (NER-259) so callers can reject a structurally-unsatisfiable structured gate
//! before any command runs; it shares the same private classifier as
//! [`parse_structured`] so the two cannot drift.
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

/// The concrete parser a `(program, args)` identity dispatches to. Used as the single
/// source of truth shared by [`parse_structured`] (which runs the matched parser) and
/// [`has_structured_parser`] (which only needs to know one exists), so the predicate and
/// the dispatcher can never disagree about which gates are structured-parseable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserKind {
    CargoTest,
    CargoClippy,
    PythonUnittest,
    Pytest,
}

/// Classify a `(program, args)` invocation into a [`ParserKind`], or `None` when no
/// structured parser is registered for it. This is the dispatch identity both
/// [`parse_structured`] and [`has_structured_parser`] build on; keep it the only place
/// that decides which invocations are structured-parseable.
fn structured_dispatch(program: &str, args: &[String]) -> Option<ParserKind> {
    let prog = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if prog == "cargo" {
        match args.first().map(String::as_str) {
            Some("test") => return Some(ParserKind::CargoTest),
            Some("clippy") => return Some(ParserKind::CargoClippy),
            _ => {}
        }
    }
    // A bare `pytest` basename routes to the pytest parser regardless of args.
    if prog == "pytest" {
        return Some(ParserKind::Pytest);
    }
    if is_python_interpreter(prog) {
        // `python -m unittest` must be checked before `python -m pytest`; they are
        // mutually exclusive in practice (a single `-m <tool>`), but ordering keeps the
        // intent explicit. `python -m mymodule` / `python script.py` match neither.
        if invokes_unittest(args) {
            return Some(ParserKind::PythonUnittest);
        }
        if invokes_pytest(args) {
            return Some(ParserKind::Pytest);
        }
    }
    None
}

/// Dispatch to a tool-specific parser by `(program, args)` identity. Returns `None`
/// when no parser matches or parsing fails (the gate then degrades to exit code).
pub fn parse_structured(
    program: &str,
    args: &[String],
    stdout: &str,
    stderr: &str,
) -> Option<StructuredOutcome> {
    match structured_dispatch(program, args)? {
        ParserKind::CargoTest => parse_cargo_test(stdout, stderr),
        ParserKind::CargoClippy => parse_cargo_clippy(stdout, stderr),
        ParserKind::PythonUnittest => parse_python_unittest(stdout, stderr),
        ParserKind::Pytest => parse_python_pytest(stdout, stderr),
    }
}

/// True iff a structured parser is registered for this `(program, args)` identity —
/// i.e. [`parse_structured`] could (given recognizable output) return `Some`. Pure,
/// numeric-only, and runs no command: it only inspects argv, so the same secret-safety
/// scope as the rest of this module applies. Used at `forge start` (NER-259) to reject a
/// `--require-tests-pass` gate whose program has no parser, which would otherwise be
/// structurally unsatisfiable (the gate reads `failed`, which stays `None`).
pub fn has_structured_parser(program: &str, args: &[String]) -> bool {
    structured_dispatch(program, args).is_some()
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

/// True when the argv invokes pytest via an adjacent `["-m", "pytest"]` pair.
/// `python3 -m mymodule` or `python3 script.py` do NOT match (no parser → `None`).
fn invokes_pytest(args: &[String]) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "-m" && pair[1] == "pytest")
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
            let body = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            // `skipped` and `expected failures` both ran-but-didn't-pass; fold both into
            // `ignored` (mirrors the pytest parser's `skipped + xfailed`) so `passed` is
            // not inflated by expected-failure tests.
            let ignored =
                unittest_count(body, "skipped") + unittest_count(body, "expected failures");
            return Some(StructuredOutcome {
                failed: Some(0),
                passed: Some(total.saturating_sub(ignored)),
                ignored: (ignored > 0).then_some(ignored),
                findings: None,
            });
        }
        if let Some(caps) = failed_re.captures(line) {
            let body = &caps[1];
            let failed = unittest_failed_count(body);
            let ignored =
                unittest_count(body, "skipped") + unittest_count(body, "expected failures");
            return Some(StructuredOutcome {
                failed: Some(failed),
                passed: Some(total.saturating_sub(failed).saturating_sub(ignored)),
                ignored: (ignored > 0).then_some(ignored),
                findings: None,
            });
        }
    }
    // Footer present but no recognized OK/FAILED status line (truncated/garbled).
    None
}

/// Strip ANSI SGR/CSI escape sequences (`\x1b[…<letter>`) so a color-wrapped pytest
/// summary line still matches. pytest emits color on a TTY (and some CI shims force it);
/// the bracketed summary can be wrapped in `\x1b[32m…\x1b[0m`. No repo-wide ANSI helper
/// exists, so this is local to the pytest path.
fn ansi_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("valid ansi escape regex"))
}

/// Match a pytest final summary line: a body bracketed by runs of `=`, e.g.
/// `==== 3 passed, 1 failed in 0.12s ====` or `==== no tests ran in 0.01s ====`.
/// Captures the inner body (between the leading/trailing `=` runs) in group 1.
///
/// NOTE: this matches the *shape* of a bracketed line only. pytest emits many section
/// headers with the same shape (`==== test session starts ====`, `==== FAILURES ====`,
/// `==== warnings summary ====`, `==== short test summary info ====`). Those are NOT
/// result summaries; [`parse_pytest_summary_body`] returns `None` for them so they
/// cannot overwrite the real result line with all-zero counts (NER-258 adversarial).
fn pytest_summary_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^=+\s+(.*?)\s+=+\s*$").expect("valid pytest summary line regex"))
}

/// Match pytest's *quiet-mode* (`-q` / `-qq`) final summary line, which is NOT bracketed:
/// e.g. `3 passed in 0.00s`, `1 failed, 2 passed in 0.01s`, `no tests ran in 0.01s`
/// (optionally a `(H:MM:SS)` clock suffix for long runs). Captures the body before the
/// `in <duration>` suffix in group 1; [`parse_pytest_summary_body`] then requires a count
/// word (or `no tests ran`) so non-summary lines like `done in 0.5s` are rejected. The
/// `in <float>s` anchor keeps this from matching arbitrary prose. Disjoint from the
/// bracketed regex: a bracketed line ends in a run of `=`, this one ends in the duration.
fn pytest_bare_summary_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(.*?\S)\s+in\s+\d+(?:\.\d+)?s(?:\s+\(\d+:\d{2}:\d{2}\))?\s*$")
            .expect("valid pytest bare summary regex")
    })
}

/// Match the `no tests ran` body variant (pytest's summary when zero tests were
/// collected, e.g. `no tests ran in 0.01s`). This is a *recognized* summary that
/// carries no count words, so it must be distinguished from a section header (which is
/// also count-word-free) when deciding whether a bracketed line is a real result line.
fn pytest_no_tests_ran_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bno tests ran\b").expect("valid pytest no-tests regex"))
}

/// Tolerant count scan over a pytest summary body: `(\d+) <word>` for each outcome word.
/// pytest writes singular/plural (`1 error`/`2 errors`, `1 warning`/`2 warnings`), so the
/// alternation covers both. Numeric-only — never captures node ids / test names.
fn pytest_count_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\d+)\s+(passed|failed|errors?|skipped|xfailed|xpassed|deselected|warnings?)")
            .expect("valid pytest count regex")
    })
}

/// Numeric outcome counts parsed from a single pytest summary body.
#[derive(Default)]
struct PytestCounts {
    passed: u64,
    failed: u64,
    errors: u64,
    skipped: u64,
    xfailed: u64,
    xpassed: u64,
}

/// Parse a `pytest` (or `python -m pytest`) run's structured outcome (NER-258).
///
/// pytest writes its real result summary to STDOUT, so STDOUT is authoritative: we take
/// the LAST recognized result line on stdout and only fall back to stderr when stdout has
/// none (a wrapper that merged/redirected streams). Scanning stderr as an equal stream
/// would let any pytest-shaped line on stderr — a child subprocess's summary, a CI banner,
/// a `pytest_terminal_summary` hook writing to stderr — shadow the real stdout result
/// (NER-258 adversarial/correctness review). Within a stream we still keep the LAST
/// recognized result line, because a child pytest can print its summary before the real
/// one on the same stream (lesson a from the unittest parser).
///
/// Critically, only *result* lines update `last`: section headers (`==== FAILURES ====`,
/// `==== warnings summary ====`, …) share the bracketed shape but carry no count words, so
/// [`parse_pytest_summary_body`] returns `None` for them and they cannot overwrite a real
/// result with all-zero counts. Returns `None` when neither stream has a recognizable
/// result line — never a spoofable `failed: 0`.
fn parse_python_pytest(stdout: &str, stderr: &str) -> Option<StructuredOutcome> {
    let counts = last_pytest_result_counts(stdout).or_else(|| last_pytest_result_counts(stderr))?;
    // Mapping (NER-258): a collection/setup error counts as a failure for the gate;
    // skipped + xfailed are ignored; xpassed folds into passed best-effort.
    let failed = counts.failed + counts.errors;
    let passed = counts.passed + counts.xpassed;
    let ignored = counts.skipped + counts.xfailed;
    Some(StructuredOutcome {
        passed: Some(passed),
        failed: Some(failed),
        ignored: (ignored > 0).then_some(ignored),
        findings: None,
    })
}

/// The counts from the LAST *recognized result* summary line in a single stream, or `None`
/// when the stream has no result line. Strips ANSI first (lesson b) so a colored summary
/// still matches; rejects section headers via [`parse_pytest_summary_body`].
fn last_pytest_result_counts(raw: &str) -> Option<PytestCounts> {
    let cleaned = ansi_re().replace_all(raw, "");
    let bracketed_re = pytest_summary_line_regex();
    let bare_re = pytest_bare_summary_regex();
    let mut last: Option<PytestCounts> = None;
    for line in cleaned.lines() {
        let trimmed = line.trim_end();
        // Default-mode (`==== 3 passed in 0.00s ====`) and quiet-mode (`3 passed in 0.00s`)
        // summaries are mutually exclusive shapes — the former ends in `=`, the latter in
        // the duration — so trying the bracketed body first then the bare body is safe.
        let body = bracketed_re
            .captures(trimmed)
            .or_else(|| bare_re.captures(trimmed))
            .map(|caps| caps[1].to_string());
        if let Some(body) = body {
            if let Some(counts) = parse_pytest_summary_body(&body) {
                last = Some(counts);
            }
        }
    }
    last
}

/// Parse the count words out of a single bracketed pytest line's body, returning `Some`
/// only when the body is a real *result* summary and `None` for a section header.
///
/// A real result line either carries at least one count word (`3 passed`, `1 failed`, …)
/// or is the `no tests ran` variant. pytest's section headers (`test session starts`,
/// `FAILURES`, `ERRORS`, `warnings summary`, `short test summary info`) share the
/// bracketed shape but have neither, so they return `None` and never overwrite a real
/// result with all-zero counts (NER-258 adversarial review).
fn parse_pytest_summary_body(body: &str) -> Option<PytestCounts> {
    let mut counts = PytestCounts::default();
    let mut saw_outcome = false;
    for caps in pytest_count_regex().captures_iter(body) {
        let n = caps[1].parse::<u64>().unwrap_or(0);
        match &caps[2] {
            "passed" => counts.passed += n,
            "failed" => counts.failed += n,
            "error" | "errors" => counts.errors += n,
            "skipped" => counts.skipped += n,
            "xfailed" => counts.xfailed += n,
            "xpassed" => counts.xpassed += n,
            // `deselected` / `warnings` are recognized by the count regex but carry no
            // pass/fail meaning, so they are intentionally not accumulated — and crucially
            // do NOT mark this body as a result. A standalone `N warnings in Xs` /
            // `N deselected in Xs` line (a CI wrapper or plugin re-emitting a count-only
            // summary after the real result) must not register as an all-zero result that
            // clobbers a prior real `failed` count to a silent failed=0 false pass
            // (NER-258 adversarial). It still counts as a result when it co-occurs with a
            // real outcome word on the same line (`1 failed, 3 warnings in Xs`), because
            // `failed` sets `saw_outcome` first.
            _ => continue,
        }
        saw_outcome = true;
    }
    if saw_outcome {
        return Some(counts);
    }
    // No pass/fail outcome words: only the `no tests ran` variant is still a valid
    // (all-zero) result; anything else with this shape — a section header, or a bare
    // `N deselected/warnings in Xs` line — is rejected so it cannot clobber a real result
    // line with all-zero counts.
    pytest_no_tests_ran_regex().is_match(body).then_some(counts)
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
        // count toward `failed`; `skipped`/`expected failures` are ignored (not passes).
        let stderr = "Ran 6 tests in 0.000s\n\n\
                      FAILED (failures=1, errors=1, unexpected successes=1, skipped=1, expected failures=1)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(3)); // 1 failure + 1 error + 1 unexpected success
        assert_eq!(outcome.ignored, Some(2)); // 1 skipped + 1 expected failure
        assert_eq!(outcome.passed, Some(1)); // 6 ran - 3 failed - 2 ignored
    }

    #[test]
    fn ok_with_expected_failures_are_ignored_not_passed() {
        // An all-green run that includes `@unittest.expectedFailure` tests: those ran and
        // failed expectedly, so they are `ignored`, never `passed`.
        let stderr = "Ran 5 tests in 0.000s\n\nOK (skipped=1, expected failures=2)\n";
        let outcome =
            parse_structured("python3", &args(&["-m", "unittest"]), "", stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, Some(3)); // 1 skipped + 2 expected failures
        assert_eq!(outcome.passed, Some(2)); // 5 ran - 3 ignored
    }

    #[test]
    fn python3_without_unittest_module_degrades_to_none() {
        // `python3 script.py` and `python3 -m mymodule` have no registered parser.
        let summary = "Ran 1 test in 0.001s\n\nOK\n";
        assert!(parse_structured("python3", &args(&["script.py"]), "", summary).is_none());
        assert!(parse_structured("python3", &args(&["-m", "mymodule"]), "", summary).is_none());
    }

    // ---- pytest (NER-258) ----

    #[test]
    fn parses_all_passed_pytest_summary_via_basename_and_module() {
        let out = "===================== 3 passed in 0.05s ======================\n";
        // bare `pytest` basename
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(3));
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, None);
        // `python3 -m pytest`
        let outcome =
            parse_structured("python3", &args(&["-m", "pytest"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(3));
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, None);
    }

    #[test]
    fn parses_pytest_failures() {
        let out = "================= 2 passed, 1 failed in 0.1s =================\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.passed, Some(2));
    }

    #[test]
    fn pytest_errors_count_as_failures() {
        // A collection/setup error must count as a failure for the gate.
        let plural = "============= 1 passed, 2 errors in 0.1s ==============\n";
        let outcome = parse_structured("pytest", &args(&[]), plural, "").expect("parsed");
        assert_eq!(outcome.failed, Some(2));
        assert_eq!(outcome.passed, Some(1));
        // singular `1 error`
        let singular = "============= 1 passed, 1 error in 0.1s ==============\n";
        let outcome = parse_structured("pytest", &args(&[]), singular, "").expect("parsed");
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn pytest_skipped_and_xfailed_are_ignored() {
        let out = "========= 1 passed, 2 skipped, 1 xfailed in 0.1s =========\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.ignored, Some(3));
        assert_eq!(outcome.passed, Some(1));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn pytest_xpassed_folds_into_passed() {
        let out = "============= 1 passed, 1 xpassed in 0.1s ==============\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(2));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn pytest_no_tests_ran_is_recognized_and_distinct_from_none() {
        let out = "==================== no tests ran in 0.01s ====================\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(0));
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.ignored, None);
    }

    #[test]
    fn parses_pytest_quiet_mode_bare_summary() {
        // `pytest -q` emits a NON-bracketed final line (`3 passed in 0.00s`). Caught by a
        // live dogfood: the bracketed-only parser returned None for `-q`, the common CI form.
        let out =
            "...                                                  [100%]\n3 passed in 0.00s\n";
        let outcome = parse_structured(
            "/tmp/venv/bin/python",
            &args(&["-m", "pytest", "-q"]),
            out,
            "",
        )
        .expect("parsed");
        assert_eq!(outcome.passed, Some(3));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn parses_pytest_quiet_mode_bare_summary_with_failures() {
        let out = "F..                                                  [100%]\n1 failed, 2 passed in 0.01s\n";
        let outcome = parse_structured("pytest", &args(&["-q"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(2));
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn pytest_quiet_no_tests_ran_is_recognized() {
        let out = "no tests ran in 0.01s\n";
        let outcome = parse_structured("pytest", &args(&["-q"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(0));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn pytest_bare_summary_requires_a_real_duration_and_count() {
        // The `in <float>s` anchor + count-word requirement reject prose that merely ends
        // in "... in <word>" so a test that prints "done in review" cannot spoof a result.
        assert!(parse_structured("pytest", &args(&["-q"]), "done in review\n", "").is_none());
        // A count word without the duration anchor is also not a summary line.
        assert!(parse_structured("pytest", &args(&["-q"]), "3 passed already\n", "").is_none());
    }

    #[test]
    fn pytest_quiet_long_run_clock_suffix_still_parses() {
        // Runs over 60s get a `(H:MM:SS)` clock suffix after the seconds.
        let out = "5 passed in 75.43s (0:01:15)\n";
        let outcome = parse_structured("pytest", &args(&["-q"]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(5));
        assert_eq!(outcome.failed, Some(0));
    }

    #[test]
    fn pytest_ansi_colored_summary_still_parses() {
        // pytest wraps the summary in color on a TTY; strip ANSI before matching.
        let out = "\x1b[32m=========== 3 passed, 1 failed in 0.12s ===========\x1b[0m\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.passed, Some(3));
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn pytest_unrecognized_output_degrades_to_none() {
        // A traceback with no bracketed summary line → None (never a spoofable failed=0).
        let out = "Traceback (most recent call last):\n  File \"x.py\", line 1\nSyntaxError\n";
        assert!(parse_structured("pytest", &args(&[]), out, "").is_none());
    }

    #[test]
    fn pytest_last_summary_wins_across_stream() {
        // A child pytest subprocess prints its own FAILED summary before the real PASSED
        // summary on the shared stream; the parser must read the LAST one.
        let out = "==================== 1 failed in 0.01s ====================\n\
                   ...more output...\n\
                   ==================== 3 passed in 0.05s ====================\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.passed, Some(3));
    }

    #[test]
    fn pytest_stdout_is_authoritative_over_stderr() {
        // pytest writes its real result to stdout. A pytest-shaped summary on stderr
        // (a child subprocess, a CI banner, a `pytest_terminal_summary` hook writing to
        // stderr) must NOT shadow the real stdout result — stdout is authoritative, and
        // stderr is consulted only when stdout has no result line. Here stdout reports the
        // real FAILED result; an innocent PASSED summary on stderr must not override it
        // (NER-258 adversarial/correctness review).
        let stdout = "==================== 2 failed in 0.5s ====================\n";
        let stderr = "==================== 5 passed in 0.1s ====================\n";
        let outcome = parse_structured("pytest", &args(&[]), stdout, stderr).expect("parsed");
        assert_eq!(
            outcome.failed,
            Some(2),
            "real stdout failures must not be shadowed by a stderr summary"
        );
        assert_eq!(outcome.passed, Some(0));
    }

    #[test]
    fn pytest_falls_back_to_stderr_when_stdout_has_no_summary() {
        // A wrapper that redirects pytest's summary onto stderr (stdout carries no result
        // line) must still be parsed — stderr is the fallback, not ignored.
        let stdout = "collected 3 items\n";
        let stderr = "==================== 3 passed in 0.05s ====================\n";
        let outcome = parse_structured("pytest", &args(&[]), stdout, stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.passed, Some(3));
    }

    #[test]
    fn pytest_section_header_after_result_does_not_clobber_to_zero() {
        // NER-258 adversarial/high: pytest emits bracketed *section headers* (FAILURES,
        // warnings summary, short test summary info) that share the summary line shape but
        // carry no count words. If one appears AFTER the real result line — e.g. a
        // `pytest_terminal_summary` hook or a plugin re-prints a header last — it must NOT
        // overwrite the real result with all-zero counts (a silent failed=0 false pass).
        let out = "===================== 3 failed in 0.52s =====================\n\
                   ===================== warnings summary ======================\n\
                   some warning text\n\
                   ================== short test summary info ===================\n\
                   FAILED test_x.py::test_a\n";
        let outcome = parse_structured("pytest", &args(&[]), out, "").expect("parsed");
        assert_eq!(
            outcome.failed,
            Some(3),
            "a trailing section header must not clobber the real result to failed=0"
        );
    }

    #[test]
    fn pytest_section_header_on_stderr_does_not_clobber_stdout_result() {
        // The most direct over-block path from the review: the real failing result is on
        // stdout; a bracketed section header (count-word-free) lands on stderr after it.
        // Because stdout is authoritative AND section headers are rejected, the real
        // result survives either way.
        let stdout = "===================== 3 failed in 0.52s =====================\n";
        let stderr = "===================== warnings summary ======================\n";
        let outcome = parse_structured("pytest", &args(&[]), stdout, stderr).expect("parsed");
        assert_eq!(outcome.failed, Some(3));
    }

    #[test]
    fn pytest_trailing_bare_count_only_line_does_not_clobber_to_zero() {
        // NER-258 adversarial: a bare quiet-mode count-only line whose only count words are
        // `warnings`/`deselected` (a CI wrapper or plugin re-emitting a summary after the
        // real result) matches the bare-summary shape AND the count regex, but carries no
        // pass/fail outcome. It must NOT register as an all-zero result that clobbers the
        // real `1 failed` to a silent failed=0 false pass.
        for trailing in ["2 warnings in 0.10s", "3 deselected in 0.10s"] {
            let out = format!("1 failed in 0.10s\n{trailing}\n");
            let outcome = parse_structured("pytest", &args(&["-q"]), &out, "").expect("parsed");
            assert_eq!(
                outcome.failed,
                Some(1),
                "trailing `{trailing}` must not clobber the real failure to failed=0"
            );
        }
    }

    #[test]
    fn pytest_combined_outcome_with_warnings_on_same_line_still_parses() {
        // The fix must NOT reject a legitimate combined summary that mixes a real outcome
        // word with `warnings`/`deselected` on the same line — `failed` marks it a result.
        let out = "1 failed, 2 passed, 3 warnings, 4 deselected in 0.10s\n";
        let outcome = parse_structured("pytest", &args(&["-q"]), out, "").expect("parsed");
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.passed, Some(2));
    }

    #[test]
    fn pytest_session_start_header_is_not_a_result() {
        // `==== test session starts ====` is a header, not a result. On its own (no real
        // result line anywhere) it must degrade to None, never a spoofable failed=0.
        let out = "================= test session starts =================\n\
                   collected 0 items\n";
        assert!(parse_structured("pytest", &args(&[]), out, "").is_none());
    }

    #[test]
    fn pytest_shaped_output_under_unittest_or_plain_python_does_not_route_to_pytest() {
        // `python3 -m unittest …` routes to the unittest arm even if pytest-shaped text
        // is present; `python3 script.py` with pytest-shaped output has no parser → None.
        let unittest_out = "Ran 2 tests in 0.001s\n\nOK\n";
        let outcome = parse_structured(
            "python3",
            &args(&["-m", "unittest", "test_mod"]),
            "",
            unittest_out,
        )
        .expect("unittest parsed");
        assert_eq!(outcome.failed, Some(0));
        let pytest_shaped = "==================== 5 passed in 0.05s ====================\n";
        assert!(parse_structured("python3", &args(&["script.py"]), pytest_shaped, "").is_none());
    }

    // ---- has_structured_parser predicate (NER-259) ----

    #[test]
    fn has_structured_parser_true_for_registered_gates() {
        assert!(has_structured_parser("cargo", &args(&["test"])));
        assert!(has_structured_parser("cargo", &args(&["clippy"])));
        assert!(has_structured_parser(
            "python3",
            &args(&["-m", "unittest", "m"])
        ));
        assert!(has_structured_parser("pytest", &args(&[])));
        assert!(has_structured_parser("python3", &args(&["-m", "pytest"])));
        // basename is taken from the path, so a fully-qualified path still routes.
        assert!(has_structured_parser(
            "/usr/bin/python3.11",
            &args(&["-m", "pytest"])
        ));
        assert!(has_structured_parser("/usr/local/bin/pytest", &args(&[])));
    }

    #[test]
    fn has_structured_parser_false_for_unregistered_gates() {
        assert!(!has_structured_parser("python3", &args(&["script.py"])));
        assert!(!has_structured_parser(
            "python3",
            &args(&["-m", "mymodule"])
        ));
        assert!(!has_structured_parser("cargo", &args(&["build"])));
        assert!(!has_structured_parser("./my_script", &args(&["--run"])));
        assert!(!has_structured_parser("python3", &args(&[])));
    }

    #[test]
    fn predicate_agrees_with_dispatcher_on_a_matrix() {
        // The predicate must never claim a parser the dispatcher lacks, nor vice-versa.
        // A representative matrix of (program, args); for each, `has_structured_parser`
        // must equal `structured_dispatch(...).is_some()` (the dispatch identity both are
        // built on). We assert against the underlying classifier directly.
        let cases: &[(&str, Vec<String>)] = &[
            ("cargo", args(&["test"])),
            ("cargo", args(&["clippy"])),
            ("cargo", args(&["build"])),
            ("cargo", args(&[])),
            ("pytest", args(&[])),
            ("pytest", args(&["-q"])),
            ("python3", args(&["-m", "unittest", "m"])),
            ("python3", args(&["-m", "pytest"])),
            ("python3", args(&["-m", "mymodule"])),
            ("python3", args(&["script.py"])),
            ("python3", args(&[])),
            ("/usr/bin/python3.11", args(&["-m", "pytest"])),
            ("./my_script", args(&["--run"])),
        ];
        for (program, argv) in cases {
            assert_eq!(
                has_structured_parser(program, argv),
                structured_dispatch(program, argv).is_some(),
                "predicate/dispatcher disagree for {program} {argv:?}"
            );
        }
    }
}
