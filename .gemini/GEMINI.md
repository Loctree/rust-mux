# rmcp-mux Project Context

## Project Overview

**rmcp-mux** is a Rust library and CLI tool for multiplexing MCP (Model Context Protocol) servers. It allows a single server process to serve multiple clients via Unix sockets, providing features like initialize caching, request ID rewriting, and automatic restarts.

## Key Features

- **Single server, multiple clients**: One MCP server child process serves many clients
- **Initialize caching**: First initialize response is cached for subsequent clients
- **Request ID rewriting**: Transparent request routing with ID collision avoidance
- **Automatic restarts**: Exponential backoff restart of failed server processes
- **Active client limiting**: Semaphore-based concurrency control
- **System tray support**: Optional tray icon with status display (feature-gated)

## Project Structure

```
src/
├── lib.rs           # Public API: MuxConfig, run_mux_server, spawn_mux_server
├── config.rs        # Configuration loading (YAML/TOML/JSON)
├── protocol.rs      # JSON-RPC message handling
├── state.rs         # Shared mux state management
├── tray.rs          # System tray UI (optional, feature = "tray")
├── runtime/         # Core runtime modules
│   ├── mod.rs       # Main mux loop orchestration
│   ├── client.rs    # Client connection handling
│   ├── server.rs    # Server process management
│   ├── proxy.rs     # Request/response proxying
│   └── status.rs    # Status broadcasting
├── wizard/          # CLI wizard for configuration (feature = "cli")
│   ├── mod.rs       # Wizard entry point
│   ├── ui.rs        # TUI components (ratatui)
│   ├── clients.rs   # Client detection
│   ├── keys.rs      # API key validation
│   ├── persist.rs   # Config file generation
│   └── types.rs     # Wizard data types
└── bin/
    ├── rmcp_mux.rs       # Main CLI binary
    └── rmcp_mux_proxy.rs # Proxy binary
```

## Building & Testing

```bash
# Build library only
cargo build --no-default-features

# Build with CLI and tray support
cargo build --all-features

# Run tests
cargo test --no-default-features

# Linting
cargo clippy --all-targets --no-default-features -- -D warnings

# Formatting
cargo fmt --check
```

## Library Usage

```rust
use rmcp_mux::{MuxConfig, run_mux_server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MuxConfig::new("/tmp/my-mcp.sock", "npx")
        .with_args(vec!["-y".into(), "@anthropic/mcp-server".into()])
        .with_max_clients(10);

    run_mux_server(config).await
}
```

## Conventions & Style

- **Rust Edition**: 2024
- **Formatting**: Standard `cargo fmt`
- **Linting**: Strict clippy with `-D warnings`
- **Features**: `cli` (wizard, TUI), `tray` (system tray)
- **Dependencies**: `rmcp` for MCP protocol, `tokio` for async runtime
- **Security**: Path sanitization for config file operations (semgrep-clean)

## Review Focus Areas

When reviewing PRs, pay attention to:
1. **Async correctness**: Proper use of tokio primitives
2. **Error handling**: Use of `anyhow::Result` with context
3. **Feature gates**: Correct `#[cfg(feature = "...")]` usage
4. **Security**: No path traversal, safe file operations
5. **API stability**: Public API changes in lib.rs
