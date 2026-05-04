//! Safe-path mux config generation.
//!
//! Given a set of discovered MCP services from various client configs, produce
//! the three rust-mux-owned files clients can opt into without us having to
//! mutate their own configs:
//!
//! - `~/.config/mux/config.toml` — daemon truth: which upstream MCP servers
//!   `rust-mux` should run, with their original `command`/`args`/`env`.
//! - `~/.config/mux/mcp.json` — client-facing JSON. Every server entry's
//!   `command` is `rust-mux-proxy` (clients launch the proxy, the proxy
//!   talks to the running mux).
//! - `~/.config/mux/mcp.toml` — client-facing TOML mirror for Codex-style
//!   clients or for users who prefer to merge the snippet manually.
//!
//! This is the **safe** flow: nothing in the user's existing client configs
//! is touched. The wizard then prints precise per-client commands the user
//! can run to opt into the generated mux config.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{Config, ServerConfig, expand_path};
use crate::scan::{ConflictReport, HostKind, HostService, MergeOutcome};

/// Default mux directory, expanded from `~/.config/mux`.
pub fn default_mux_dir() -> PathBuf {
    expand_path("~/.config/mux")
}

/// Default per-server socket directory under the mux dir.
pub fn default_socket_dir(mux_dir: &Path) -> PathBuf {
    mux_dir.join("sockets")
}

/// All artifacts produced by the safe-path generator. Returned both for
/// preview (string contents) and for actual on-disk writes.
#[derive(Debug, Clone, Serialize)]
pub struct MuxOutputs {
    pub mux_dir: PathBuf,
    pub socket_dir: PathBuf,
    pub config_toml_path: PathBuf,
    pub mcp_json_path: PathBuf,
    pub mcp_toml_path: PathBuf,
    pub config_toml: String,
    pub mcp_json: String,
    pub mcp_toml: String,
    pub services: Vec<HostService>,
    pub conflicts: Vec<ConflictReport>,
}

/// File handles produced after writing [`MuxOutputs`] to disk.
#[derive(Debug, Clone, Serialize)]
pub struct MuxFiles {
    pub config_toml_path: PathBuf,
    pub mcp_json_path: PathBuf,
    pub mcp_toml_path: PathBuf,
}

/// Build the three mux outputs from a merge result. The proxy command and
/// optional pre-args (e.g. `["proxy"]`) are spliced into every client-facing
/// server entry as `<proxy_cmd> [proxy_args...] --socket <path>`.
pub fn build_mux_outputs(
    merge: &MergeOutcome,
    mux_dir: &Path,
    proxy_cmd: &str,
    proxy_args: &[String],
) -> Result<MuxOutputs> {
    let socket_dir = default_socket_dir(mux_dir);

    let daemon_cfg = build_daemon_config(&merge.services, &socket_dir);
    let config_toml =
        toml::to_string_pretty(&daemon_cfg).context("serialize daemon config.toml")?;

    let client_json = build_client_json(&merge.services, &socket_dir, proxy_cmd, proxy_args);
    let mcp_json = serde_json::to_string_pretty(&client_json).context("serialize mcp.json")?;

    let client_toml = build_client_toml(&merge.services, &socket_dir, proxy_cmd, proxy_args);
    let mcp_toml = toml::to_string_pretty(&client_toml).context("serialize mcp.toml")?;

    Ok(MuxOutputs {
        mux_dir: mux_dir.to_path_buf(),
        socket_dir,
        config_toml_path: mux_dir.join("config.toml"),
        mcp_json_path: mux_dir.join("mcp.json"),
        mcp_toml_path: mux_dir.join("mcp.toml"),
        config_toml,
        mcp_json,
        mcp_toml,
        services: merge.services.clone(),
        conflicts: merge.conflicts.clone(),
    })
}

