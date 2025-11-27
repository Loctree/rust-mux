use std::io::{stdout, BufRead, BufReader, IsTerminal};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::{expand_path, load_config, Config, ServerConfig};

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
}

use crate::scan::{discover_hosts, HostFile, HostKind, scan_host_file, rewire_host};

// ─────────────────────────────────────────────────────────────────────────────
// Enums and data structures
// ─────────────────────────────────────────────────────────────────────────────

/// Wizard step in the two-step flow
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardStep {
    /// Step 1: Detect and select MCP servers
    ServerSelection,
    /// Step 2: Detect and select MCP clients (hosts)
    ClientSelection,
    /// Step 3: Final confirmation and save options
    Confirmation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    ServiceName,
    Socket,
    Cmd,
    Args,
    MaxClients,
    LogLevel,
    Tray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    ServiceList,
    Editor,
    ConfirmDialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmChoice {
    SaveAll,
    SaveMuxOnly,
    CopyToClipboard,
    Back,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthStatus {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceSource {
    /// Loaded from config file
    Config,
    /// Detected from running process
    Detected,
}

#[derive(Debug, Clone)]
struct ServiceEntry {
    name: String,
    config: ServerConfig,
    health: HealthStatus,
    dirty: bool,
    source: ServiceSource,
    /// PID of running process (if detected)
    pid: Option<u32>,
    /// Whether this server is selected for inclusion in mux config
    selected: bool,
}

/// Represents a detected MCP client (host application)
#[derive(Debug, Clone)]
struct ClientEntry {
    /// Host kind (Codex, Cursor, VSCode, Claude, JetBrains)
    kind: HostKind,
    /// Path to the client's config file
    config_path: PathBuf,
    /// Whether this client is selected for rewiring
    selected: bool,
    /// Services defined in this client's config
    services: Vec<String>,
    /// Whether the client is already rewired to use rmcp_mux
    already_rewired: bool,
}

#[derive(Debug, Clone)]
struct FormState {
    service_name: String,
    socket: String,
    cmd: String,
    args: String,
    max_clients: String,
    log_level: String,
    tray: bool,
    dirty: bool,
}

impl Default for FormState {
    fn default() -> Self {
        Self {
            service_name: String::new(),
            socket: String::new(),
            cmd: "npx".into(),
            args: "@modelcontextprotocol/server-memory".into(),
            max_clients: "5".into(),
            log_level: "info".into(),
            tray: false,
            dirty: false,
        }
    }
}

struct AppState {
    /// Current wizard step
    wizard_step: WizardStep,
    /// Path to mux config file
    config_path: PathBuf,
    /// Socket directory for mux services
    socket_dir: PathBuf,
    /// Detected/configured MCP servers
    services: Vec<ServiceEntry>,
    /// Currently highlighted server in list
    selected_service: usize,
    /// Detected MCP clients (hosts)
    clients: Vec<ClientEntry>,
    /// Currently highlighted client in list
    selected_client: usize,
    /// Form state for editing
    form: FormState,
    current_field: Field,
    editing: Option<Field>,
    active_panel: Panel,
    confirm_choice: ConfirmChoice,
    message: String,
    dry_run: bool,
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
    if let Some(ref svc_name) = args.service {
        if !services.iter().any(|s| s.name == *svc_name) {
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
        services.iter().position(|s| s.name == *svc_name).unwrap_or(0)
    } else {
        0
    };

    let form = form_from_service(&services[selected]);

    // Default socket directory
    let socket_dir = expand_path("~/mcp-sockets");

    let mut app = AppState {
        wizard_step: WizardStep::ServerSelection,
        config_path,
        socket_dir,
        services,
        selected_service: selected,
        clients: Vec::new(), // Will be populated in step 2
        selected_client: 0,
        form,
        current_field: Field::ServiceName,
        editing: None,
        active_panel: Panel::ServiceList,
        confirm_choice: ConfirmChoice::SaveAll,
        message: "STEP 1: Server Detection - Space: toggle selection | Tab: switch | Enter: edit | n: next step | q: quit".into(),
        dry_run: args.dry_run,
    };

    run_tui(&mut app)?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Service loading and health check
// ─────────────────────────────────────────────────────────────────────────────

fn load_all_services(path: &Path) -> Result<Vec<ServiceEntry>> {
    let cfg = load_config(path)?;
    let mut services = Vec::new();

    // Load from config file
    if let Some(cfg) = cfg {
        for (name, server_cfg) in cfg.servers {
            services.push(ServiceEntry {
                name,
                config: server_cfg,
                health: HealthStatus::Unknown,
                dirty: false,
                source: ServiceSource::Config,
                pid: None,
                selected: true,
            });
        }
    }

    // Detect running MCP processes and merge
    let detected = detect_running_mcp_servers();
    for mut det in detected {
        // Check if we already have this service in config (by matching command+args)
        let already_configured = services.iter().any(|s| {
            // Match by name or by command+args combination
            s.name == det.name || (
                s.config.cmd == det.config.cmd &&
                s.config.args == det.config.args
            )
        });

        if !already_configured {
            // Generate a socket path for the detected service
            let socket_path = format!("~/mcp-sockets/{}.sock", det.name);
            det.config.socket = Some(socket_path);
            services.push(det);
        }
    }

    // Sort: config entries first, then detected, both alphabetically
    services.sort_by(|a, b| {
        match (&a.source, &b.source) {
            (ServiceSource::Config, ServiceSource::Detected) => std::cmp::Ordering::Less,
            (ServiceSource::Detected, ServiceSource::Config) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });

    Ok(services)
}

/// Patterns that indicate an MCP server process
const MCP_PATTERNS: &[&str] = &[
    "@modelcontextprotocol/",
    "mcp-server-",
    "server-memory",
    "server-filesystem",
    "server-github",
    "server-gitlab",
    "server-slack",
    "server-google-drive",
    "server-postgres",
    "server-sqlite",
    "server-redis",
    "server-brave-search",
    "server-fetch",
    "server-puppeteer",
    "server-sequential-thinking",
    "claude-mcp",
    "mcp_server",
];

/// Detect running MCP server processes by scanning `ps` output
fn detect_running_mcp_servers() -> Vec<ServiceEntry> {
    let mut detected = Vec::new();

    // Run `ps -eo pid,args` to get all processes with their arguments
    let output = match Command::new("ps")
        .args(["-eo", "pid,args"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return detected,
    };

    if !output.status.success() {
        return detected;
    }

    let reader = BufReader::new(&output.stdout[..]);
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in reader.lines().flatten() {
        let line = line.trim();
        
        // Skip header line
        if line.starts_with("PID") {
            continue;
        }

        // Parse PID and args
        let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
        if parts.len() < 2 {
            continue;
        }

        let pid: u32 = match parts[0].trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let args = parts[1].trim();

        // Check if this process matches any MCP pattern
        let is_mcp = MCP_PATTERNS.iter().any(|pattern| args.contains(pattern));
        if !is_mcp {
            continue;
        }

        // Skip rmcp_mux itself and its proxy
        if args.contains("rmcp_mux") {
            continue;
        }

        // Extract a meaningful name from the process
        let name = extract_service_name(args);
        
        // Ensure unique names
        let unique_name = if seen_names.contains(&name) {
            let mut counter = 2;
            loop {
                let candidate = format!("{}-{}", name, counter);
                if !seen_names.contains(&candidate) {
                    break candidate;
                }
                counter += 1;
            }
        } else {
            name
        };
        seen_names.insert(unique_name.clone());

        // Try to extract command and args from the process line
        let (cmd, cmd_args) = extract_cmd_and_args(args);

        let config = ServerConfig {
            socket: None, // Will be generated when user configures
            cmd: Some(cmd),
            args: Some(cmd_args),
            max_active_clients: Some(5),
            tray: Some(false),
            service_name: Some(unique_name.clone()),
            log_level: Some("info".into()),
            lazy_start: Some(false),
            max_request_bytes: Some(1_048_576),
            request_timeout_ms: Some(30_000),
            restart_backoff_ms: Some(1_000),
            restart_backoff_max_ms: Some(30_000),
            max_restarts: Some(5),
            status_file: None,
        };

        detected.push(ServiceEntry {
            name: unique_name,
            config,
            health: HealthStatus::Healthy, // It's running, so it's "healthy"
            dirty: false,
            source: ServiceSource::Detected,
            pid: Some(pid),
            selected: true,
        });
    }

    detected
}

/// Extract a human-readable service name from process arguments
fn extract_service_name(args: &str) -> String {
    // Try to find @modelcontextprotocol/server-XXX pattern
    if let Some(idx) = args.find("@modelcontextprotocol/") {
        let rest = &args[idx + "@modelcontextprotocol/".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }

    // Try to find mcp-server-XXX pattern
    if let Some(idx) = args.find("mcp-server-") {
        let rest = &args[idx + "mcp-server-".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() {
            return format!("mcp-{}", name);
        }
    }

    // Try to find server-XXX pattern (common MCP naming)
    if let Some(idx) = args.find("server-") {
        let rest = &args[idx + "server-".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }

    // Fallback: use a generic name
    "detected-mcp".into()
}

/// Extract command and arguments from a process command line
fn extract_cmd_and_args(args: &str) -> (String, Vec<String>) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return ("unknown".into(), vec![]);
    }

    // Find the main command (npx, node, python, etc.)
    let cmd = if parts[0].contains('/') {
        // Full path - extract just the binary name
        parts[0].rsplit('/').next().unwrap_or(parts[0]).to_string()
    } else {
        parts[0].to_string()
    };

    // Everything after the command is args
    let cmd_args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    (cmd, cmd_args)
}

fn check_health(config: &ServerConfig) -> HealthStatus {
    let socket_path = match &config.socket {
        Some(s) => expand_path(s),
        None => return HealthStatus::Unknown,
    };

    // Try to connect to the socket synchronously
    match UnixStream::connect(&socket_path) {
        Ok(_) => HealthStatus::Healthy,
        Err(_) => HealthStatus::Unhealthy,
    }
}

fn default_server_config() -> ServerConfig {
    ServerConfig {
        socket: Some("~/mcp-sockets/general-memory.sock".into()),
        cmd: Some("npx".into()),
        args: Some(vec!["@modelcontextprotocol/server-memory".into()]),
        max_active_clients: Some(5),
        tray: Some(false),
        service_name: None,
        log_level: Some("info".into()),
        lazy_start: Some(false),
        max_request_bytes: Some(1_048_576),
        request_timeout_ms: Some(30_000),
        restart_backoff_ms: Some(1_000),
        restart_backoff_max_ms: Some(30_000),
        max_restarts: Some(5),
        status_file: None,
    }
}

fn form_from_service(svc: &ServiceEntry) -> FormState {
    FormState {
        service_name: svc.name.clone(),
        socket: svc.config.socket.clone().unwrap_or_default(),
        cmd: svc.config.cmd.clone().unwrap_or_else(|| "npx".into()),
        args: svc.config.args.clone().unwrap_or_default().join(" "),
        max_clients: svc.config.max_active_clients.unwrap_or(5).to_string(),
        log_level: svc.config.log_level.clone().unwrap_or_else(|| "info".into()),
        tray: svc.config.tray.unwrap_or(false),
        dirty: false,
    }
}

fn service_from_form(form: &FormState) -> ServerConfig {
    let args_vec: Vec<String> = form
        .args
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    ServerConfig {
        socket: Some(form.socket.clone()),
        cmd: Some(form.cmd.clone()),
        args: Some(args_vec),
        max_active_clients: form.max_clients.trim().parse().ok(),
        tray: Some(form.tray),
        service_name: Some(form.service_name.clone()),
        log_level: Some(form.log_level.clone()),
        lazy_start: Some(false),
        max_request_bytes: Some(1_048_576),
        request_timeout_ms: Some(30_000),
        restart_backoff_ms: Some(1_000),
        restart_backoff_max_ms: Some(30_000),
        max_restarts: Some(5),
        status_file: None,
    }
}

/// Detect MCP clients (host applications) using discover_hosts from scan.rs
fn detect_clients() -> Vec<ClientEntry> {
    let hosts = discover_hosts();
    let mut clients = Vec::new();

    for host in hosts {
        // Try to scan the host file for services
        let services: Vec<String> = match scan_host_file(&host) {
            Ok(scan_result) => scan_result.services.iter().map(|s| s.name.clone()).collect(),
            Err(_) => Vec::new(),
        };

        // Check if already rewired (command contains rmcp_mux)
        let already_rewired = match scan_host_file(&host) {
            Ok(scan_result) => scan_result.services.iter().any(|s| {
                s.command.contains("rmcp_mux") || s.command.contains("rmcp_mux_proxy")
            }),
            Err(_) => false,
        };

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

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Drawing
// ─────────────────────────────────────────────────────────────────────────────

fn draw_ui(f: &mut Frame, app: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Main area (two columns)
            Constraint::Length(3),  // Status bar
        ])
        .split(f.area());

    // Title with step indicator
    let step_info = match app.wizard_step {
        WizardStep::ServerSelection => "Step 1/3: Server Detection",
        WizardStep::ClientSelection => "Step 2/3: Client Detection",
        WizardStep::Confirmation => "Step 3/3: Confirmation",
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "rmcp_mux wizard",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — "),
        Span::styled(step_info, Style::default().fg(Color::Cyan)),
    ]));
    f.render_widget(title, chunks[0]);

    // Main area: two columns
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35),  // Left: list
            Constraint::Percentage(65),  // Right: editor/details
        ])
        .split(chunks[1]);

    // Draw appropriate content based on wizard step
    match app.wizard_step {
        WizardStep::ServerSelection => {
            draw_service_list(f, app, main_chunks[0]);
            draw_editor(f, app, main_chunks[1]);
        }
        WizardStep::ClientSelection => {
            draw_client_list(f, app, main_chunks[0]);
            draw_client_details(f, app, main_chunks[1]);
        }
        WizardStep::Confirmation => {
            draw_summary(f, app, main_chunks[0]);
            draw_save_options(f, app, main_chunks[1]);
        }
    }

    // Status bar
    let mut footer_spans = vec![Span::raw(&app.message)];
    if app.dry_run {
        footer_spans.push(Span::styled(" | DRY-RUN", Style::default().fg(Color::Yellow)));
    }
    let status = Paragraph::new(Line::from(footer_spans))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, chunks[2]);

    // Draw confirm dialog if active
    if app.active_panel == Panel::ConfirmDialog {
        draw_confirm_dialog(f, app);
    }
}

fn draw_service_list(f: &mut Frame, app: &AppState, area: Rect) {
    let is_active = app.active_panel == Panel::ServiceList && app.wizard_step == WizardStep::ServerSelection;
    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Count services by source and selection for title
    let selected_count = app.services.iter().filter(|s| s.selected).count();
    let total_count = app.services.len();
    let title = format!("STEP 1: Servers [{}/{}]", selected_count, total_count);

    let items: Vec<ListItem> = app
        .services
        .iter()
        .enumerate()
        .map(|(i, svc)| {
            // Selection checkbox
            let checkbox = if svc.selected {
                Span::styled("[✓] ", Style::default().fg(Color::Green))
            } else {
                Span::styled("[ ] ", Style::default().fg(Color::DarkGray))
            };

            // Source indicator: config file vs detected process
            let source_indicator = match svc.source {
                ServiceSource::Config => Span::styled("[C]", Style::default().fg(Color::Blue)),
                ServiceSource::Detected => Span::styled("[D]", Style::default().fg(Color::Magenta)),
            };

            // Health indicator
            let health_indicator = match svc.health {
                HealthStatus::Healthy => Span::styled(" ● ", Style::default().fg(Color::Green)),
                HealthStatus::Unhealthy => Span::styled(" ● ", Style::default().fg(Color::Red)),
                HealthStatus::Unknown => Span::styled(" ○ ", Style::default().fg(Color::DarkGray)),
            };

            let name_style = if i == app.selected_service {
                if is_active {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default()
            };

            let dirty_marker = if svc.dirty {
                Span::styled(" *", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            };

            // Show PID for detected processes
            let pid_info = match (svc.source, svc.pid) {
                (ServiceSource::Detected, Some(pid)) => {
                    Span::styled(format!(" ({})", pid), Style::default().fg(Color::DarkGray))
                }
                _ => Span::raw(""),
            };

            ListItem::new(Line::from(vec![
                checkbox,
                source_indicator,
                health_indicator,
                Span::styled(&svc.name, name_style),
                dirty_marker,
                pid_info,
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        );

    f.render_widget(list, area);
}

fn draw_editor(f: &mut Frame, app: &AppState, area: Rect) {
    let is_active = app.active_panel == Panel::Editor;
    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let fields = vec![
        (Field::ServiceName, "Service name", &app.form.service_name),
        (Field::Socket, "Socket", &app.form.socket),
        (Field::Cmd, "Command", &app.form.cmd),
        (Field::Args, "Args", &app.form.args),
        (Field::MaxClients, "Max clients", &app.form.max_clients),
        (Field::LogLevel, "Log level", &app.form.log_level),
    ];

    let mut lines: Vec<Line> = fields
        .into_iter()
        .map(|(field, label, value)| {
            let label_style = Style::default().fg(Color::Cyan);
            let val_style = if Some(field) == app.editing {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if field == app.current_field && is_active {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };

            Line::from(vec![
                Span::styled(format!("{label:<14}"), label_style),
                Span::styled(value.clone(), val_style),
            ])
        })
        .collect();

    // Tray field
    let tray_label = if app.form.tray { "true" } else { "false" };
    let tray_style = if Some(Field::Tray) == app.editing {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else if app.current_field == Field::Tray && is_active {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    lines.push(Line::from(vec![
        Span::styled("Tray enabled  ", Style::default().fg(Color::Cyan)),
        Span::styled(tray_label, tray_style),
    ]));

    let editor = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Editor"),
        );

    f.render_widget(editor, area);
}

fn draw_confirm_dialog(f: &mut Frame, app: &AppState) {
    let area = f.area();
    let dialog_width = 40;
    let dialog_height = 7;
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    // Clear the background
    f.render_widget(Clear, dialog_area);

    let choices = [
        (ConfirmChoice::SaveAll, "SAVE ALL"),
        (ConfirmChoice::SaveMuxOnly, "MUX ONLY"),
        (ConfirmChoice::CopyToClipboard, "CLIPBOARD"),
        (ConfirmChoice::Back, "BACK"),
        (ConfirmChoice::Exit, "EXIT"),
    ];

    let choice_spans: Vec<Span> = choices
        .iter()
        .map(|(choice, label)| {
            if *choice == app.confirm_choice {
                Span::styled(
                    format!(" [{label}] "),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!("  {label}  "), Style::default().fg(Color::White))
            }
        })
        .collect();

    let content = vec![
        Line::from(""),
        Line::from("Save configuration?"),
        Line::from(""),
        Line::from(choice_spans),
    ];

    let dialog = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title("Confirm"),
        )
        .alignment(ratatui::layout::Alignment::Center);

    f.render_widget(dialog, dialog_area);
}

fn draw_client_list(f: &mut Frame, app: &AppState, area: Rect) {
    let is_active = app.active_panel == Panel::ServiceList && app.wizard_step == WizardStep::ClientSelection;
    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let selected_count = app.clients.iter().filter(|c| c.selected).count();
    let total_count = app.clients.len();
    let title = format!("STEP 2: Clients [{}/{}]", selected_count, total_count);

    let items: Vec<ListItem> = app
        .clients
        .iter()
        .enumerate()
        .map(|(i, client)| {
            // Selection checkbox
            let checkbox = if client.selected {
                Span::styled("[✓] ", Style::default().fg(Color::Green))
            } else {
                Span::styled("[ ] ", Style::default().fg(Color::DarkGray))
            };

            // Host kind indicator
            let kind_label = match client.kind {
                HostKind::Codex => Span::styled("Codex", Style::default().fg(Color::Blue)),
                HostKind::Cursor => Span::styled("Cursor", Style::default().fg(Color::Magenta)),
                HostKind::VSCode => Span::styled("VSCode", Style::default().fg(Color::Cyan)),
                HostKind::Claude => Span::styled("Claude", Style::default().fg(Color::Yellow)),
                HostKind::JetBrains => Span::styled("JetBrains", Style::default().fg(Color::Green)),
                HostKind::Unknown => Span::styled("Unknown", Style::default().fg(Color::DarkGray)),
            };

            // Rewired status indicator
            let status = if client.already_rewired {
                Span::styled(" [rewired]", Style::default().fg(Color::Green))
            } else {
                Span::styled(" [not rewired]", Style::default().fg(Color::Yellow))
            };

            let name_style = if i == app.selected_client {
                if is_active {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }
            } else {
                Style::default()
            };

            // Service count
            let svc_count = Span::styled(
                format!(" ({} svcs)", client.services.len()),
                Style::default().fg(Color::DarkGray),
            );

            ListItem::new(Line::from(vec![
                checkbox,
                Span::styled("", name_style), // Apply style context
                kind_label,
                status,
                svc_count,
            ]))
        })
        .collect();

    let list = if items.is_empty() {
        let empty_msg = Paragraph::new("No MCP clients detected.\nSupported: Codex, Cursor, VSCode, Claude, JetBrains")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(title),
            )
            .wrap(Wrap { trim: true });
        f.render_widget(empty_msg, area);
        return;
    } else {
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(title),
            )
    };

    f.render_widget(list, area);
}

fn draw_client_details(f: &mut Frame, app: &AppState, area: Rect) {
    let is_active = app.active_panel == Panel::Editor && app.wizard_step == WizardStep::ClientSelection;
    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Client Configuration Details",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if app.clients.is_empty() {
        lines.push(Line::from("No clients detected."));
        lines.push(Line::from(""));
        lines.push(Line::from("The wizard searches for MCP client configs in:"));
        lines.push(Line::from("  • ~/.codex/config.toml (Codex)"));
        lines.push(Line::from("  • ~/Library/.../Cursor/settings.json"));
        lines.push(Line::from("  • ~/Library/.../Code/settings.json (VSCode)"));
        lines.push(Line::from("  • ~/.config/Claude/claude_config.json"));
        lines.push(Line::from("  • ~/Library/.../JetBrains/LLM/mcp.json"));
    } else if app.selected_client < app.clients.len() {
        let client = &app.clients[app.selected_client];
        
        lines.push(Line::from(vec![
            Span::styled("Host:     ", Style::default().fg(Color::Cyan)),
            Span::raw(client.kind.as_label()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Config:   ", Style::default().fg(Color::Cyan)),
            Span::raw(client.config_path.display().to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Status:   ", Style::default().fg(Color::Cyan)),
            if client.already_rewired {
                Span::styled("Already rewired to rmcp_mux", Style::default().fg(Color::Green))
            } else {
                Span::styled("Not yet rewired", Style::default().fg(Color::Yellow))
            },
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Services in this client:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        
        if client.services.is_empty() {
            lines.push(Line::from("  (no services defined)"));
        } else {
            for svc in &client.services {
                lines.push(Line::from(format!("  • {}", svc)));
            }
        }
        
        lines.push(Line::from(""));
        if client.selected {
            lines.push(Line::from(Span::styled(
                "This client will be rewired to use rmcp_mux proxy.",
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "This client will NOT be modified.",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let details = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Details"),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(details, area);
}

fn draw_summary(f: &mut Frame, app: &AppState, area: Rect) {
    let border_style = Style::default().fg(Color::Cyan);

    let selected_servers: Vec<&str> = app
        .services
        .iter()
        .filter(|s| s.selected)
        .map(|s| s.name.as_str())
        .collect();

    let selected_clients: Vec<&str> = app
        .clients
        .iter()
        .filter(|c| c.selected)
        .map(|c| c.kind.as_label())
        .collect();

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Configuration Summary",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Selected Servers ({})", selected_servers.len()),
            Style::default().fg(Color::Cyan),
        )),
    ];

    for name in &selected_servers {
        lines.push(Line::from(format!("  ✓ {}", name)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Selected Clients ({})", selected_clients.len()),
        Style::default().fg(Color::Cyan),
    )));

    for name in &selected_clients {
        lines.push(Line::from(format!("  ✓ {}", name)));
    }

    let summary = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("STEP 3: Summary"),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(summary, area);
}

fn draw_save_options(f: &mut Frame, app: &AppState, area: Rect) {
    let border_style = Style::default().fg(Color::Cyan);

    let choices = [
        (ConfirmChoice::SaveAll, "Save All", "Save mux config AND rewire selected clients"),
        (ConfirmChoice::SaveMuxOnly, "Mux Only", "Save mux config only (no client rewiring)"),
        (ConfirmChoice::CopyToClipboard, "Clipboard", "Copy config to clipboard"),
        (ConfirmChoice::Back, "Back", "Return to previous step"),
        (ConfirmChoice::Exit, "Exit", "Exit without saving"),
    ];

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Save Options",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Use Up/Down to select, Enter to confirm:"),
        Line::from(""),
    ];

    for (choice, label, description) in choices {
        let is_selected = choice == app.confirm_choice;
        let prefix = if is_selected { "▶ " } else { "  " };
        let style = if is_selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(format!("[{}]", label), style),
            Span::raw(" - "),
            Span::styled(description, Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));
    if app.dry_run {
        lines.push(Line::from(Span::styled(
            "DRY-RUN MODE: No files will be modified",
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Backups will be created for all modified files (.bak)",
            Style::default().fg(Color::Green),
        )));
    }

    let options = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Actions"),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(options, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Key handling
// ─────────────────────────────────────────────────────────────────────────────

fn handle_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Result<bool> {
    // Handle confirm dialog separately
    if app.active_panel == Panel::ConfirmDialog {
        return handle_confirm_dialog_key(app, key);
    }

    let is_ctrl_s = key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'));
    let is_plain_s = matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
        && app.editing.is_none()
        && app.active_panel != Panel::Editor;
    let is_save = is_ctrl_s || is_plain_s;

    match key.code {
        // Quit (only when not editing)
        KeyCode::Char('q') if app.editing.is_none() => {
            return Ok(true);
        }

        // Save -> open confirm dialog
        _ if is_save => {
            // First sync form to service
            sync_form_to_service(app);
            app.active_panel = Panel::ConfirmDialog;
            app.confirm_choice = ConfirmChoice::SaveAll;
            app.message = "Use arrows to select, Enter to confirm".into();
        }

        // Tab to switch panels
        KeyCode::Tab => {
            if app.editing.is_none() {
                app.active_panel = match app.active_panel {
                    Panel::ServiceList => Panel::Editor,
                    Panel::Editor => Panel::ServiceList,
                    Panel::ConfirmDialog => Panel::ConfirmDialog,
                };
                update_message(app);
            }
        }

        KeyCode::BackTab => {
            if app.editing.is_none() {
                app.active_panel = match app.active_panel {
                    Panel::ServiceList => Panel::Editor,
                    Panel::Editor => Panel::ServiceList,
                    Panel::ConfirmDialog => Panel::ConfirmDialog,
                };
                update_message(app);
            }
        }

        // Navigation
        KeyCode::Up => {
            if app.editing.is_none() {
                match (app.wizard_step, app.active_panel) {
                    (WizardStep::ServerSelection, Panel::ServiceList) => {
                        if app.selected_service > 0 {
                            sync_form_to_service(app);
                            app.selected_service -= 1;
                            load_service_to_form(app);
                        }
                    }
                    (WizardStep::ServerSelection, Panel::Editor) => {
                        app.current_field = previous_field(app.current_field);
                    }
                    (WizardStep::ClientSelection, Panel::ServiceList) => {
                        if app.selected_client > 0 {
                            app.selected_client -= 1;
                        }
                    }
                    (WizardStep::Confirmation, _) => {
                        // Navigate through save options
                        app.confirm_choice = previous_confirm_choice(app.confirm_choice);
                    }
                    _ => {}
                }
            }
        }

        KeyCode::Down => {
            if app.editing.is_none() {
                match (app.wizard_step, app.active_panel) {
                    (WizardStep::ServerSelection, Panel::ServiceList) => {
                        if app.selected_service < app.services.len().saturating_sub(1) {
                            sync_form_to_service(app);
                            app.selected_service += 1;
                            load_service_to_form(app);
                        }
                    }
                    (WizardStep::ServerSelection, Panel::Editor) => {
                        app.current_field = next_field(app.current_field);
                    }
                    (WizardStep::ClientSelection, Panel::ServiceList) => {
                        if app.selected_client < app.clients.len().saturating_sub(1) {
                            app.selected_client += 1;
                        }
                    }
                    (WizardStep::Confirmation, _) => {
                        // Navigate through save options
                        app.confirm_choice = next_confirm_choice(app.confirm_choice);
                    }
                    _ => {}
                }
            }
        }

        // Enter
        KeyCode::Enter => {
            match (app.wizard_step, app.active_panel) {
                (WizardStep::ServerSelection, Panel::ServiceList) => {
                    // Switch to editor panel
                    app.active_panel = Panel::Editor;
                    update_message(app);
                }
                (WizardStep::ServerSelection, Panel::Editor) => {
                    if app.current_field == Field::Tray {
                        app.form.tray = !app.form.tray;
                        app.form.dirty = true;
                    } else {
                        app.editing = Some(app.current_field);
                        app.message = "Editing... Esc to finish".into();
                    }
                }
                (WizardStep::ClientSelection, Panel::ServiceList) => {
                    // Toggle client selection on Enter as well
                    if app.selected_client < app.clients.len() {
                        app.clients[app.selected_client].selected = 
                            !app.clients[app.selected_client].selected;
                        update_step2_message(app);
                    }
                }
                (WizardStep::Confirmation, _) => {
                    // Execute the selected action
                    return execute_confirm_choice(app);
                }
                _ => {}
            }
        }

        // Space toggles selection (in ServiceList for both steps) or tray (in Editor)
        KeyCode::Char(' ') => {
            match (app.wizard_step, app.active_panel) {
                (WizardStep::ServerSelection, Panel::ServiceList) => {
                    // Toggle selection for current server
                    if app.selected_service < app.services.len() {
                        app.services[app.selected_service].selected = 
                            !app.services[app.selected_service].selected;
                        let selected_count = app.services.iter().filter(|s| s.selected).count();
                        app.message = format!(
                            "STEP 1: {} servers selected | Space: toggle | Tab: edit | n: next step",
                            selected_count
                        );
                    }
                }
                (WizardStep::ServerSelection, Panel::Editor) => {
                    if app.current_field == Field::Tray {
                        app.form.tray = !app.form.tray;
                        app.form.dirty = true;
                    }
                }
                (WizardStep::ClientSelection, Panel::ServiceList) => {
                    // Toggle selection for current client
                    if app.selected_client < app.clients.len() {
                        app.clients[app.selected_client].selected = 
                            !app.clients[app.selected_client].selected;
                        update_step2_message(app);
                    }
                }
                _ => {}
            }
        }

        // Escape
        KeyCode::Esc => {
            if app.editing.is_some() {
                app.editing = None;
                update_message(app);
            }
        }

        // Next step with 'n' key
        KeyCode::Char('n') if app.editing.is_none() => {
            match app.wizard_step {
                WizardStep::ServerSelection => {
                    // Check if any servers are selected
                    let selected_count = app.services.iter().filter(|s| s.selected).count();
                    if selected_count == 0 {
                        app.message = "Please select at least one server (use Space to toggle)".into();
                    } else {
                        // Move to step 2: Client Selection
                        sync_form_to_service(app);
                        app.wizard_step = WizardStep::ClientSelection;
                        app.clients = detect_clients();
                        app.selected_client = 0;
                        app.active_panel = Panel::ServiceList;
                        let client_count = app.clients.len();
                        app.message = format!(
                            "STEP 2: Client Detection - {} clients found | Space: toggle | n: next step | p: previous",
                            client_count
                        );
                    }
                }
                WizardStep::ClientSelection => {
                    // Move to step 3: Confirmation
                    app.wizard_step = WizardStep::Confirmation;
                    app.active_panel = Panel::ConfirmDialog;
                    app.confirm_choice = ConfirmChoice::SaveAll;
                    app.message = "STEP 3: Confirm - Select action and press Enter".into();
                }
                WizardStep::Confirmation => {
                    // Already at confirmation, do nothing
                }
            }
        }

        // Previous step with 'p' key
        KeyCode::Char('p') if app.editing.is_none() => {
            match app.wizard_step {
                WizardStep::ServerSelection => {
                    // Already at first step, do nothing
                }
                WizardStep::ClientSelection => {
                    // Go back to step 1
                    app.wizard_step = WizardStep::ServerSelection;
                    app.active_panel = Panel::ServiceList;
                    let selected_count = app.services.iter().filter(|s| s.selected).count();
                    app.message = format!(
                        "STEP 1: Server Detection - {} servers selected | Space: toggle | n: next step",
                        selected_count
                    );
                }
                WizardStep::Confirmation => {
                    // Go back to step 2
                    app.wizard_step = WizardStep::ClientSelection;
                    app.active_panel = Panel::ServiceList;
                    let client_count = app.clients.len();
                    app.message = format!(
                        "STEP 2: Client Detection - {} clients found | Space: toggle | n: next step | p: previous",
                        client_count
                    );
                }
            }
        }

        // Add new service with 'a' key
        KeyCode::Char('a') if app.editing.is_none() && app.active_panel == Panel::ServiceList && app.wizard_step == WizardStep::ServerSelection => {
            let new_name = format!("new-service-{}", app.services.len() + 1);
            app.services.push(ServiceEntry {
                name: new_name,
                config: default_server_config(),
                health: HealthStatus::Unknown,
                dirty: true,
                source: ServiceSource::Config,
                pid: None,
                selected: true,
            });
            app.selected_service = app.services.len() - 1;
            load_service_to_form(app);
            app.message = "New service added. Edit in the right panel.".into();
        }

        // Refresh health with 'r' key (must be before general Char(c) handler)
        KeyCode::Char('r') if app.editing.is_none() => {
            for svc in &mut app.services {
                svc.health = check_health(&svc.config);
            }
            app.message = "Health checks refreshed".into();
        }

        // Backspace in edit mode
        KeyCode::Backspace => {
            if let Some(field) = app.editing {
                mutate_field(&mut app.form, field, |s| {
                    s.pop();
                });
            }
        }

        // Character input in edit mode (must be last among Char handlers)
        KeyCode::Char(c) => {
            if let Some(field) = app.editing {
                mutate_field(&mut app.form, field, |s| s.push(c));
            }
        }

        _ => {}
    }

    Ok(false)
}

fn handle_confirm_dialog_key(app: &mut AppState, key: crossterm::event::KeyEvent) -> Result<bool> {
    // Order of choices for navigation: SaveAll, SaveMuxOnly, CopyToClipboard, Back, Exit
    let choices = [
        ConfirmChoice::SaveAll,
        ConfirmChoice::SaveMuxOnly,
        ConfirmChoice::CopyToClipboard,
        ConfirmChoice::Back,
        ConfirmChoice::Exit,
    ];
    let current_idx = choices.iter().position(|c| *c == app.confirm_choice).unwrap_or(0);

    match key.code {
        KeyCode::Left => {
            let new_idx = if current_idx == 0 { choices.len() - 1 } else { current_idx - 1 };
            app.confirm_choice = choices[new_idx];
        }
        KeyCode::Right => {
            let new_idx = (current_idx + 1) % choices.len();
            app.confirm_choice = choices[new_idx];
        }
        KeyCode::Enter => {
            match app.confirm_choice {
                ConfirmChoice::SaveAll => {
                    if !app.dry_run {
                        persist_all(app)?;
                        // TODO: Also rewire selected clients
                    }
                    app.message = if app.dry_run {
                        "Dry run: config would be saved. Exiting...".into()
                    } else {
                        "Configuration saved!".into()
                    };
                    return Ok(true);
                }
                ConfirmChoice::SaveMuxOnly => {
                    if !app.dry_run {
                        persist_all(app)?;
                    }
                    app.message = if app.dry_run {
                        "Dry run: mux config would be saved. Exiting...".into()
                    } else {
                        "Mux configuration saved!".into()
                    };
                    return Ok(true);
                }
                ConfirmChoice::CopyToClipboard => {
                    // TODO: Copy config to clipboard
                    app.message = "Config copied to clipboard (not yet implemented)".into();
                }
                ConfirmChoice::Back => {
                    app.active_panel = Panel::Editor;
                    update_message(app);
                }
                ConfirmChoice::Exit => {
                    return Ok(true);
                }
            }
        }
        KeyCode::Esc => {
            app.active_panel = Panel::Editor;
            update_message(app);
        }
        _ => {}
    }
    Ok(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper functions
// ─────────────────────────────────────────────────────────────────────────────

fn sync_form_to_service(app: &mut AppState) {
    if app.form.dirty {
        let idx = app.selected_service;
        if idx < app.services.len() {
            app.services[idx].name = app.form.service_name.clone();
            app.services[idx].config = service_from_form(&app.form);
            app.services[idx].dirty = true;
        }
        app.form.dirty = false;
    }
}

fn load_service_to_form(app: &mut AppState) {
    let idx = app.selected_service;
    if idx < app.services.len() {
        app.form = form_from_service(&app.services[idx]);
    }
}

fn update_message(app: &mut AppState) {
    app.message = match app.active_panel {
        Panel::ServiceList => "Tab: switch | Up/Down: select | Enter: edit | n: new | r: refresh health | s: save | q: quit".into(),
        Panel::Editor => "Tab: switch | Up/Down: navigate | Enter: edit field | Space: toggle tray | Esc: stop edit | s: save".into(),
        Panel::ConfirmDialog => "Left/Right: select | Enter: confirm | Esc: cancel".into(),
    };
}

fn mutate_field<F: FnOnce(&mut String)>(form: &mut FormState, field: Field, f: F) {
    let target = match field {
        Field::ServiceName => &mut form.service_name,
        Field::Socket => &mut form.socket,
        Field::Cmd => &mut form.cmd,
        Field::Args => &mut form.args,
        Field::MaxClients => &mut form.max_clients,
        Field::LogLevel => &mut form.log_level,
        Field::Tray => return,
    };
    f(target);
    form.dirty = true;
}

fn previous_field(current: Field) -> Field {
    match current {
        Field::ServiceName => Field::Tray,
        Field::Socket => Field::ServiceName,
        Field::Cmd => Field::Socket,
        Field::Args => Field::Cmd,
        Field::MaxClients => Field::Args,
        Field::LogLevel => Field::MaxClients,
        Field::Tray => Field::LogLevel,
    }
}

fn next_field(current: Field) -> Field {
    match current {
        Field::ServiceName => Field::Socket,
        Field::Socket => Field::Cmd,
        Field::Cmd => Field::Args,
        Field::Args => Field::MaxClients,
        Field::MaxClients => Field::LogLevel,
        Field::LogLevel => Field::Tray,
        Field::Tray => Field::ServiceName,
    }
}

fn previous_confirm_choice(current: ConfirmChoice) -> ConfirmChoice {
    match current {
        ConfirmChoice::SaveAll => ConfirmChoice::Exit,
        ConfirmChoice::SaveMuxOnly => ConfirmChoice::SaveAll,
        ConfirmChoice::CopyToClipboard => ConfirmChoice::SaveMuxOnly,
        ConfirmChoice::Back => ConfirmChoice::CopyToClipboard,
        ConfirmChoice::Exit => ConfirmChoice::Back,
    }
}

fn next_confirm_choice(current: ConfirmChoice) -> ConfirmChoice {
    match current {
        ConfirmChoice::SaveAll => ConfirmChoice::SaveMuxOnly,
        ConfirmChoice::SaveMuxOnly => ConfirmChoice::CopyToClipboard,
        ConfirmChoice::CopyToClipboard => ConfirmChoice::Back,
        ConfirmChoice::Back => ConfirmChoice::Exit,
        ConfirmChoice::Exit => ConfirmChoice::SaveAll,
    }
}

fn update_step2_message(app: &mut AppState) {
    let selected_count = app.clients.iter().filter(|c| c.selected).count();
    let total_count = app.clients.len();
    app.message = format!(
        "STEP 2: {} of {} clients selected | Space: toggle | n: next step | p: previous",
        selected_count, total_count
    );
}

fn execute_confirm_choice(app: &mut AppState) -> Result<bool> {
    match app.confirm_choice {
        ConfirmChoice::SaveAll => {
            if !app.dry_run {
                // Save mux config
                persist_all(app)?;
                // Rewire selected clients
                rewire_selected_clients(app)?;
            }
            app.message = if app.dry_run {
                "Dry run: config would be saved and clients rewired. Exiting...".into()
            } else {
                "Configuration saved and clients rewired!".into()
            };
            Ok(true)
        }
        ConfirmChoice::SaveMuxOnly => {
            if !app.dry_run {
                persist_all(app)?;
            }
            app.message = if app.dry_run {
                "Dry run: mux config would be saved. Exiting...".into()
            } else {
                "Mux configuration saved!".into()
            };
            Ok(true)
        }
        ConfirmChoice::CopyToClipboard => {
            // Build config string for clipboard
            let cfg = build_config_for_export(app);
            if let Ok(text) = toml::to_string_pretty(&cfg) {
                // Try to copy to clipboard using pbcopy on macOS
                if let Ok(mut child) = std::process::Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
                    use std::io::Write;
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                    app.message = "Configuration copied to clipboard!".into();
                } else {
                    app.message = "Failed to copy to clipboard (pbcopy not available)".into();
                }
            } else {
                app.message = "Failed to serialize configuration".into();
            }
            Ok(false)
        }
        ConfirmChoice::Back => {
            // Go back to step 2
            app.wizard_step = WizardStep::ClientSelection;
            app.active_panel = Panel::ServiceList;
            update_step2_message(app);
            Ok(false)
        }
        ConfirmChoice::Exit => {
            Ok(true)
        }
    }
}

fn build_config_for_export(app: &AppState) -> Config {
    let mut cfg = Config::default();
    for svc in &app.services {
        if svc.selected {
            cfg.servers.insert(svc.name.clone(), svc.config.clone());
        }
    }
    cfg
}

fn rewire_selected_clients(app: &AppState) -> Result<()> {
    for client in &app.clients {
        if client.selected && !client.already_rewired {
            let host_file = HostFile {
                kind: client.kind.clone(),
                path: client.config_path.clone(),
                format: match client.config_path.extension().and_then(|e| e.to_str()) {
                    Some("toml") => crate::scan::HostFormat::Toml,
                    _ => crate::scan::HostFormat::Json,
                },
            };
            
            // Use the socket_dir from app state
            match rewire_host(&host_file, &app.socket_dir, "rmcp_mux_proxy", &[], false) {
                Ok(outcome) => {
                    if let Some(backup) = outcome.backup {
                        tracing::info!(
                            "Rewired {} (backup: {})",
                            client.config_path.display(),
                            backup.display()
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to rewire {}: {}",
                        client.config_path.display(),
                        e
                    );
                }
            }
        }
    }
    Ok(())
}

fn persist_all(app: &AppState) -> Result<()> {
    let expanded_path = expand_path(app.config_path.to_string_lossy());

    // Create parent directory if needed
    if let Some(parent) = expanded_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Build the config
    let mut cfg = Config::default();
    for svc in &app.services {
        cfg.servers.insert(svc.name.clone(), svc.config.clone());
    }

    // Serialize based on extension
    let serialized = match expanded_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => serde_json::to_string_pretty(&cfg)?,
        "yaml" | "yml" => serde_yaml::to_string(&cfg)?,
        _ => toml::to_string_pretty(&cfg)?,
    };

    // Create backup if file exists
    if expanded_path.exists() {
        let backup_path = expanded_path.with_extension("bak");
        std::fs::copy(&expanded_path, &backup_path)
            .with_context(|| format!("failed to create backup at {}", backup_path.display()))?;
    }

    // Write the config
    std::fs::write(&expanded_path, serialized)
        .with_context(|| format!("failed to write {}", expanded_path.display()))?;

    Ok(())
}
