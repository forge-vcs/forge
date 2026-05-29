---
date: 2026-05-29
topic: encrypted-env-secrets
---

# Encrypted Env Secrets — Safe Agent Secret Access (NER-141)

## Summary

A `forge secret` capability: a human encrypts env values through a forge-owned, audited crypto layer with the key held in the OS keychain (never on disk); the ciphertext rides snapshots / restore / exports safely; and `forge run` decrypts in memory for the real workload. The honest, agreed bar is that this stops *accidental* and *at-rest* leakage of plaintext — not a determined same-user agent, which cannot be stopped when that agent writes and runs its own code.

---

## Problem Frame

Forge today excludes secret-risk files (`.env`, `.env.*`, `*.pem`, credential paths) from both snapshots and exports via the shared `is_secret_risk_path` / `is_ignored_by_policy` predicate (`crates/forge-content/src/lib.rs:82`, shipped in NER-133). That is safe but lossy: `restore` won't bring `.env` back, an attempt that needs env vars can't be faithfully reproduced, and compare/rank is blind to env config.

The deeper pain the operator actually cares about is different from "reproducibility": **the agent is the party they don't fully trust with plaintext secrets.** They want an agent to be able to *use* `GRAPH_PASSWORD` to run a real workload while only ever *seeing* ciphertext (`JAHAHD...`) in the env file — never `myPassword!`.

Two facts about the current code define the boundary of what is achievable:

- `forge run` spawns the child with the parent's inherited environment and never injects or clears env (`crates/forge-evidence/src/lib.rs:49`). At the solo-dev tier the plaintext `.env` already sits gitignored in the worktree and the app loads it itself, so the secret is *already* available to anything the agent runs.
- The evidence redactor is line-oriented `key=value` only (`crates/forge-content/src/lib.rs:134`); a bare printed value on its own line is not caught.

The consequence: if the agent controls the code that runs with the decrypted secret, the agent can read the secret (`forge run -- printenv`, or an agent-authored test that dumps `process.env`). The OS keychain raises the bar on the *key at rest*; it does nothing about a *value live in a process the agent controls*. So the realistic, defensible posture — not zero-trust — is the target this doc scopes.

```
trust boundary (what the agent sees vs. what it cannot easily reach)

  [ OS keychain ]  ──fetch in memory only──┐        key NEVER on disk,
   (key at rest)                           │        never in repo / .forge
                                           ▼
  .env (committed)        forge run --with-secrets        child process
  GRAPH_PASSWORD=         ───decrypt in memory───▶        env: GRAPH_PASSWORD=
  enc:JAHAHD...                                           myPassword!  (in-memory)
       │                                                       │
   capturable:                                          if printed → forge
   snapshots/restore/                                   exact-value redacts
   exports (ciphertext)                                 before evidence persist
       │                                                       │
   plaintext .env / mixed  ── still excluded ──┘         conceded limit: agent's
   (content-verified)                                    own code CAN still read it
```

---

## Actors

- A1. **Operator (human):** owns the secrets, enters plaintext values into forge's prompt, and is the only party trusted with plaintext. Holds the keychain.
- A2. **Agent:** writes and runs code in the repo via `forge run`. Treated as untrusted-for-reading: should see only ciphertext in normal flows. Never the actor that enters a plaintext secret.
- A3. **forge CLI:** the trusted process that encrypts on entry, fetches the key from the keychain, decrypts in memory at run time, and redacts evidence.

---

## Key Flows

- F1. **Encrypt a secret (entry)**
  - **Trigger:** operator runs `forge secret set GRAPH_PASSWORD`.
  - **Actors:** A1, A3.
  - **Steps:** forge prompts for the value with hidden input (or reads piped stdin) → encrypts in memory with the repo key → writes the ciphertext into the env file → ensures the key exists in the OS keychain. Plaintext never lands on disk and never appears in argv/shell history.
  - **Outcome:** the env file holds `GRAPH_PASSWORD=enc:...`; the agent reading the file sees ciphertext.
  - **Covered by:** R1, R2, R6, R7.

