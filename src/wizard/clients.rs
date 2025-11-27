//! Client (host application) detection logic.

use crate::scan::{discover_hosts, scan_host_file};

use super::types::ClientEntry;

/// Detect MCP clients (host applications) using discover_hosts from scan.rs
pub fn detect_clients() -> Vec<ClientEntry> {
    let hosts = discover_hosts();
    let mut clients = Vec::new();

    for host in hosts {
        // Scan the host file once and reuse the result
        let scan_result = scan_host_file(&host).ok();

        let services: Vec<String> = scan_result
            .as_ref()
            .map(|r| r.services.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();

        // Check if already rewired (command contains rmcp_mux)
        let already_rewired = scan_result
            .as_ref()
            .map(|r| {
                r.services.iter().any(|s| {
                    s.command.contains("rmcp_mux") || s.command.contains("rmcp_mux_proxy")
                })
            })
            .unwrap_or(false);

        clients.push(ClientEntry {
            kind: host.kind,
            config_path: host.path,
            selected: !already_rewired, // Auto-select if not already rewired
            services,
            already_rewired,
        });
    }

    clients
}