/// Write the three mux outputs to disk, creating the parent directory if
/// needed. Existing files are replaced; the safe path is rust-mux-owned, so
/// no backup is required (this directory belongs to us).
pub fn write_mux_outputs(outputs: &MuxOutputs) -> Result<MuxFiles> {
    fs::create_dir_all(&outputs.mux_dir).with_context(|| {
        format!(
            "failed to create mux directory {}",
            outputs.mux_dir.display()
        )
    })?;
    fs::create_dir_all(&outputs.socket_dir).with_context(|| {
        format!(
            "failed to create socket directory {}",
            outputs.socket_dir.display()
        )
    })?;

    fs::write(&outputs.config_toml_path, &outputs.config_toml)
        .with_context(|| format!("failed to write {}", outputs.config_toml_path.display()))?;
    fs::write(&outputs.mcp_json_path, &outputs.mcp_json)
        .with_context(|| format!("failed to write {}", outputs.mcp_json_path.display()))?;
    fs::write(&outputs.mcp_toml_path, &outputs.mcp_toml)
        .with_context(|| format!("failed to write {}", outputs.mcp_toml_path.display()))?;

    Ok(MuxFiles {
        config_toml_path: outputs.config_toml_path.clone(),
        mcp_json_path: outputs.mcp_json_path.clone(),
        mcp_toml_path: outputs.mcp_toml_path.clone(),
    })
}

