//! Type definitions for the wizard module.

use std::path::PathBuf;

use crate::config::ServerConfig;
use crate::scan::{Confidence, ConfigSchema, HostFormat, HostKind};

// ─────────────────────────────────────────────────────────────────────────────
// Enums
// ─────────────────────────────────────────────────────────────────────────────

/// Wizard step in the four-step flow
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    /// Step 1: Detect and select MCP servers
    ServerSelection,
    /// Step 2: Detect and select MCP clients (hosts)
    ClientSelection,
    /// Step 3: Final confirmation and save options
    Confirmation,
    /// Step 4: Health check - verify configuration works
    HealthCheck,
}

/// Choice for the health check step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthCheckChoice {
    /// Configuration verified, exit wizard
    Ok,
    /// Re-run detection and try again
    TryAgain,
}

/// Action queued by a confirm-dialog choice that needs to run *outside* the
/// raw-mode TUI loop (e.g. anything that prints to stdout or asks for typed
/// confirmation on a normal terminal).
///
/// `keys.rs` sets this and exits the loop; `wizard/mod.rs::run_tui` drains
/// it after restoring the cooked terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    SafeGenerate,
    DangerAutoConfigure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    ServiceName,
    Socket,
    Cmd,
    Args,
    Env,
    MaxClients,
    LogLevel,
    Tray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    ServiceList,
    Editor,
    ConfirmDialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    /// Safe path: write `~/.config/mux/{config.toml,mcp.json,mcp.toml}` and
    /// print per-client setup commands. Existing client configs are left
    /// untouched.
    SafeGenerate,
    /// Save mux daemon config only (legacy `~/.codex/mcp-mux.toml` flow).
    SaveMuxOnly,
    /// Copy mux config to clipboard.
    CopyToClipboard,
    /// `[DANGER]` automatically rewrite known MCP server blocks in existing
    /// client configs to point at `rust-mux-proxy`. Backup-first,
    /// preview-first, explicit-confirmation-only.
    DangerAutoConfigure,
    /// Go back to the previous step.
    Back,
    /// Exit without saving.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSource {
    /// Loaded from config file
    Config,
    /// Detected from running process
    Detected,
}

// ─────────────────────────────────────────────────────────────────────────────
// Structs
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub name: String,
    pub config: ServerConfig,
    pub health: HealthStatus,
    pub dirty: bool,
    pub source: ServiceSource,
    /// PID of running process (if detected)
    pub pid: Option<u32>,
    /// Whether this server is selected for inclusion in mux config
    pub selected: bool,
}

/// Represents a detected MCP client (host application)
#[derive(Debug, Clone)]
pub struct ClientEntry {
    /// Host kind (Codex, Claude, ClaudeDesktop, Junie, Gemini, ...)
    pub kind: HostKind,
    /// Path to the client's config file
    pub config_path: PathBuf,
    /// Wire format of the file (json/toml).
    pub format: HostFormat,
    /// Logical schema inside the file.
    pub schema: ConfigSchema,
    /// Discovery confidence — informs the UI label and filtering.
    pub confidence: Confidence,
    /// Whether this client is selected for rewiring
    pub selected: bool,
    /// Services defined in this client's config
    pub services: Vec<String>,
    /// Whether the client is already rewired to use rmcp_mux
    pub already_rewired: bool,
    /// Whether the config file exists (client may be installed but without MCP config)
    pub config_exists: bool,
    /// Whether this client is eligible for the [DANGER] auto-rewrite flow.
    pub eligible_for_danger: bool,
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub service_name: String,
    pub socket: String,
    pub cmd: String,
    pub args: String,
    pub env: String,
    pub max_clients: String,
    pub log_level: String,
    pub tray: bool,
    pub dirty: bool,
}

impl Default for FormState {
    fn default() -> Self {
        Self {
            service_name: String::new(),
            socket: String::new(),
            cmd: "npx".into(),
            args: "@modelcontextprotocol/server-memory".into(),
            env: String::new(),
            max_clients: "5".into(),
            log_level: "info".into(),
            tray: false,
            dirty: false,
        }
    }
}

pub struct AppState {
    /// Current wizard step
    pub wizard_step: WizardStep,
    /// Path to mux config file
    pub config_path: PathBuf,
    /// Detected/configured MCP servers
    pub services: Vec<ServiceEntry>,
    /// Currently highlighted server in list
    pub selected_service: usize,
    /// Detected MCP clients (hosts)
    pub clients: Vec<ClientEntry>,
    /// Currently highlighted client in list
    pub selected_client: usize,
    /// Form state for editing
    pub form: FormState,
    pub current_field: Field,
    pub editing: Option<Field>,
    pub active_panel: Panel,
    pub confirm_choice: ConfirmChoice,
    /// Health check step choice
    pub health_choice: HealthCheckChoice,
    pub message: String,
    pub dry_run: bool,
    /// Action to perform after the TUI loop exits and the terminal has
    /// been restored to cooked mode (used for `[DANGER]` and the
    /// safe-path generator that prints to stdout).
    pub pending_action: Option<PendingAction>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Navigation helpers
// ─────────────────────────────────────────────────────────────────────────────

pub fn previous_field(current: Field) -> Field {
    match current {
        Field::ServiceName => Field::Tray,
        Field::Socket => Field::ServiceName,
        Field::Cmd => Field::Socket,
        Field::Args => Field::Cmd,
        Field::Env => Field::Args,
        Field::MaxClients => Field::Env,
        Field::LogLevel => Field::MaxClients,
        Field::Tray => Field::LogLevel,
    }
}

pub fn next_field(current: Field) -> Field {
    match current {
        Field::ServiceName => Field::Socket,
        Field::Socket => Field::Cmd,
        Field::Cmd => Field::Args,
        Field::Args => Field::Env,
        Field::Env => Field::MaxClients,
        Field::MaxClients => Field::LogLevel,
        Field::LogLevel => Field::Tray,
        Field::Tray => Field::ServiceName,
    }
}

pub fn previous_confirm_choice(current: ConfirmChoice) -> ConfirmChoice {
    match current {
        ConfirmChoice::SafeGenerate => ConfirmChoice::Exit,
        ConfirmChoice::SaveMuxOnly => ConfirmChoice::SafeGenerate,
        ConfirmChoice::CopyToClipboard => ConfirmChoice::SaveMuxOnly,
        ConfirmChoice::DangerAutoConfigure => ConfirmChoice::CopyToClipboard,
        ConfirmChoice::Back => ConfirmChoice::DangerAutoConfigure,
        ConfirmChoice::Exit => ConfirmChoice::Back,
    }
}

pub fn next_confirm_choice(current: ConfirmChoice) -> ConfirmChoice {
    match current {
        ConfirmChoice::SafeGenerate => ConfirmChoice::SaveMuxOnly,
        ConfirmChoice::SaveMuxOnly => ConfirmChoice::CopyToClipboard,
        ConfirmChoice::CopyToClipboard => ConfirmChoice::DangerAutoConfigure,
        ConfirmChoice::DangerAutoConfigure => ConfirmChoice::Back,
        ConfirmChoice::Back => ConfirmChoice::Exit,
        ConfirmChoice::Exit => ConfirmChoice::SafeGenerate,
    }
}
