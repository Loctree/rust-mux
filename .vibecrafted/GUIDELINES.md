# rust-mux — VetCoders GUIDELINES

> Per-repo, agent-agnostic instructions. Same rules for Claude, Codex, Gemini, Junie, Qwen.
> Global doctrine in `~/.claude/CLAUDE.md` (and equivalents) still applies; this file
> overrides/extends only where this repo has its own contract.

## Identity

- **Crate:** `rust-mux` v0.4.0 · edition 2024 · MIT OR Apache-2.0
- **Org:** github.com/Loctree/rust-mux
- **Role:** library-first MCP transport multiplexer (`run_mux_server` / `spawn_mux_server` / `MuxHandle`) plus two CLI binaries (`rust-mux`, `rust-mux-proxy`).
- **Active runtime line:** `src/runtime/` (modular). The `src/runtime.rs` monolith is gone (commit `2638441`). Do **not** reintroduce it. `mux-000 runtime duplication` is resolved at HEAD; if a plan still references `src/runtime.rs`, the plan is stale.
- **Active wizard line:** `src/wizard/` (modular). `src/wizard_legacy.rs` and `src/runtime_legacy.rs` are deleted (commit `4b896b3`). Same rule.

## Canonical command surface

Use the `Makefile`. Do **not** invent equivalent ad-hoc cargo invocations; the operator and CI key off these names.

| Target | Meaning |
|---|---|
| `make gates` | **Required green before any commit:** `fmt-check + clippy + test` (all features). |
| `make test-full` | `gates` + `--ignored` transport tests (`mux_transport_roundtrip_with_loctree_mcp` needs local `loctree-mcp` running). |
| `make check` | `cargo check --all-targets --all-features` |
| `make wizard` / `make wizard-dry-run` | Three-step TUI: services → clients → save (with safe vs `[DANGER]` paths). |
| `make run` / `make run-tray` | Run mux daemon for `SERVICE` from `CONFIG` (default `~/.codex/mcp-mux.toml`). |
| `make proxy` | Run `rust-mux-proxy` against `SOCKET`. |
| `make health` / `make daemon-status` / `make dashboard` | Single-service health, multi-service daemon status, tray dashboard. |

Override variables on the command line: `make run SERVICE=brave-search CONFIG=~/.codex/mcp.json`.

CI mirrors `make gates` (`.github/workflows/ci.yml`) with `--no-default-features` so tray drops out cleanly. Anything you add must keep CI green in both feature configurations.

## Quality gates — non-negotiable

Before every commit:

```bash
make gates              # fmt-check + clippy -D warnings + test --all-features
```

For changes touching `runtime/` transport, also run `make test-full` locally (requires `loctree-mcp` binary on PATH).

Suppression policy: do **not** add new `#[allow(...)]`, `// nosemgrep`, `clippy::*` allows, or `#[cfg(test)] dead_code` islands without a one-line `Why:` comment naming the constraint. Existing forgotten silencers are fair game for `vc-prune`.

## Living Tree convention

This repo runs **parallel agents on a shared worktree** (Claude, Codex, Gemini all touching `src/` simultaneously). Concurrent edits are the rule, not the exception.

- Re-read files before editing if more than a few minutes passed since last `slice` / `Read`.
- Before any non-trivial edit window, check `loctree-mcp doctor()` (or atlas `receipt`) to detect a concurrent rescan; if fingerprint moved, call `context(fresh: true)` again.
- Do **not** revert another agent's change unless explicitly asked; if it conflicts with your work, reconcile or report — never silently overwrite.
- Commit in 5–6 file packs when shape allows. `git commit --only <paths>` is **not atomic** vs. parallel commits — your message can land under another agent's envelope. Don't fight it; document in the report and move on.
- `~/.vibecrafted/artifacts/Loctree/rust-mux/<YYYY_MMDD>/{plans,reports}` is auto-rotated daily; the in-repo `.vibecrafted/{plans,reports}` are symlinks to today's directory. Symlink target moving on a date roll is **not** a code change — leave the diff if it appears in `git status`.
- `.vibecrafted/tmp/` is scratch. Anything under `.vibecrafted/` is gitignored (per `/.vibecrafted` rule).

## Structural map (for planning, not for memorization)

Always run `loctree-mcp slice <file>` before editing a hub. Current top hubs (importer counts, drift fast):

| File | Importers | Role |
|---|---|---|
| `src/config.rs` | 13 | `MuxConfig`, `ServerConfig`, `ResolvedParams`, `CliOptions` trait |
| `src/state.rs` | 11 | `MuxState`, `StatusSnapshot`, error/id helpers |
| `src/scan.rs` | 9 | Host discovery, rewire (1388 LOC — split candidate when touched) |
| `src/multi.rs` | 6 | Multi-service supervisor |
| `src/runtime/mod.rs` | 5 | `run_mux`, `run_mux_internal`, entrypoints |

Known structural debt — **triage, don't chop:**

- **`src/common.rs` is a half-finished extraction.** Twins exist in `scan.rs` for `HostKind`, `HostFormat`, `as_label`, `display_name`. The `common.rs` copies have 0 importers; the `scan.rs` copies are live. Consolidation is desirable, but `common.rs` is a forgotten gem — preserve the intent, surface it as a `vc-prune`/`vc-marbles` finding, do not delete in passing.
- **`print_status_table`, `DaemonStatus`, `check_health`, `HealthStatus` twins** between `lib.rs` ↔ `runtime/status.rs` and `wizard/types.rs` ↔ `state.rs`. Same triage rule.
- **One cycle:** `src/multi.rs ↔ src/state.rs` (length 2). Acceptable while the supervisor needs `MuxState`; flag if you add a third hop.

