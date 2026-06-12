---
date: 2026-05-29
topic: claude-session-relay
title: "Automated Claude Code Session Relay — Design"
status: design (not yet planned or built)
substrate: herdr (chosen 2026-05-29)
origin: 6-agent research workflow (session-relay-design), then synthesis
scope_note: >
  This is a dev-workflow / tooling design for running Claude Code, NOT a Forge
  product feature. Filed here for durability; relocate if it grows into its own track.
---

# Automated Claude Code Session Relay — Design

Goal: a self-perpetuating loop so that when one Claude Code session's context fills,
it writes a handoff, spawns a fresh session pre-seeded with that handoff in a new
pane, continues the work, and the old pane is torn down — all steerable from a phone.
**Chosen substrate: herdr** (runs inside Warp; only substrate that can script the full
loop). See § Substrate recommendation for why, and § Open questions for what to verify
before building.

## The relay loop

1. **Detect** — The active session approaches compaction. The `PreCompact` hook fires (matcher `auto`) — the only reliable "context nearly full" signal Claude Code exposes.
2. **Handoff** — The hook script invokes the handoff generation (a headless `claude -p` turn, or the `/handoff` skill output) and writes the handoff doc to a stable path under the OS temp dir, then mirrors it to `docs/handoffs/`.
3. **Spawn** — The hook (async) tells the pane substrate to open a new pane in the same repo/worktree.
4. **Seed** — The new pane launches a fresh `claude` session with the handoff file fed in (via `@file` reference or `--append-system-prompt-file`), under unattended permissions with turn/budget caps.
5. **Continue** — The new session resumes the work. It captures its own `session_id` (from `--output-format json`) so the orchestrator can address it later.
6. **Close** — Once the new session has confirmed it's alive and working, it (or a central orchestrator) closes the old pane.
7. **Repeat** — The new session carries the same hook config, so step 1 recurs when *it* fills. A relay-depth counter guards against infinite chaining (see Risks).

A note on framing: `PreCompact` fires when Claude has *decided to compact*, not at a fixed token threshold. So this is really "compaction is imminent" detection. If you want the relay to fire *instead of* compacting, the `PreCompact` hook can block compaction (exit 2) after writing the handoff — but blocking unconditionally causes thrashing, so gate it on a relay-depth check.

## Trigger