- F2. **Run a workload with secrets**
  - **Trigger:** operator/agent runs `forge run --with-secrets -- <cmd>`.
  - **Actors:** A2, A3.
  - **Steps:** forge fetches the key from the keychain → decrypts values in memory → injects them into the child env → captures stdout/stderr → exact-value-redacts any injected plaintext before persisting evidence.
  - **Outcome:** the workload runs with real secrets; no plaintext is written to `.forge`, the DB, or an export. If the agent's own command prints a value, it is redacted from stored evidence (but the running command did observe it — the conceded limit).
  - **Covered by:** R11, R12, R13, R14.

- F3. **Capture / restore / export**
  - **Trigger:** any snapshot, restore, or export.
  - **Actors:** A3.
  - **Steps:** policy checks each env file's content → a fully-ciphertext file is captured and round-trips; a plaintext or mixed file is excluded as today; the key is never a captured artifact.
  - **Outcome:** the encrypted env config is reproducible and travels with exports; plaintext never does.
  - **Covered by:** R8, R9, R10.

---

## Requirements

**Secret entry and management**
- R1. `forge secret set <NAME>` reads the value from a hidden interactive prompt or piped stdin — never from a command-line argument — so plaintext never enters shell history or process arguments.
- R2. forge provides no plaintext-value-on-argv entry path, making human-mediated entry the natural path; the agent is not expected to be the actor that supplies a plaintext secret value.
- R3. `forge secret list` shows secret names only, never values.
- R4. `forge secret rm <NAME>` removes a secret from the encrypted file.
- R5. A one-shot command encrypts the plaintext values of an existing `.env` in place, so adopting the feature does not require re-entering every secret by hand.

**Encryption and key storage**
- R6. Values are encrypted using an audited, well-reviewed cryptographic library; forge does not implement its own crypto primitives.
- R7. The decryption key lives in the OS keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service) and is never written to a file in the repo or `.forge`; plaintext key material is fetched into memory only when needed. Any key material that ever exists as a file remains excluded by policy.
- R8. One key per forge repository.

**Capture / restore / export policy**
- R9. An env file whose values are all forge-ciphertext is recognized as safe and is included in snapshots, restore, and exports — carving an exception into today's blanket `.env*` exclusion.
- R10. Recognition is content-based (verify every value is ciphertext), not name-only, so a plaintext or mixed env file remains excluded exactly as today and a misnamed plaintext file cannot leak.

**Run-time decryption and injection**
- R11. `forge run` injects decrypted secret values into the child process environment only when explicitly opted in; the default `forge run` does not inject secrets.
- R12. Decryption happens in memory at run time; decrypted plaintext is never written to disk — not to `.forge`, the DB, or a restored/exported worktree.
- R13. When injection is requested but no key is available (locked or absent keychain, headless CI), `forge run` fails with a clear error rather than silently running the command without the secrets.

**Evidence leak containment**
- R14. Because forge holds the exact decrypted plaintext strings at run time, it redacts exact-value matches of injected secrets from captured stdout/stderr before persistence — catching bare printed values the existing `key=value` redactor misses — and labels such evidence secret-risk under the existing sensitivity model.

---

## Acceptance Examples

- AE1. **Covers R1, R2.** Given an agent session, when `forge secret set GRAPH_PASSWORD` runs, the value is read from a hidden prompt/stdin and never appears in `ps` output or shell history.
- AE2. **Covers R9, R10.** Given a `.env` with all values encrypted, when a snapshot runs, the file is captured and restores byte-identically; given a `.env` with at least one plaintext value, it is excluded from the snapshot and export as today.
- AE3. **Covers R11, R12.** Given `forge run --with-secrets -- printenv`, the child's env contains the decrypted values, yet no plaintext is written to `.forge` or the DB; given `forge run -- printenv` (no opt-in), no secrets are injected.
- AE4. **Covers R13.** Given no key available in the keychain, when `forge run --with-secrets -- <cmd>` runs, it exits with a clear "no key available" error and does not run `<cmd>` without the secrets.
- AE5. **Covers R14.** Given `forge run --with-secrets -- node -e 'console.log(process.env.GRAPH_PASSWORD)'` where the value is `myPassword!`, the stored evidence excerpt shows `myPassword!` redacted.

---

## Success Criteria