## Wizard / config doctrine

Two parallel paths in the wizard, do **not** merge them:

- **Safe path → `src/mux_gen.rs`.** Emits to `~/.config/mux/{mcp.json, mcp.toml, config.toml}` with dedup + conflict detection. Never touches host-side AI client configs. This is the default and the recommended onboarding flow.
- **`[DANGER]` path → `src/danger.rs`.** Backup-first, preview-first JSON/TOML rewrite of `.claude/`, `.codex/`, `.gemini/`, `.junie/` host configs with rollback. Always create `.bak`, always offer `--dry-run`, always show the diff before write.

Never silently rewrite a host config from the safe path. Never skip backup from the danger path.

Reference: `docs/WIZARD.md`, `docs/vc-agents-client-discovery-plan.md`.

## API surface (library users)

The public library surface is what's re-exported from `src/lib.rs`. Keep it stable across patch versions:

```rust
use rust_mux::{
    MuxConfig, MuxHandle, ResolvedParams, CliOptions,
    run_mux_server, spawn_mux_server, run_mux_with_shutdown, check_health,
};
```

Internal modules (`runtime::client`, `runtime::server`, `wizard::services`, ...) are private; if a consumer needs a symbol from there, promote it through `lib.rs` deliberately — do not leak the path.

Feature gating:
- default: `["tray", "cli"]`
- `cli` → wizard, scan, binaries (clap, ratatui, crossterm, tracing-subscriber)
- `tray` → tray-icon + image (GUI deps; CI builds with `--no-default-features` to keep headless green)
- Library-only consumers should depend with `default-features = false`.

## Paths (v0.4.0 canonical)

- Sockets: `~/.rmcp-servers/rust-mux/sockets/<service>.sock`
- Status files: `~/.rust-mux/status/<service>.json` (per-service) or `~/.rmcp-servers/rust-mux/status.json` (combined)
- Mux config (safe wizard): `~/.config/mux/{mcp.json, mcp.toml, config.toml}`
- Default service config: `~/.codex/mcp-mux.toml`
- Detection still matches **legacy `rmcp_mux` patterns** (transition compat); do not strip the legacy regex without a release note.

## .env hygiene

Repo currently ships zero `.env*` files. If you ever need one locally:
- Add the exact filename to `.gitignore` (the existing `/.vibecrafted` + `/*.py` rules are intentional; widening to `.env*` is a normal addition).
- Never commit even an example with real secrets.
- Pre-commit blocks accidental `.env` commits; if a hook fires, fix the cause, don't bypass.

## Commit convention

- Title prefix: `[<agent>/<workflow>] <description>` (e.g. `[claude/vc-implement] add heartbeat config plumbing`).
- Body: bulleted list of changes if non-trivial. Empty bodies are not allowed for multi-file packs.
- **AGENT FAIRNESS.** Trailer authorship goes to the agent that **actually wrote the code**:
  ```
  Authored-By: <agent> <agents@vetcoders.io>
  ```
  where `<agent>` ∈ {`claude`, `codex`, `gemini`, `junie`, `qwen`}. Multi-agent collaboration → multiple `Authored-By` lines.
- **Forbidden trailers in this repo:**
  - `Co-Authored-By: Claude Opus … <noreply@anthropic.com>` and any vendor-default footer.
  - `Co-Authored-By: Maciej/Klaudiusz/<personal handle>` — no personal sigs in commits.
  - Coordinator agents do **not** add their signature to other agents' work.
- Footer ends with the canonical brand line **only when a sigblock is needed** (file headers, release notes, public artifacts):
  ```
  𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by VetCoders (c)2024-2026 LibraxisAI
  ```
  Note the trailing dot after `𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍`, the `2024-2026` range, and `LibraxisAI` (not `VetCoders`).

`Cargo.toml` `authors = [...]` is package metadata (PEP 621 / npm-style), not a sig — leave it alone.

## Init discipline

Every session on this repo starts with:
1. Read `~/.claude/Klaudiusz/kronika_*.md` (or equivalent agent chronicle).
2. `/vc-init` (no-op): perception via `loctree-mcp context()`, intentions via `aicx_intents project=rust-mux`, ground truth via `repo-full`.
3. **Then** touch code.

Skipping init in this repo is how parallel agents step on each other. Don't.

## Anti-patterns specific to this repo

- Reintroducing `src/runtime.rs` monolith because a plan referenced it. The plan is stale.
- Reintroducing `src/runtime_legacy.rs` / `src/wizard_legacy.rs`. They are deleted on purpose.
- Silently dropping the `legacy rmcp_mux` detection regex.
- Merging `src/mux_gen.rs` and `src/danger.rs` into one "wizard config writer" — the safe/danger split is the security model.
- Bumping `Cargo.toml` version without a matching `CHANGELOG.md` section.
- Editing `AI_README.md` without also re-running structure check (`loctree-mcp focus src/`) — it lies the moment the tree changes.
- Using `socat` instead of `rust-mux-proxy` for host STDIO bridges in examples.
- Adding `#[allow(dead_code)]` to silence a clippy after a refactor instead of either consolidating the twin or surfacing it as a finding.

## Notes for AI agents

- Comments and docstrings: English. Polish is reserved for operator-facing reports under `.vibecrafted/reports/`.
- `.ai-agents/**` and `*.txt` are scratch — do not commit.
- `AGENTS.md` (root, if it appears) is deprecated; ignore it. This file is the canonical per-repo source.
- When you find drift between this file and the code, **the code wins** — open a follow-up to update GUIDELINES, do not bend the code to match stale guidelines.

---

_𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by VetCoders (c)2024-2026 LibraxisAI_