**Hook: `PreCompact` with matcher `auto`.** This is the closest thing to a context-full signal. (There is no hook payload field for token count or context-remaining — confirmed in the findings. The only earlier-warning alternative is polling `transcript_path` JSONL size from a `Stop` hook, ~200KB ≈ heavily-used context, but that's a heuristic, not documented.)

`~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "matcher": "auto",
        "hooks": [
          {
            "type": "command",
            "async": true,
            "command": "$CLAUDE_PROJECT_DIR/.forge/relay/on-precompact.sh"
          }
        ]
      }
    ]
  }
}
```

The hook receives on stdin: `session_id`, `transcript_path`, `cwd`, `trigger` (`auto`|`manual`), `custom_instructions`. Run it `async: true` so spawning a pane doesn't block Claude. Note hooks run **without a controlling terminal** (since v2.1.139) — you cannot write to `/dev/tty`; spawning a GUI pane via `osascript`/`open` or a substrate CLI works because those don't need a TTY.

## Handoff generation

Two viable sources, in order of preference:

- **Headless one-shot inside the hook.** The cleanest automatable path — the `/handoff` skill is interactive and has no defined output path or START-PROMPT format (confirmed: its SKILL.md is a 15-line instruction-only file, no mktemp pattern, no copy-paste block). So drive a dedicated headless turn against the *current* session instead:

```bash
#!/usr/bin/env bash
# on-precompact.sh  (excerpt)
set -euo pipefail
INPUT=$(cat)
SID=$(printf '%s' "$INPUT" | jq -r '.session_id')
HANDOFF="$CLAUDE_PROJECT_DIR/docs/handoffs/relay-$(date +%Y%m%d-%H%M%S).md"

# Resume the filling session headlessly and ask it to write its own handoff.
claude -p --resume "$SID" --output-format text \
  --permission-mode dontAsk --allowedTools "Read,Write,Bash(git log *),Bash(git status *)" \
  "Write a handoff document for the next agent session to $HANDOFF. \
Include a 'suggested skills' section. Reference plans/PRDs by path, do not duplicate them. \
Redact secrets. End with a one-line 'NEXT TASK:' the new session should start on." \
  > /dev/null

ln -sf "$HANDOFF" "$CLAUDE_PROJECT_DIR/docs/handoffs/latest.md"
```

**Where it's written:** `docs/handoffs/relay-<timestamp>.md` (durable, per the repo convention) with a `latest.md` symlink the spawn step reads. The repo's existing handoff convention (`docs/handoffs/<plan-stem>-phase-<n>.md`) layers on top; for relays a timestamped name avoids collisions.

Caveat: resuming the same session that is mid-compaction to write the handoff is slightly racy. Safer is to read `transcript_path` directly and summarize it in a separate `--bare -p` call that has no dependency on the live session.

## Spawn + seed

The seed command is the same Claude invocation across substrates — only the "open a pane and run this" wrapper differs. Canonical seed:

```bash
claude --append-system-prompt-file docs/handoffs/latest.md \
  --output-format json --permission-mode auto \
  --max-turns 40 --max-budget-usd 3.00 \
  "Resume this handoff. Start on the NEXT TASK line."
```

`--append-system-prompt-file` is preferred over stdin (10 MB cap) and avoids the `@file` quoting hazards. `--permission-mode auto` gives a background safety classifier with no human prompts (requires v2.1.83+, Opus/Sonnet 4.6, Anthropic API — **not** Bedrock/Vertex). `--max-turns`/`--max-budget-usd` are the runaway guards.

### Warp (tab config + `warp://` URI)

The findings contain **no Warp-specific syntax** — do not invent a socket API or CLI Warp doesn't have. What is grounded: Warp supports Launch Configurations (YAML, one command per tab) and `warp://` deep links to open them. So the honest pattern is:

```bash
# Write a Launch Configuration YAML, then open it via deep link.
cat > ~/.warp/launch_configurations/forge-relay.yaml <<'EOF'
name: forge-relay
windows:
  - tabs:
      - title: forge-relay
        layout:
          cwd: /path/to/forge
          commands:
            - exec: >
                claude --append-system-prompt-file docs/handoffs/latest.md
                --output-format json --permission-mode auto --max-turns 40
                "Resume this handoff. Start on the NEXT TASK line."
EOF
open "warp://launch/forge-relay"
```

**Honest limitation:** Warp has no documented socket/CLI to *read pane output*, *wait on agent status*, or *close a specific pane* programmatically. Warp can *open* a seeded session but cannot complete the close-the-old-pane or status-detection halves of the loop without a workaround. This is the core reason it isn't the recommended substrate (below).

### herdr (socket/CLI — fully scriptable) — CHOSEN

```bash
herdr pane split 1-2 --direction right --no-focus
# Pane IDs are NOT durable — resolve the new one fresh:
NEWPANE=$(herdr pane list --json | jq -r '.[-1].id')   # confirm field name against your build
herdr pane run "$NEWPANE" "claude --append-system-prompt-file docs/handoffs/latest.md --permission-mode auto --max-turns 40 'Resume this handoff. Start on the NEXT TASK line.'"
# Wait until it's actually working before closing the old pane:
herdr wait agent-status "$NEWPANE" --status working --timeout 60000
```

`pane run` submits text + Enter atomically (prefer over `send-text` + `send-keys Enter`). Use `wait agent-status` (semantic state) for coding agents, not `wait output`. Timeouts are **milliseconds**.

### cmux (manaflow-ai, macOS — scriptable with caveats)

```bash
cmux new-split right
SID=$(cmux list-surfaces --json | jq -r '.[-1].surface_id')
cmux send-surface --surface "$SID" \
  "claude --append-system-prompt-file docs/handoffs/latest.md --permission-mode auto --max-turns 40 'Resume this handoff. Start on the NEXT TASK line.'\n"
```

Note the trailing `\n` to execute. cmux has **no** native "launch agent with prompt" command and **no** git-worktree command — you compose split + send-text yourself. The findings rate cmux **medium** confidence and flag that `read-screen`/`new-pane` forms circulating online are unverified; only `--surface` via `send-surface`/`focus-surface` is confirmed.

## Close the old pane

Closing should be **triggered by the new session** (it confirms it's alive, then tears down its predecessor) — this avoids orphaning work if the spawn fails. A central orchestrator is the alternative if you want one process owning all pane IDs, but it adds a daemon. Recommendation: new session closes the old pane, after a self-check.

The new session needs the **old pane's ID**, passed in via the handoff (e.g. an `OLD_PANE_ID:` line) or an env var set at spawn.

- **herdr:** `herdr pane close <old_pane_id>` — the *only* pane-termination verb (no `pane kill`). Re-resolve the ID first; IDs compact when panes close.
- **cmux:** No documented per-pane kill. Only `cmux close-workspace --workspace <id>`. So model **one workspace per relay generation**, and the new session closes the previous workspace: `cmux close-workspace --workspace "$OLD_WS"`.
- **Warp:** **No documented programmatic per-pane/tab close.** This half of the loop is not achievable with grounded Warp syntax — you'd fall back to manual close or an `osascript` UI hack (not in findings, so not recommended). Stated plainly: Warp cannot close the old pane automatically.

## Mobile steering

Grounded options, strongest first:

- **herdr remote/socket relay.** herdr's control surface is a local Unix socket (`~/.config/herdr/herdr.sock`) addressable via `HERDR_SOCKET_PATH`/`HERDR_SESSION`. The findings do **not** document a native herdr mobile app or hosted remote. The realistic mobile path is **SSH from a phone (e.g. Termius/Blink) into the Mac, then drive the herdr CLI/socket** — every relay verb (split, run, wait, close) works over that SSH session. This is the most complete mobile story because the *entire* loop is CLI-scriptable.
- **claude.ai / Claude Code remote.** Headless `-p` sessions are resumable by `session_id` via `--resume`. If you persist each generation's `session_id` somewhere the phone can reach, you can resume/continue from a mobile Claude Code client. The findings confirm resumability but do **not** confirm a specific mobile UI for it — treat as "resume by ID works; the mobile front-end is your choice."
- **cmux mobile.** The findings explicitly rate this **weak**: cmux ships `cmux ssh` and `claude-teams` but **no documented mobile app or web remote API**; its socket is local-only. Mobile = SSH-tunnel-it-yourself.
- **Warp mobile.** Nothing in the findings. No grounded mobile control path.

Net: **mobile steering = SSH into the Mac and drive the substrate CLI.** herdr makes that fully closed-loop; the others leave gaps.

## Substrate recommendation

**Default: herdr.** It's the only substrate in the findings that can script *all four* hard parts of the loop — spawn a seeded pane, **wait on semantic agent status**, **read pane output**, and **close a specific pane** — over a documented socket/CLI, and it runs **inside Warp** (it's terminal-native), so you keep your current Warp setup rather than migrating to a new app.

Tradeoffs:
- **vs Warp-native:** Warp can *open* a seeded session via `warp://` launch configs, but has no grounded API to read output, wait on status, or close a pane — it can't complete the loop unattended. herdr fills exactly those gaps while living inside Warp.
- **vs cmux:** cmux is a full macOS-native terminal with a clean socket, but it's **medium-confidence** in the findings, has no per-pane close (workspace-per-generation workaround), no agent-launch or worktree command, and a **weak** mobile story. Its one edge — git-worktree-per-agent isolation — isn't actually first-class in the manaflow-ai build (that's the *different* craigsc/cmux bash project).
- **vs going pure-headless (no substrate panes):** You could skip panes entirely and relay via chained `claude -p --resume` calls in a shell loop — simplest and most portable, but you lose the visible side-by-side panes and the human-takeover affordance. Good fallback if pane orchestration proves brittle.

## Open questions / risks

- **Unattended-permissions risk.** `--permission-mode auto` aborts after 3 consecutive / 20 total classifier blocks (no human to approve) — design relay tasks to stay inside classifier bounds. `bypassPermissions`/`--dangerously-skip-permissions` removes *all* checks and refuses to run as root — containers/VMs only; do not use on the host Mac for an unattended loop. `auto` also requires Anthropic API + Opus/Sonnet 4.6 (not Bedrock/Vertex). Pair with `--max-turns` and `--max-budget-usd` on **every** spawn as cost circuit-breakers.
- **Infinite-relay guard (critical).** Each new session inherits the `PreCompact` hook, so it will spawn *another* session when it fills — unbounded chaining and cost. Mitigation: pass a `RELAY_DEPTH` env var / handoff line, increment per generation, and have `on-precompact.sh` exit early (no spawn) above a cap (e.g. 5). Also reuse `--request-id`-style idempotency so a re-fired hook doesn't double-spawn.
- **Context loss across the boundary.** The handoff is lossy by construction — it's a summary, not the full transcript. `--append-system-prompt-file` injects it as *system* context (not a user turn). Two specific hazards: (1) `--bare` skips `CLAUDE.md`, hooks, and MCP servers, so a `--bare` relay won't load this repo's conventions or Linear MCP — don't use `--bare` for the seeded session unless you re-inject context explicitly; (2) the handoff must carry forward the `OLD_PANE_ID`/`OLD_WS` and `RELAY_DEPTH` or the close + guard steps break.
- **Race in handoff generation.** Resuming the *same* session that's mid-compaction to write its own handoff is racy; summarizing `transcript_path` in a separate `--bare -p` call is safer.
- **Low-confidence / unverified items flagged by the research:**
  - **cmux overall: medium confidence**; `read-screen`/`new-pane`/`send --surface-id` forms are *unverified* — only `send-surface`/`focus-surface`/`send-key-surface` confirmed.
  - **herdr socket method behind `pane run` is not pinned** in the docs (likely `pane.send_input`); pane-list field names should be confirmed against your build. The pane-level `wait agent-status` documents a `done` state the agent-level `agent wait` does not — treat that distinction as lightly-documented.
  - **Warp:** no grounded socket/CLI for read/wait/close — those capabilities are asserted-absent here, not researched-present. If Warp has added a CLI since, re-verify before relying on it.
  - **No mobile-native control surface** is documented for any substrate; all mobile paths reduce to SSH. Confirm before promising "phone-native" UX.

**First thing to verify before building (herdr):** the installed herdr build's `pane list` JSON shape and the `wait agent-status` semantics — the whole close-the-old-pane step depends on resolving the right pane ID at the right moment. herdr is not yet installed on this machine.

## Provenance

Synthesized from a 6-agent research fan-out (Claude Code headless/resume modes, hooks, the local `handoff` skill, herdr API, cmux CLI). Commands reflect the research's confidence levels — high-confidence for the Claude Code flags, medium for herdr/cmux substrate syntax. Re-verify substrate commands against installed versions before relying on them.
