//! Interactive wizard for configuring rmcp_mux services and rewiring MCP clients.
//!
//! The wizard provides a four-step TUI flow:
//! 1. Server Detection - detect and select MCP servers
//! 2. Client Detection - detect and select MCP clients (hosts)
//! 3. Confirmation - review and save configuration
//! 4. Health Check - verify configuration works, with option to retry

use std::io::{IsTerminal, stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};
use clap::Args;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::expand_path;

mod clients;
mod keys;
mod persist;
mod services;
mod types;
mod ui;

use clients::client_entry_from_custom_path;
use keys::handle_key;
use persist::{run_danger_auto_configure, run_safe_generate};
use services::{check_health, default_server_config, form_from_service, load_all_services};
use types::{
    AppState, ConfirmChoice, Field, HealthCheckChoice, HealthStatus, Panel, PendingAction,
    ServiceEntry, ServiceSource, WizardStep,
};
use ui::draw_ui;

// ─────────────────────────────────────────────────────────────────────────────
// CLI arguments
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Args)]
pub struct WizardArgs {
    /// Path to mux config (json/yaml/toml). Default: ~/.codex/mcp-mux.toml (expanded to home directory)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Service key to edit or create.
    #[arg(long)]
    pub service: Option<String>,
    /// Socket path override.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Command override (e.g. npx).
    #[arg(long)]
    pub cmd: Option<String>,
    /// Args override (space separated).
    #[arg(long)]
    pub args: Vec<String>,
    /// Max clients override.
    #[arg(long)]
    pub max_clients: Option<usize>,
    /// Log level override.
    #[arg(long)]
    pub log_level: Option<String>,
    /// Tray override.
    #[arg(long)]
    pub tray: Option<bool>,
    /// Do not write files; just preview.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Import additional MCP client config files (JSON or TOML) discovered
    /// outside the canonical default locations. May be passed multiple times.
    #[arg(long = "import-config")]
    pub import_configs: Vec<PathBuf>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Main entry point
// ─────────────────────────────────────────────────────────────────────────────

pub async fn run_wizard(args: WizardArgs) -> Result<()> {
    if !stdout().is_terminal() {
        return Err(anyhow!(
            "wizard requires an interactive TTY; run with --config/--service in non-interactive mode"
        ));
    }

    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| expand_path("~/.codex/mcp-mux.toml"));

    let mut services = load_all_services(&config_path)?;

    // If --service provided, ensure it exists in the list
    if let Some(ref svc_name) = args.service
        && !services.iter().any(|s| s.name == *svc_name)
    {
        services.push(ServiceEntry {
            name: svc_name.clone(),
            config: default_server_config(),
            health: HealthStatus::Unknown,
            dirty: false,
            source: ServiceSource::Config,
            pid: None,
            selected: true,
        });
    }

    // If list is empty, add a default entry
    if services.is_empty() {
        services.push(ServiceEntry {
            name: "general-memory".into(),
            config: default_server_config(),
            health: HealthStatus::Unknown,
            dirty: false,
            source: ServiceSource::Config,
            pid: None,
            selected: true,
        });
    }

    // Run initial health checks
    for svc in &mut services {
        svc.health = check_health(&svc.config);
    }

    // Find initial selection
    let selected = if let Some(ref svc_name) = args.service {
        services
            .iter()
            .position(|s| s.name == *svc_name)
            .unwrap_or(0)
    } else {
        0
    };

    let form = form_from_service(&services[selected]);

    let imported_clients: Vec<types::ClientEntry> = args
        .import_configs
        .iter()
        .map(|p| client_entry_from_custom_path(p))
        .collect();

    let mut app = AppState {
        wizard_step: WizardStep::ServerSelection,
        config_path,
        services,
        selected_service: selected,
        clients: imported_clients,
        selected_client: 0,
        form,
        current_field: Field::ServiceName,
        editing: None,
        active_panel: Panel::ServiceList,
        confirm_choice: ConfirmChoice::SafeGenerate,
        health_choice: HealthCheckChoice::Ok,
        message: "STEP 1: Server Detection - Space: toggle selection | Tab: switch | Enter: edit | n: next step | q: quit".into(),
        dry_run: args.dry_run,
        pending_action: None,
    };

    run_tui(&mut app)?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// TUI main loop
// ─────────────────────────────────────────────────────────────────────────────

fn run_tui(app: &mut AppState) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let evt = event::read()?;
        if let Event::Key(key) = evt {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if handle_key(app, key)? {
                break;
            }
        }
    }

    // Restore cooked terminal before any post-loop side effect (println,
    // confirmation prompt) so output is visible to the user.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Drain pending action — these need a normal stdout/stdin and never run
    // inside the raw-mode loop above.
    if let Some(action) = app.pending_action.take() {
        match action {
            PendingAction::SafeGenerate => {
                let summary = run_safe_generate(app)?;
                println!("{summary}");
            }
            PendingAction::DangerAutoConfigure => {
                // Pass an inert sink — the function manages its own
                // crossterm state guarded by the leave/enter calls inside.
                let mut sink = std::io::sink();
                let summary = run_danger_auto_configure(app, &mut sink)?;
                println!("{summary}");
            }
        }
    }

    Ok(())
}
