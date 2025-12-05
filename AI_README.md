# rmcp_mux – AI-facing Overview

> **Version:** 0.2.1  
> **Last updated:** 2025-11-27

This document provides a concise technical overview for AI agents working with the rmcp_mux codebase.

## Purpose

Share a single MCP server process (e.g., `npx @modelcontextprotocol/server-memory`) across many MCP hosts via a Unix socket. The mux handles:
- JSON-RPC ID rewriting per client
- `initialize` request caching and fan-out
- Request limits, timeouts, and size guards
- Child process restart with exponential backoff
- Status snapshots for UI/automation

## Quick Start

```bash
# Build
cargo build --release

# Run mux daemon
./target/release/rmcp_mux \
  --socket ~/.rmcp_servers/rmcp_mux/sockets/memory.sock \
  --cmd npx -- @modelcontextprotocol/server-memory \
  --max-active-clients 5 \
  --status-file ~/.rmcp_servers/rmcp_mux/status.json

# Host side: use bundled proxy
rmcp_mux_proxy --socket ~/.rmcp_servers/rmcp_mux/sockets/memory.sock
```

## Project Structure (v0.2.1)

```
src/
├── main.rs              # CLI entry, subcommand dispatch
├── config.rs            # Config, ServerConfig, ResolvedParams, load_config
├── state.rs             # MuxState, StatusSnapshot, error_response, set_id
├── scan.rs              # Host discovery, rewiring (HostKind, discover_hosts)
├── tray.rs              # Tray icon (feature-gated: #[cfg(feature = "tray")])
├── bin/
│   └── rmcp_mux_proxy.rs    # Standalone STDIO↔socket proxy
├── runtime/             # Mux daemon core
│   ├── mod.rs           # run_mux, health_check, reap_timeouts
│   ├── types.rs         # ServerEvent, MAX_QUEUE, MAX_PENDING
│   ├── client.rs        # handle_client, handle_client_message
│   ├── server.rs        # server_manager, handle_server_events
│   ├── proxy.rs         # run_proxy (STDIO)
│   ├── status.rs        # write_status_file, spawn_status_writer
│   └── tests.rs         # All runtime tests (753 LOC)
└── wizard/              # Three-step TUI wizard
    ├── mod.rs           # run_wizard, run_tui
    ├── types.rs         # WizardStep, ServiceEntry, ClientEntry, FormState
    ├── services.rs      # load_all_services, detect_running_mcp_servers
    ├── clients.rs       # detect_clients
    ├── ui.rs            # draw_ui, draw_service_list, draw_client_list
    ├── keys.rs          # handle_key, sync_form_to_service
    └── persist.rs       # persist_all, rewire_selected_clients
```

## CLI Subcommands

| Command | Purpose |
|---------|---------|
| (default) | Run mux daemon |
| `wizard` | Three-step TUI: servers → clients → save |
| `scan` | Discover hosts, generate manifest/snippets |
| `rewire` | Update host config to use proxy |
| `status` | Check if host is rewired |
| `health` | Verify socket reachability |
| `proxy` | STDIO↔socket proxy |

## Config (JSON/YAML/TOML)

Default path: `~/.codex/mcp.json` (override `--config`, pick `--service` key under `servers.<name>`).

**Fields per service:**
- `socket`, `cmd`, `args` – required
- `max_active_clients` – default 5
- `lazy_start` – default false
- `max_request_bytes` – default 1_048_576
- `request_timeout_ms` – default 30_000
- `restart_backoff_ms` – default 1_000
- `restart_backoff_max_ms` – default 30_000
- `max_restarts` – default 5 (0 = unlimited)
- `tray`, `service_name`, `log_level`
- `status_file` – JSON snapshots for UI/automation

## Three-Step Wizard

```bash
rmcp_mux wizard --config ~/.codex/mcp-mux.toml
```

1. **Server Detection** – scans `ps` for MCP processes, loads config, toggles with `Space`
2. **Client Detection** – finds Codex/Cursor/VSCode/Claude/JetBrains configs, shows rewire status
3. **Confirmation** – save options: Save All, Mux Only, Clipboard, Back, Exit

Navigation: `n` next, `p` previous, `Space` toggle, `Tab` switch panel, `q` quit.

## Status Snapshots

Written atomically to `status_file` on every state change:
```json
{
  "service_name": "memory",
  "server_status": "Running",
  "restarts": 0,
  "connected_clients": 2,
  "active_clients": 1,
  "pending_requests": 0,
  "queue_depth": 0,
  "child_pid": 12345,
  "cached_initialize": true
}
```

## Testing

```bash
# Full suite (34 tests)
cargo test

# Without tray feature (CI/headless)
cargo test --no-default-features

# Linting
cargo clippy --all-targets --all-features -- -D warnings

# Coverage
cargo tarpaulin --all-targets --no-default-features --out Lcov
```

## Key Symbols for Navigation

| Symbol | Location | Purpose |
|--------|----------|---------|
| `ResolvedParams` | config.rs | Merged CLI + config parameters |
| `MuxState` | state.rs | Runtime state (clients, pending, cache) |
| `StatusSnapshot` | state.rs | JSON status output |
| `run_mux` | runtime/mod.rs | Main mux loop |
| `server_manager` | runtime/server.rs | Child process lifecycle |
| `handle_client` | runtime/client.rs | Client connection handler |
| `run_wizard` | wizard/mod.rs | TUI entry point |
| `WizardStep` | wizard/types.rs | Step enum (Server/Client/Confirmation) |
| `discover_hosts` | scan.rs | Find host config files |

## Notes for AI Agents

1. **Feature gating:** Tray code uses `#[cfg(feature = "tray")]`. Build with `--no-default-features` for headless.

2. **Single child model:** One MCP server per socket. Don't introduce multi-child patterns.

3. **Initialize caching:** First `initialize` is cached in `MuxState.cached_initialize`. Later clients get cached response via `init_waiting`.

4. **Error handling:** Use `anyhow::Result` and `.with_context()` for all fallible operations.

5. **Tests:** Colocated in each module as `#[cfg(test)] mod tests`. Use `tempfile::tempdir()` for filesystem tests.

6. **Workspace:** `.ai-agents/` is AI scratch space. Keep helper files there, document in `AI_GUIDELINES.md`.

7. **Code style:** 
   - Imports: std → external crates → crate-local
   - English comments only
   - Run `cargo fmt` before committing

## CI Workflow

`.github/workflows/ci.yml`:
- `cargo fmt --check`
- `cargo clippy --all-targets --no-default-features -- -D warnings`
- `cargo test --no-default-features`
- `cargo tarpaulin` (coverage)

## See Also

- [README.md](README.md) – User documentation
- [CHANGELOG.md](CHANGELOG.md) – Version history
- [.ai-agents/AI_GUIDELINES.md](.ai-agents/AI_GUIDELINES.md) – Detailed development guidelines