/// Per-client guidance the wizard prints after the safe path runs.
pub fn safe_path_instructions(outputs: &MuxOutputs) -> Vec<ClientInstruction> {
    let mcp_json = outputs.mcp_json_path.display().to_string();
    let mcp_toml = outputs.mcp_toml_path.display().to_string();

    vec![
        ClientInstruction {
            kind: HostKind::Claude,
            headline: "Claude Code (strict config)".to_string(),
            commands: vec![format!(
                "claude --strict-mcp-config --mcp-config \"{}\"",
                mcp_json
            )],
            note: "Strict mode prevents Claude Code from loading any other MCP config alongside the mux one.".to_string(),
        },
        ClientInstruction {
            kind: HostKind::ClaudeDesktop,
            headline: "Claude Desktop".to_string(),
            commands: vec![format!(
                "Open ~/Library/Application Support/Claude/claude_desktop_config.json and merge the `mcpServers` block from {}",
                mcp_json
            )],
            note: "Claude Desktop has no strict-config CLI flag; merge by hand or use the [DANGER] flow.".to_string(),
        },
        ClientInstruction {
            kind: HostKind::Codex,
            headline: "Codex CLI".to_string(),
            commands: vec![
                "# Codex's `-c/--config` is a key=value override, not a config-file flag.".to_string(),
                "# Either merge the [mcp_servers] block from the file below into ~/.codex/config.toml,".to_string(),
                format!("# or `codex mcp add` each server pointing at rust-mux-proxy. Source: {}", mcp_toml),
            ],
            note: "There is no verified Codex flag that swaps the entire MCP config file; merge or `codex mcp add` is required.".to_string(),
        },
        ClientInstruction {
            kind: HostKind::Junie,
            headline: "Junie".to_string(),
            commands: vec![format!(
                "junie --mcp-location \"{}\"",
                mcp_json
            )],
            note: "`--mcp-default-locations` lets Junie keep its other MCP files alongside the mux one if you prefer additive mode.".to_string(),
        },
        ClientInstruction {
            kind: HostKind::Gemini,
            headline: "Gemini CLI".to_string(),
            commands: gemini_mcp_add_commands(&outputs.services, &outputs.socket_dir),
            note: "No verified Gemini flag for a strict config file; use `gemini mcp add` per server or fall back to the [DANGER] flow.".to_string(),
        },
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInstruction {
    pub kind: HostKind,
    pub headline: String,
    pub commands: Vec<String>,
    pub note: String,
}

fn gemini_mcp_add_commands(services: &[HostService], socket_dir: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(services.len());
    for svc in services {
        let socket = svc.socket.clone().unwrap_or_else(|| {
            socket_dir
                .join(format!("{}.sock", svc.name))
                .to_string_lossy()
                .into_owned()
        });
        out.push(format!(
            "gemini mcp add {} -- rust-mux-proxy --socket {}",
            svc.name, socket
        ));
    }
    if out.is_empty() {
        out.push("# (no services to add)".to_string());
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Builders for the three artifacts
// ─────────────────────────────────────────────────────────────────────────────

fn build_daemon_config(services: &[HostService], socket_dir: &Path) -> Config {
    let mut cfg = Config::default();
    for svc in services {
        let socket = svc.socket.clone().unwrap_or_else(|| {
            socket_dir
                .join(format!("{}.sock", svc.name))
                .to_string_lossy()
                .into_owned()
        });
        cfg.servers.insert(
            svc.name.clone(),
            ServerConfig {
                socket: Some(socket),
                cmd: Some(svc.command.clone()),
                args: Some(svc.args.clone()),
                max_active_clients: Some(5),
                tray: Some(false),
                service_name: Some(svc.name.clone()),
                log_level: Some("info".into()),
                lazy_start: Some(false),
                max_request_bytes: Some(1_048_576),
                request_timeout_ms: Some(30_000),
                restart_backoff_ms: Some(1_000),
                restart_backoff_max_ms: Some(30_000),
                max_restarts: Some(5),
                status_file: None,
                env: svc.env.clone(),
                heartbeat_interval_ms: Some(30_000),
                heartbeat_timeout_ms: Some(30_000),
                heartbeat_max_failures: Some(3),
                heartbeat_enabled: Some(true),
            },
        );
    }
    cfg
}

#[derive(Debug, Serialize)]
struct ClientServerJson {
    command: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct ClientJsonRoot {
    #[serde(rename = "mcpServers")]
    mcp_servers: HashMap<String, ClientServerJson>,
}

fn build_client_json(
    services: &[HostService],
    socket_dir: &Path,
    proxy_cmd: &str,
    proxy_args: &[String],
) -> ClientJsonRoot {
    let mut servers: HashMap<String, ClientServerJson> = HashMap::new();
    for svc in services {
        let socket = svc.socket.clone().unwrap_or_else(|| {
            socket_dir
                .join(format!("{}.sock", svc.name))
                .to_string_lossy()
                .into_owned()
        });
        let mut args: Vec<String> = proxy_args.to_owned();
        args.push("--socket".to_string());
        args.push(socket);
        servers.insert(
            svc.name.clone(),
            ClientServerJson {
                command: proxy_cmd.to_string(),
                args,
                env: svc.env.clone(),
            },
        );
    }
    ClientJsonRoot {
        mcp_servers: servers,
    }
}

#[derive(Debug, Serialize)]
struct ClientServerToml {
    command: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct ClientTomlRoot {
    mcp_servers: HashMap<String, ClientServerToml>,
}

fn build_client_toml(
    services: &[HostService],
    socket_dir: &Path,
    proxy_cmd: &str,
    proxy_args: &[String],
) -> ClientTomlRoot {
    let mut servers: HashMap<String, ClientServerToml> = HashMap::new();
    for svc in services {
        let socket = svc.socket.clone().unwrap_or_else(|| {
            socket_dir
                .join(format!("{}.sock", svc.name))
                .to_string_lossy()
                .into_owned()
        });
        let mut args: Vec<String> = proxy_args.to_owned();
        args.push("--socket".to_string());
        args.push(socket);
        servers.insert(
            svc.name.clone(),
            ClientServerToml {
                command: proxy_cmd.to_string(),
                args,
                env: svc.env.clone(),
            },
        );
    }
    ClientTomlRoot {
        mcp_servers: servers,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::MergeOutcome;
    use tempfile::tempdir;

    fn one_service() -> MergeOutcome {
        MergeOutcome {
            services: vec![HostService {
                name: "memory".into(),
                command: "npx".into(),
                args: vec!["@modelcontextprotocol/server-memory".into()],
                socket: None,
                env: None,
                enabled: None,
            }],
            conflicts: Vec::new(),
        }
    }

    #[test]
    fn build_outputs_carry_socket_paths() {
        let dir = tempdir().expect("tempdir");
        let mux_dir = dir.path().join("mux");
        let merge = one_service();
        let outputs = build_mux_outputs(&merge, &mux_dir, "rust-mux-proxy", &[]).expect("build");

        assert!(outputs.config_toml.contains("npx"));
        // Daemon config keeps the upstream command intact.
        assert!(
            outputs
                .config_toml
                .contains("@modelcontextprotocol/server-memory")
        );
        // Client JSON points clients at rust-mux-proxy.
        assert!(outputs.mcp_json.contains("rust-mux-proxy"));
        assert!(outputs.mcp_json.contains("--socket"));
        // Client TOML uses snake_case `mcp_servers` which Codex understands.
        assert!(outputs.mcp_toml.contains("[mcp_servers."));
        assert!(outputs.mcp_toml.contains("rust-mux-proxy"));
    }

    #[test]
    fn write_outputs_creates_files_in_temp_dir() {
        let dir = tempdir().expect("tempdir");
        let mux_dir = dir.path().join("mux");
        let merge = one_service();
        let outputs = build_mux_outputs(&merge, &mux_dir, "rust-mux-proxy", &[]).expect("build");
        let files = write_mux_outputs(&outputs).expect("write");

        assert!(files.config_toml_path.exists(), "config.toml not written");
        assert!(files.mcp_json_path.exists(), "mcp.json not written");
        assert!(files.mcp_toml_path.exists(), "mcp.toml not written");
        assert!(outputs.socket_dir.exists(), "socket dir not created");
    }

    #[test]
    fn safe_path_instructions_cover_all_clients() {
        let dir = tempdir().expect("tempdir");
        let outputs = build_mux_outputs(
            &one_service(),
            &dir.path().join("mux"),
            "rust-mux-proxy",
            &[],
        )
        .expect("build");
        let kinds: std::collections::HashSet<HostKind> = safe_path_instructions(&outputs)
            .iter()
            .map(|i| i.kind)
            .collect();
        for required in [
            HostKind::Claude,
            HostKind::ClaudeDesktop,
            HostKind::Codex,
            HostKind::Junie,
            HostKind::Gemini,
        ] {
            assert!(
                kinds.contains(&required),
                "missing instruction for {:?}",
                required
            );
        }
    }

    #[test]
    fn instructions_use_strict_flag_for_claude_code() {
        let dir = tempdir().expect("tempdir");
        let outputs = build_mux_outputs(
            &one_service(),
            &dir.path().join("mux"),
            "rust-mux-proxy",
            &[],
        )
        .expect("build");
        let claude = safe_path_instructions(&outputs)
            .into_iter()
            .find(|i| i.kind == HostKind::Claude)
            .expect("claude instructions");
        assert!(
            claude
                .commands
                .iter()
                .any(|c| c.contains("--strict-mcp-config")),
            "claude commands should use --strict-mcp-config: {:?}",
            claude.commands
        );
    }

    #[test]
    fn instructions_for_junie_use_mcp_location() {
        let dir = tempdir().expect("tempdir");
        let outputs = build_mux_outputs(
            &one_service(),
            &dir.path().join("mux"),
            "rust-mux-proxy",
            &[],
        )
        .expect("build");
        let junie = safe_path_instructions(&outputs)
            .into_iter()
            .find(|i| i.kind == HostKind::Junie)
            .expect("junie instructions");
        assert!(
            junie.commands.iter().any(|c| c.contains("--mcp-location")),
            "junie commands should use --mcp-location: {:?}",
            junie.commands
        );
    }

    #[test]
    fn instructions_for_codex_do_not_invent_a_config_flag() {
        let dir = tempdir().expect("tempdir");
        let outputs = build_mux_outputs(
            &one_service(),
            &dir.path().join("mux"),
            "rust-mux-proxy",
            &[],
        )
        .expect("build");
        let codex = safe_path_instructions(&outputs)
            .into_iter()
            .find(|i| i.kind == HostKind::Codex)
            .expect("codex instructions");
        // We must not document a fake `codex --config <file>.toml` line.
        for cmd in &codex.commands {
            assert!(
                !cmd.contains("codex --config "),
                "codex command line invents an unsupported flag: {}",
                cmd
            );
        }
    }
}
