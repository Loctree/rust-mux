//! Multi-server types and utilities for rmcp_mux.
//!
//! Provides shared types for managing multiple MCP servers in a single process.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::ResolvedParams;
use crate::state::MuxState;

/// Status level for display in dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusLevel {
    Ok,
    Warn,
    Error,
    Lazy,
}

/// Server command for control operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCommand {
    Start,
    Stop,
    Restart,
}

/// Status snapshot for multi-server display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiServerStatus {
    pub name: String,
    pub level: StatusLevel,
    pub status_text: String,
    pub connected_clients: usize,
    pub active_clients: usize,
    pub max_active_clients: usize,
    pub pending_requests: usize,
    pub restarts: u64,
    pub uptime_ms: u64,
    pub in_backoff: bool,
    pub heartbeat_latency_ms: Option<u64>,
}

/// A managed MCP server instance.
pub struct ManagedServer {
    /// Server configuration
    pub params: ResolvedParams,
    /// Server state
    pub state: Arc<Mutex<MuxState>>,
    /// Shutdown token for this server
    pub shutdown: CancellationToken,
    /// When the server was started
    pub started_at: Instant,
}

/// State for TUI-based multi-server management.
pub struct TuiMuxState {
    /// All managed servers
    pub servers: HashMap<String, ManagedServer>,
    /// Global shutdown token
    pub shutdown: CancellationToken,
    /// When the multi-mux runtime started
    pub start_time: Instant,
}

impl TuiMuxState {
    pub fn new(shutdown: CancellationToken) -> Self {
        Self {
            servers: HashMap::new(),
            shutdown,
            start_time: Instant::now(),
        }
    }

    pub fn add_server(&mut self, name: String, server: ManagedServer) {
        self.servers.insert(name, server);
    }
}

/// Format uptime in human-readable form.
pub fn format_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;

    if days > 0 {
        format!("{}d {}h", days, hours % 24)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins % 60)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_seconds() {
        assert_eq!(format_uptime(5000), "5s");
        assert_eq!(format_uptime(45000), "45s");
    }

    #[test]
    fn format_uptime_minutes() {
        assert_eq!(format_uptime(60000), "1m 0s");
        assert_eq!(format_uptime(125000), "2m 5s");
    }

    #[test]
    fn format_uptime_hours() {
        assert_eq!(format_uptime(3600000), "1h 0m");
        assert_eq!(format_uptime(7500000), "2h 5m");
    }

    #[test]
    fn format_uptime_days() {
        assert_eq!(format_uptime(86400000), "1d 0h");
        assert_eq!(format_uptime(90000000), "1d 1h");
    }

    #[test]
    fn status_level_serialization() {
        assert_eq!(serde_json::to_string(&StatusLevel::Ok).unwrap(), "\"Ok\"");
    }
}
