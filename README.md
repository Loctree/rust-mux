# rmcp_mux – MCP Server Multiplexer

[![CI](https://github.com/Loctree/rmcp-mux/actions/workflows/ci.yml/badge.svg)](https://github.com/Loctree/rmcp-mux/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/rmcp_mux.svg)](https://crates.io/crates/rmcp_mux)
[![Version](https://img.shields.io/badge/version-0.3.0-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A Rust library and daemon that lets many MCP clients reuse a single STDIO server process (e.g. `npx @modelcontextprotocol/server-memory`) over a Unix socket. It rewrites JSON-RPC IDs per client, caches `initialize`, restarts the child on failure, and cleans up the socket on exit.

**NEW in 0.3.0**: Now available as an embeddable library! Integrate MCP multiplexing directly into your Rust application.

## Table of Contents
- [Features](#features)
- [Library Usage](#library-usage) ⭐ NEW
- [Quick Start (CLI)](#quick-start-cli)
- [Installation](#installation)
- [Configuration](#configuration)
- [Interactive Wizard (TUI)](#interactive-wizard-tui)
- [Subcommands](#subcommands)
- [Runtime Behavior](#runtime-behavior)
- [Project Structure](#project-structure)
- [Testing](#testing)
- [Contributing](#contributing)

## Features

### Core
- **One child process per service** – spawned from `--cmd ...`
- **Multiple clients via Unix socket** – ID rewriting keeps responses matched to the right client
- **Initialize caching** – executed once; later clients get cached response immediately
- **Concurrent requests** – active client slots limited by `--max-active-clients` (default 5)
- **Notification broadcasting** – notifications sent to all connected clients
- **Auto-restart** – child restarts on exit; pending/waiting requests receive error on reset
- **Graceful shutdown** – Ctrl+C stops mux, kills child, removes socket file

### Monitoring & Status
- **JSON status snapshots** (`--status-file`) – PID, restarts, queue depth for automation
- **Optional tray indicator** (`--tray`) – live server status, client/pending counts, restart reason
- **Health check subcommand** – verify socket reachability

### Configuration
- **Multi-format configs** – JSON, YAML, TOML with auto-detection
- **Three-step wizard** – guided TUI for server detection, client rewiring, and config generation
- **Host scanning** – auto-detect MCP configs in Codex, Cursor, VSCode, Claude, JetBrains

## Library Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
rmcp_mux = { version = "0.3", default-features = false }
```

### Basic Example - Single Mux Server

```rust
use rmcp_mux::{MuxConfig, run_mux_server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MuxConfig::new("/tmp/my-mcp.sock", "npx")
        .with_args(vec!["-y".into(), "@anthropic/mcp-server".into()])
        .with_max_clients(10)
        .with_service_name("my-mcp-server");

    run_mux_server(config).await
}
```

### Multiple Mux Instances (Single Process)

Perfect for tools like **loctree** that need to run multiple MCP services:

```rust
use rmcp_mux::{MuxConfig, spawn_mux_server, MuxHandle};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Define your MCP services
    let services = vec![
        ("memory", "/tmp/mcp-memory.sock", "npx", vec!["@mcp/server-memory"]),
        ("filesystem", "/tmp/mcp-fs.sock", "npx", vec!["@mcp/server-filesystem"]),
        ("brave-search", "/tmp/mcp-brave.sock", "npx", vec!["@mcp/server-brave"]),
    ];

    // Spawn all services in a single process
    let mut handles: Vec<MuxHandle> = Vec::new();
    for (name, socket, cmd, args) in services {
        let config = MuxConfig::new(socket, cmd)
            .with_args(args.into_iter().map(String::from).collect())
            .with_service_name(name)
            .with_request_timeout(Duration::from_secs(60));

        handles.push(spawn_mux_server(config).await?);
    }

    println!("Running {} MCP services in single process", handles.len());

    // Wait for all to complete (or shutdown signal)
    for handle in handles {
        handle.wait().await?;
    }
    Ok(())
}
```

### Programmatic Shutdown

```rust
use rmcp_mux::{MuxConfig, spawn_mux_server};
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let handle = spawn_mux_server(
        MuxConfig::new("/tmp/mcp.sock", "my-server")
    ).await?;

    // Run for 60 seconds, then shutdown
    sleep(Duration::from_secs(60)).await;
    handle.shutdown();
    handle.wait().await?;

    Ok(())
}
```

### With External CancellationToken

For advanced integration with your own shutdown logic:

```rust
use rmcp_mux::{MuxConfig, ResolvedParams, run_mux_with_shutdown};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    // Your own shutdown logic
    tokio::spawn(async move {
        // Wait for your application's shutdown signal
        // ...
        shutdown_clone.cancel();
    });

    let config = MuxConfig::new("/tmp/mcp.sock", "my-server");
    run_mux_with_shutdown(config.into(), shutdown).await
}
```

### Health Check

```rust
use rmcp_mux::check_health;

async fn verify_service() -> bool {
    check_health("/tmp/mcp-memory.sock").await.is_ok()
}
```

### Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `cli` | ✓ | CLI binary, wizard, scan commands |
| `tray` | ✓ | System tray icon support |

For library-only usage (minimal dependencies):

```toml
[dependencies]
rmcp_mux = { version = "0.3", default-features = false }
```

## Quick Start (CLI)

```bash
# Build
cargo build --release

# Run with memory server
./target/release/rmcp_mux \
  --socket /tmp/mcp-memory.sock \
  --cmd npx -- @modelcontextprotocol/server-memory \
  --max-active-clients 5

# Connect via proxy (for MCP hosts expecting STDIO)
rmcp_mux_proxy --socket /tmp/mcp-memory.sock
```

## Installation

### From source
```bash
cargo build --release
# Binaries: target/release/rmcp_mux, target/release/rmcp_mux_proxy
```

### One-liner (curl | sh)
```bash
curl -fsSL https://raw.githubusercontent.com/Loctree/rmcp-mux/main/tools/install.sh | sh
```

**Environment overrides:**
- `INSTALL_DIR` – wrapper location (default: `$HOME/.local/bin`)
- `CARGO_HOME` – cargo home (default: `~/.cargo`)
- `MUX_REF` – branch/tag/commit (default: `main`)
- `MUX_NO_LOCK=1` – skip `--locked` flag

### Built-in proxy
If your MCP host needs a STDIO command, use the bundled proxy instead of `socat`:
```bash
rmcp_mux_proxy --socket /tmp/mcp-memory.sock
```

## Configuration

### Config file formats
Default path: `~/.codex/mcp.json` (override with `--config <path>`). Parser auto-detects by extension.

**JSON:**
```json
{
  "servers": {
    "general-memory": {
      "socket": "~/mcp-sockets/general-memory.sock",
      "cmd": "npx",
      "args": ["@modelcontextprotocol/server-memory"],
      "max_active_clients": 5,
      "max_request_bytes": 1048576,
      "request_timeout_ms": 30000,
      "restart_backoff_ms": 1000,
      "restart_backoff_max_ms": 30000,
      "max_restarts": 5,
      "status_file": "~/.rmcp_servers/rmcp_mux/status.json",
      "lazy_start": false,
      "tray": true,
      "service_name": "general-memory"
    }
  }
}
```

**YAML:**
```yaml
servers:
  general-memory:
    socket: "~/mcp-sockets/general-memory.sock"
    cmd: "npx"
    args: ["@modelcontextprotocol/server-memory"]
    max_active_clients: 5
    tray: true
```

**TOML:**
```toml
[servers.general-memory]
socket = "~/mcp-sockets/general-memory.sock"
cmd = "npx"
args = ["@modelcontextprotocol/server-memory"]
max_active_clients = 5
tray = true
```

### Running with config
```bash
./target/release/rmcp_mux --config ~/.codex/mcp.json --service general-memory
```
CLI flags override config values (e.g. `--socket`, `--cmd`, `--tray`).

### Parameter defaults
| Parameter | Default | Description |
|-----------|---------|-------------|
| `socket` | required | Unix socket path |
| `cmd` | required | MCP server command |
| `args` | `[]` | Arguments for command |
| `max_active_clients` | `5` | Concurrent client limit |
| `lazy_start` | `false` | Defer child spawn until first request |
| `max_request_bytes` | `1048576` | Max request size (1 MiB) |
| `request_timeout_ms` | `30000` | Request timeout (30s) |
| `restart_backoff_ms` | `1000` | Initial restart delay (1s) |
| `restart_backoff_max_ms` | `30000` | Max restart delay (30s) |
| `max_restarts` | `5` | Restart limit (0 = unlimited) |
| `tray` | `false` | Enable tray icon |
| `status_file` | none | Path for JSON status snapshots |

## Interactive Wizard (TUI)

The wizard provides a **three-step guided flow** for configuring rmcp_mux and rewiring MCP clients:

```bash
rmcp_mux wizard --config ~/.codex/mcp-mux.toml
```

### Step 1: Server Detection
- Detects running MCP server processes via `ps` command
- Loads existing services from config file
- Displays servers with selection checkboxes:
  - `[✓]` selected / `[ ]` unselected
  - `[C]` config-based / `[D]` detected process
  - Health status: 🟢 healthy / 🔴 unhealthy / ⚪ unknown

**Controls:**
- `Space` – toggle server selection
- `Tab` – switch to editor panel
- `↑/↓` – navigate list
- `n` – proceed to Step 2

### Step 2: Client Detection
- Discovers MCP client applications:
  - Codex (`~/.codex/config.toml`)
  - Cursor (`~/Library/Application Support/Cursor/...`)
  - VSCode (`~/Library/Application Support/Code/...`)
  - Claude (`~/.config/Claude/claude_config.json`)
  - JetBrains (`~/Library/Application Support/JetBrains/LLM/mcp.json`)
- Shows rewire status: `[rewired]` or `[not rewired]`
- Lists services defined in each client config

**Controls:**
- `Space` – toggle client selection for rewiring
- `n` – proceed to Step 3
- `p` – go back to Step 1

### Step 3: Confirmation
- Displays summary of selected servers and clients
- Save options:
  - **Save All** – save mux config AND rewire selected clients
  - **Mux Only** – save mux config only
  - **Clipboard** – copy config to clipboard (`pbcopy` on macOS)
  - **Back** – return to Step 2
  - **Exit** – exit without saving

**Features:**
- Creates `.bak` backup files for all modified configs
- `--dry-run` mode to preview changes without writing

### Wizard options
```bash
rmcp_mux wizard \
  --config ~/.codex/mcp-mux.toml \
  --service general-memory \
  --dry-run
```

## Subcommands

### `scan` – Discover and generate configs
```bash
# Generate mux manifest and host snippets
rmcp_mux scan \
  --manifest ~/.codex/mcp-mux.toml \
  --snippet ~/.codex/mcp-mux \
  --socket-dir ~/.rmcp_servers/rmcp_mux/sockets
```

### `rewire` – Update host configs
```bash
# Rewire a host config to use rmcp_mux proxy (creates .bak backup)
rmcp_mux rewire --host codex --socket-dir ~/.rmcp_servers/rmcp_mux/sockets

# Preview changes without writing
rmcp_mux rewire --host codex --dry-run
```

### `status` – Check rewire status
```bash
rmcp_mux status --host codex --proxy-cmd rmcp_mux_proxy
```

### `health` – Verify connectivity
```bash
# Direct check
rmcp_mux health --socket /tmp/mcp-memory.sock --cmd npx -- @modelcontextprotocol/server-memory

# Config-based check
rmcp_mux health --config ~/.codex/mcp.json --service general-memory
```

### `proxy` – STDIO proxy
```bash
rmcp_mux proxy --socket /tmp/mcp-memory.sock
```

## Runtime Behavior

### Client handling
1. New client → assigned `client_id`
2. Messages get `global_id = c<client>:<seq>`
3. Responses demuxed back to original client/local ID
4. First `initialize` hits server; response cached and fanned out to waiters
5. Later `initialize` calls answered from cache

### Safety guards
- **Max request size** – default 1 MiB
- **Request timeout** – default 30s with cleanup of pending calls
- **Exponential restart backoff** – 1s → 30s with 5 restart limit
- **Lazy start** – defer child spawn until first request

### Error handling
- Child exit or I/O failure → restart child, clear cache/pending, send errors to affected clients
- Graceful shutdown (Ctrl+C) → stop child, delete socket

## Tray Status (optional)

Run with `--tray` to spawn a status icon showing:
- Service name and server state
- Connected/active clients
- Pending requests
- Initialize cache state
- Restart count and reason

Click "Quit mux" in the tray menu to stop the daemon.

For custom monitoring, write status snapshots:
```bash
rmcp_mux --status-file ~/.rmcp_servers/rmcp_mux/status.json ...
```

## Project Structure

```
rmcp_mux/
├── src/
│   ├── lib.rs            # Library entry point, MuxConfig, public API
│   ├── config.rs         # Config types, CliOptions trait, loading
│   ├── state.rs          # MuxState, StatusSnapshot, helpers
│   ├── scan.rs           # Host discovery and rewiring (cli feature)
│   ├── tray.rs           # Tray icon (tray feature)
│   ├── bin/
│   │   ├── rmcp_mux.rs       # CLI binary (cli feature)
│   │   └── rmcp_mux_proxy.rs # STDIO proxy binary (cli feature)
│   ├── runtime/          # Core mux daemon (always available)
│   │   ├── mod.rs        # run_mux, run_mux_internal, health_check
│   │   ├── types.rs      # ServerEvent, constants
│   │   ├── client.rs     # Client connection handling
│   │   ├── server.rs     # Child process management
│   │   ├── proxy.rs      # STDIO proxy logic
│   │   ├── status.rs     # Status file writing
│   │   └── tests.rs      # Runtime tests
│   └── wizard/           # Interactive TUI wizard (cli feature)
│       ├── mod.rs        # Entry point
│       ├── types.rs      # WizardStep, ServiceEntry, etc.
│       ├── services.rs   # Server detection, health checks
│       ├── clients.rs    # Client detection
│       ├── ui.rs         # Ratatui drawing
│       ├── keys.rs       # Key event handling
│       └── persist.rs    # Config saving, rewiring
├── tools/
│   ├── install.sh        # One-liner installer
│   ├── launchd/          # macOS launchd templates
│   └── githooks/         # Git hooks
├── public/
│   └── rmcp_mux_icon.png # Tray icon
└── .ai-agents/           # AI agent workspace
    └── AI_GUIDELINES.md  # Guidelines for AI agents
```

### Module Visibility

| Module | Feature | Public API |
|--------|---------|------------|
| `runtime` | always | `run_mux`, `run_mux_internal`, `health_check`, `run_proxy` |
| `config` | always | `Config`, `ResolvedParams`, `MuxConfig`, `CliOptions` |
| `state` | always | `MuxState`, `StatusSnapshot`, `ServerStatus` |
| `scan` | cli | `run_scan_cmd`, `run_rewire_cmd`, `run_status_cmd` |
| `wizard` | cli | `run_wizard`, `WizardArgs` |
| `tray` | tray | Internal (started via `MuxConfig::with_tray(true)`) |

## Testing

```bash
# Run all tests
cargo test

# Run tests without tray feature (for CI/headless)
cargo test --no-default-features

# Linting
cargo clippy --all-targets --all-features

# Coverage
cargo tarpaulin --all-targets --timeout 120
```

**Test coverage includes:**
- ID rewriting and error responses
- Initialize caching and fan-out
- Reset state broadcasting
- Config loading (JSON/YAML/TOML)
- Parameter resolution and defaults
- Health checks
- Status file writing
- Proxy forwarding

## launchd (macOS)

Template at `tools/launchd/rmcp_mux.sample.plist`:
```bash
cp tools/launchd/rmcp_mux.sample.plist ~/Library/LaunchAgents/rmcp_mux.general-memory.plist
# Edit paths and user
launchctl load -w ~/Library/LaunchAgents/rmcp_mux.general-memory.plist
```

## Dependency Notes

- `ratatui` + `crossterm` – TUI wizard (pure Rust)
- `tray-icon` + `image` – optional tray feature
- `tokio` – async runtime
- `rmcp` – JSON-RPC message codec
- `tempfile` – dev-only for test fixtures

Build without optional deps:
```bash
cargo build --no-default-features
```

## Contributing

See [.ai-agents/AI_GUIDELINES.md](.ai-agents/AI_GUIDELINES.md) for development guidelines.

## License

MIT
