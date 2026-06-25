# Security Policy

Forge is an agent-native source-control CLI. Security reports are especially
important when they affect source snapshots, evidence logs, redaction, trust
policy, signatures, sync bundles, native object storage, or Git export
provenance.

## Supported Versions

Forge is currently in public release-candidate status.

| Version | Supported |
| --- | --- |
| `main` | Yes |
| Latest `v0.1.x` release candidate | Best effort |
| Older tags and historical commits | No |

Until Forge reaches a stable release, security fixes target `main` first and
may be released as a new release candidate when appropriate.

## Reporting a Vulnerability

Please do not open a public issue for a suspected vulnerability.

Use GitHub private vulnerability reporting:

<https://github.com/forge-vcs/forge/security/advisories/new>

If GitHub private reporting is unavailable to you, open a public issue that
only says you have a security report to share and does not include exploit
details, secrets, crash payloads, or private data.

Helpful reports include:

- Forge version, commit SHA, operating system, and shell.
- The command or workflow involved.
- A minimal reproduction using a temporary repository and fake data.
- Whether the issue affects snapshots, redaction, signatures, trust policy,
  sync import/export, native storage, or Git export provenance.
- Any logs needed to understand the issue, with secrets removed.

Please do not include real access tokens, private keys, credentials, customer
data, or proprietary source code in a report.

## Response Expectations

For actionable reports, maintainers aim to:

- acknowledge the report within 7 days
- confirm scope and severity after reproduction
- keep the reporter updated while a fix is prepared
- credit the reporter in the advisory or release notes if they want credit
- request a CVE when the impact justifies one

This project does not currently run a paid bug bounty program.

## In Scope

Examples of security-sensitive areas:

- secret-redaction bypasses in captured command output or persisted evidence
- snapshots or exports that include `.forge`, `.env`, private keys, credential
  files, or other excluded paths
- signature, trust-policy, attestation, or `doctor` verification bypasses
- tampered native history, decisions, evidence, or sync data accepted as valid
- sync bundles or remote peers causing path traversal, repository corruption,
  arbitrary file writes, or unsafe materialization
- Git export provenance trailers that can be forged or incorrectly verified
- crashes or denial of service from malformed repositories, objects, packs, or
  sync manifests

## Out of Scope

The following usually do not qualify as vulnerabilities by themselves:

- issues that require full control of the user's machine or shell
- reports based only on outdated dependencies without an exploitable Forge path
- social engineering, phishing, spam, or physical attacks
- disclosure of secrets you created only for your own test
- missing hardening that does not cross a documented trust or data boundary

## Safe Harbor

Good-faith research is welcome when it uses your own repositories, your own
data, and non-destructive test cases. Do not access, modify, delete, or
exfiltrate data that is not yours. Do not disrupt services or other users.

Forge is currently a local CLI and sync tool, not a hosted multi-tenant service.
Please keep testing focused on local repositories, local fixtures, and test
peers you control.