- An agent operating in the repo reads only ciphertext from the env file; plaintext never lands in the worktree at rest beyond the workload's own memory, nor in snapshots, the DB, exports, or a published branch — while secrets remain usable for real `forge run` workloads.
- A plaintext or mixed env file is never captured or exported (no regression of the NER-133 secret-exclusion default).
- Downstream `ce-plan` can implement without inventing the threat bar, entry UX, crypto mechanism, or policy-recognition behavior; and the conceded limit (a same-user agent can still extract via its own code) is explicit so no one over-claims zero-trust.

---

## Scope Boundaries

- **Not** cryptographic zero-trust against a same-user agent that authors and runs its own code — conceded architecturally impossible at this tier.
- No key rotation or revocation in v0.
- No team / multi-user key sharing.
- No external KMS (AWS / GCP / Azure) backends.
- No CI / hosted-runner key delivery (fail-loud when no keychain).
- No cross-machine key transport — the key never travels, by design.
- No allowlisted / sandboxed command-execution model (the path that would enable real zero-trust against agent-authored code).
- Not wrapping dotenvx / SOPS, and not a thin policy-only no-crypto layer — both considered and rejected; interop is not precluded later.
- No expansion of secret-file coverage beyond env `name=value` pairs; `.pem` / `.key` / credential files stay excluded as today.
- Does not subsume NER-136's full entropy / PEM / JSON redactor corpus; this adds only exact-value redaction of injected secrets.

---

## Key Decisions

- **Native, forge-owned crypto with an audited Rust crate** (candidates: `age` for encryption, `keyring` for the cross-platform OS keychain), chosen over (a) wrapping dotenvx/SOPS — a hard runtime binary dependency plus a leaky keychain→tool key hand-off — and (b) a thin policy-only layer that wouldn't deliver the keychain or `forge secret` UX. Rationale: no external binary, one cohesive UX, works offline, and forge controls exact-value redaction tightly. ("Audited crate only, custom crypto banned" matches the ROADMAP Phase 9 stance.)
- **Explicit `forge secret set` over a file-watcher daemon:** a watcher has a plaintext-on-disk window between save and re-encrypt and silently fails if it isn't running; a one-shot command keeps plaintext off disk and fits forge's CLI model.
- **Content-based recognition of encrypted files, not name-only:** preserves the security default that any plaintext/mixed file is still excluded.
- **`forge run` injection is opt-in, off by default:** contains the new child-env leak surface.
- **Per-repo single key; fail loud when the key is unavailable:** simplest scope; avoids a silent run-without-secrets footgun.

---

## Dependencies / Assumptions

- Builds on the NER-133 secret-exclusion policy (`is_ignored_by_policy` / `is_secret_risk_path`, `crates/forge-content/src/lib.rs`); this feature carves the encrypted-file exception into that shared predicate without weakening it for plaintext.
- Extends the NER-136 (Phase 5) evidence redactor with exact-value redaction; the heuristic / entropy / PEM / JSON redactor remains NER-136's scope.
- Scoped precursor to the NER-140 (Phase 9) crypto / key / identity model: establishes the OS-keychain + audited-crypto pattern for the narrow secret-at-rest case, which Phase 9 signing can reuse.
- Assumes an OS keychain is present on the operator's machine; headless environments without one are out of scope and must fail loud.
- Assumes `forge run` keeps spawning the child with forge's inherited env (verified at `crates/forge-evidence/src/lib.rs:49`); injection adds the decrypted values to that env.

---

## Outstanding Questions

### Resolve Before Planning

- (none — threat bar, entry UX, mechanism, and policy-recognition behavior are decided.)

### Deferred to Planning

- [Affects R11][Technical] Injection opt-in surface: per-invocation flag, per-repo policy/config, or both. Lean: a flag in v0.
- [Affects R9, R10][Technical] Encrypted-file shape: per-value encryption in place in `.env` (single familiar file) vs. a dedicated `.env.enc` / vault file. Lean: in-place per-value encryption, with capture gated on content verification.
- [Affects R6, R7][Needs research] Confirm the specific audited crates (`age`, `keyring`) and their audit/maintenance status; validate Linux Secret Service availability assumptions and headless behavior.
- [Affects R14][Technical] Whether to also redact encoded forms of injected values (base64 / url-encoded) alongside exact matches.
