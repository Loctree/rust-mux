//! Client (host application) detection logic.
//!
//! Detection works off `scan::default_sources()` so the wizard always sees
//! the same canonical list as the rest of the codebase. For each candidate
//! source we record whether the file exists, attempt to parse it, and
//! flag whether it is already pointing at `rust-mux-proxy` (so we don't
//! re-rewire something that's already done).

use std::collections::HashSet;
use std::path::PathBuf;

use crate::scan::{HostKind, default_sources, host_file_from_custom_path, scan_host_file};

use super::types::ClientEntry;

/// Detect MCP clients by walking the canonical default sources.
///
/// We surface every source that actually exists on disk plus, for the
/// well-known clients (Claude Code, Claude Desktop, Codex, Junie, Gemini),
/// any whose app appears installed but whose config file is missing — so
/// the wizard can offer to create one.
pub fn detect_clients() -> Vec<ClientEntry> {
    let mut clients = Vec::new();
    let mut seen: HashSet<(HostKind, PathBuf)> = HashSet::new();

    // Pass 1: existing files (real MCP configs).
    for source in default_sources() {
        if !source.path.exists() {
            continue;
        }

        let scan = scan_host_file(&source).ok();
        let services: Vec<String> = scan
            .as_ref()
            .map(|r| r.services.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();

        let already_rewired = scan
            .as_ref()
            .map(|r| r.services.iter().any(|s| is_proxy_command(&s.command)))
            .unwrap_or(false);

        seen.insert((source.kind, source.path.clone()));
        clients.push(ClientEntry {
            kind: source.kind,
            config_path: source.path.clone(),
            format: source.format,
            schema: source.schema,
            confidence: source.confidence,
            selected: !already_rewired,
            services,
            already_rewired,
            config_exists: true,
            eligible_for_danger: source.eligible_for_danger,
        });
    }

    // Pass 2: well-known clients with their config missing but the app/dir
    // present. This is informational: the wizard can offer to create the file.
    for source in default_sources() {
        if seen.contains(&(source.kind, source.path.clone())) {
            continue;
        }
        if !is_well_known_client(source.kind) {
            continue;
        }
        if !app_installed(source.kind) {
            continue;
        }

        clients.push(ClientEntry {
            kind: source.kind,
            config_path: source.path.clone(),
            format: source.format,
            schema: source.schema,
            confidence: source.confidence,
            selected: true,
            services: Vec::new(),
            already_rewired: false,
            config_exists: false,
            eligible_for_danger: source.eligible_for_danger,
        });
        seen.insert((source.kind, source.path));
    }

    clients
}

/// Build a [`ClientEntry`] from a user-provided custom path. Used by the
/// wizard when the operator wants to import a config not in the default
/// list (e.g. a workspace-local MCP file).
pub fn client_entry_from_custom_path(path: &std::path::Path) -> ClientEntry {
    let source = host_file_from_custom_path(path);
    let scan = scan_host_file(&source).ok();
    let services: Vec<String> = scan
        .as_ref()
        .map(|r| r.services.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let already_rewired = scan
        .as_ref()
        .map(|r| r.services.iter().any(|s| is_proxy_command(&s.command)))
        .unwrap_or(false);

    ClientEntry {
        kind: source.kind,
        config_path: source.path.clone(),
        format: source.format,
        schema: source.schema,
        confidence: source.confidence,
        selected: true,
        services,
        already_rewired,
        config_exists: source.path.exists(),
        eligible_for_danger: source.eligible_for_danger,
    }
}

fn is_proxy_command(cmd: &str) -> bool {
    cmd.contains("rust-mux-proxy") || cmd.contains("rust_mux") || cmd.contains("rmcp-mux")
}

fn is_well_known_client(kind: HostKind) -> bool {
    matches!(
        kind,
        HostKind::Claude
            | HostKind::ClaudeDesktop
            | HostKind::Codex
            | HostKind::Junie
            | HostKind::Gemini
    )
}

/// Heuristic: does an indicator path exist for this client kind? Mirrors the
/// previous behaviour of the wizard: a client without a config file is
/// surfaced only if the app appears installed.
fn app_installed(kind: HostKind) -> bool {
    use crate::config::expand_path;
    let candidates: Vec<PathBuf> = match kind {
        HostKind::Claude => vec![
            expand_path("~/.claude"),
            expand_path("/Applications/Claude Code.app"),
        ],
        HostKind::ClaudeDesktop => vec![
            expand_path("~/Library/Application Support/Claude"),
            expand_path("/Applications/Claude.app"),
        ],
        HostKind::Codex => vec![expand_path("~/.codex")],
        HostKind::Junie => vec![
            expand_path("~/.junie"),
            expand_path("~/.agents"),
            expand_path("~/.ai"),
        ],
        HostKind::Gemini => vec![expand_path("~/.gemini")],
        HostKind::Cursor => vec![
            expand_path("~/Library/Application Support/Cursor"),
            expand_path("/Applications/Cursor.app"),
        ],
        HostKind::VSCode => vec![
            expand_path("~/Library/Application Support/Code"),
            expand_path("/Applications/Visual Studio Code.app"),
        ],
        HostKind::JetBrains => vec![expand_path("~/Library/Application Support/JetBrains")],
        HostKind::Custom | HostKind::Unknown => Vec::new(),
    };
    candidates.iter().any(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn custom_path_import_picks_up_existing_services() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("workspace-mcp.json");
        fs::write(
            &path,
            r#"{"mcpServers": {"memory": {"command": "npx", "args": []}}}"#,
        )
        .expect("write");
        let entry = client_entry_from_custom_path(&path);
        assert_eq!(entry.kind, HostKind::Custom);
        assert!(entry.config_exists);
        assert_eq!(entry.services, vec!["memory".to_string()]);
        assert!(!entry.already_rewired);
        assert!(entry.eligible_for_danger);
    }

    #[test]
    fn custom_path_for_missing_file_marks_as_missing() {
        let entry = client_entry_from_custom_path(std::path::Path::new(
            "/tmp/this-file-does-not-exist-rust-mux-test.json",
        ));
        assert!(!entry.config_exists);
        assert!(entry.services.is_empty());
    }

    #[test]
    fn proxy_command_detection_recognises_rust_mux_proxy() {
        assert!(is_proxy_command("rust-mux-proxy"));
        assert!(is_proxy_command("/usr/local/bin/rust-mux-proxy"));
        assert!(!is_proxy_command("npx"));
    }
}
