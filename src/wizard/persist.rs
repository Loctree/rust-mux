//! Persistence and export functions for the wizard.
//!
//! Three flows live here:
//! - **Safe path** (`run_safe_generate`): build `~/.config/mux/{config.toml,
//!   mcp.json, mcp.toml}` and print per-client setup instructions. Never
//!   touches existing client configs.
//! - **Mux-only persist** (`persist_all`): legacy save-to-`~/.codex/mcp-mux.toml`.
//! - **Danger path** (`run_danger_auto_configure`): build a
//!   [`crate::danger::DangerPlan`], leave the alternate screen so the user
//!   sees the full preview, prompt for an explicit `CONFIRM` token, and
//!   only then mutate files.

use std::io::{Write, stdin, stdout};

use anyhow::{Context, Result};

use crate::config::{Config, expand_path, safe_copy};
use crate::danger::{
    DangerStatus, execute_plan, format_preview, plan_danger_rewrite, rollback_commands,
};
use crate::mux_gen::{
    build_mux_outputs, default_mux_dir, safe_path_instructions, write_mux_outputs,
};
use crate::scan::{HostFile, MergeOutcome};

use super::types::{AppState, ConfirmChoice, HealthCheckChoice, Panel, WizardStep};

// ─────────────────────────────────────────────────────────────────────────────
// Build a MergeOutcome for the safe / danger flows from the wizard's selected
// services and selected clients.
// ─────────────────────────────────────────────────────────────────────────────

fn build_merge_from_services(app: &AppState) -> MergeOutcome {
    use crate::scan::HostService;
    let mut services = Vec::new();
    for svc in &app.services {
        if !svc.selected {
            continue;
        }
        services.push(HostService {
            name: svc.name.clone(),
            command: svc.config.cmd.clone().unwrap_or_default(),
            args: svc.config.args.clone().unwrap_or_default(),
            socket: svc.config.socket.clone(),
            env: svc.config.env.clone(),
            enabled: None,
        });
    }
    MergeOutcome {
        services,
        conflicts: Vec::new(),
    }
}

fn build_host_files_from_clients(app: &AppState) -> Vec<HostFile> {
    app.clients
        .iter()
        .filter(|c| c.selected && c.config_exists)
        .map(|c| HostFile {
            kind: c.kind,
            path: c.config_path.clone(),
            format: c.format,
            schema: c.schema,
            confidence: c.confidence,
            writable: true,
            eligible_for_danger: c.eligible_for_danger,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Config building (legacy mux-only persist target)
// ─────────────────────────────────────────────────────────────────────────────

pub fn build_config_for_export(app: &AppState) -> Config {
    let mut cfg = Config::default();
    for svc in &app.services {
        if svc.selected {
            cfg.servers.insert(svc.name.clone(), svc.config.clone());
        }
    }
    cfg
}

// ─────────────────────────────────────────────────────────────────────────────
// Safe path: write `~/.config/mux/{config.toml,mcp.json,mcp.toml}`
// ─────────────────────────────────────────────────────────────────────────────

pub fn run_safe_generate(app: &AppState) -> Result<String> {
    let merge = build_merge_from_services(app);
    if merge.services.is_empty() {
        return Ok("No services selected — nothing generated.".into());
    }
    let mux_dir = default_mux_dir();
    let outputs = build_mux_outputs(&merge, &mux_dir, "rust-mux-proxy", &[])?;

    if app.dry_run {
        let mut summary = String::new();
        summary.push_str(&format!(
            "(dry-run) Would generate mux configs under {}:\n",
            outputs.mux_dir.display()
        ));
        summary.push_str(&format!("  - {}\n", outputs.config_toml_path.display()));
        summary.push_str(&format!("  - {}\n", outputs.mcp_json_path.display()));
        summary.push_str(&format!("  - {}\n", outputs.mcp_toml_path.display()));
        summary.push_str("Per-client commands would be printed on completion.");
        return Ok(summary);
    }

    write_mux_outputs(&outputs)?;

    // Print instructions to stdout (visible after the wizard exits its
    // alternate screen). Keep the in-TUI message short.
    println!();
    println!(
        "✅ rust-mux config generated at {}",
        outputs.mux_dir.display()
    );
    println!("   • Daemon truth : {}", outputs.config_toml_path.display());
    println!("   • Client JSON  : {}", outputs.mcp_json_path.display());
    println!("   • Client TOML  : {}", outputs.mcp_toml_path.display());
    println!();
    println!("Start the mux:");
    println!(
        "   rust-mux --config {}",
        outputs.config_toml_path.display()
    );
    println!();
    println!("Per-client setup instructions:");
    for inst in safe_path_instructions(&outputs) {
        println!("• {} ({})", inst.headline, inst.kind.as_label());
        for cmd in &inst.commands {
            println!("    {cmd}");
        }
        println!("    note: {}", inst.note);
        println!();
    }
    if !outputs.conflicts.is_empty() {
        println!(
            "⚠️  {} server-name conflict(s) surfaced — review the config.toml entries.",
            outputs.conflicts.len()
        );
        println!();
    }

    Ok(format!(
        "Generated {} mux files at {} ({} services)",
        3,
        outputs.mux_dir.display(),
        outputs.services.len()
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Danger path: rewrite existing client configs.
// ─────────────────────────────────────────────────────────────────────────────

/// Danger flow. Caller is responsible for leaving the TUI's alternate
/// screen and disabling raw mode before invoking this; the `_unused_sink`
/// argument is here only so `wizard/mod.rs` can keep a single signature
/// across pending actions if it later needs a real writer.
pub fn run_danger_auto_configure<W: std::io::Write>(
    app: &AppState,
    _unused_sink: &mut W,
) -> Result<String> {
    let merge = build_merge_from_services(app);
    if merge.services.is_empty() {
        return Ok("No services selected — danger flow has nothing to do.".into());
    }
    let sources = build_host_files_from_clients(app);
    if sources.is_empty() {
        return Ok("No selected clients have an existing config; nothing to rewrite.".into());
    }

    let plan = plan_danger_rewrite(
        &sources,
        "rust-mux-proxy",
        &[],
        &expand_path("~/.config/mux/sockets"),
    );

    let preview = format_preview(&plan);
    println!("\n{preview}");

    let any_planned = plan
        .actions
        .iter()
        .any(|a| matches!(a.status, DangerStatus::Planned));
    if !any_planned {
        println!("(no files planned for change — nothing to confirm)\n");
        return Ok("No eligible files to rewrite.".into());
    }

    if app.dry_run {
        println!("(dry-run) plan above would have been executed; no files modified.\n");
        return Ok("Dry-run: danger plan rendered, no writes performed.".into());
    }

    println!(
        "Type CONFIRM (uppercase) and press Enter to apply the rewrite, anything else to cancel:"
    );
    print!("> ");
    let _ = stdout().flush();
    let mut input = String::new();
    stdin()
        .read_line(&mut input)
        .context("read confirmation prompt")?;

    if input.trim() != "CONFIRM" {
        println!("Cancelled — no files modified.\n");
        return Ok("Danger flow cancelled by operator.".into());
    }

    let outcomes = execute_plan(&plan, true)?;

    println!("\nResults:");
    let mut written = 0usize;
    for o in &outcomes {
        match &o.status {
            DangerStatus::Planned if o.written => {
                written += 1;
                let backup = o
                    .backup
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".into());
                println!("  ✓ wrote {} (backup: {})", o.path.display(), backup);
            }
            other => {
                let err = o.error.as_deref().unwrap_or("");
                println!(
                    "  · {} skipped ({:?}){}",
                    o.path.display(),
                    other,
                    if err.is_empty() {
                        String::new()
                    } else {
                        format!(": {err}")
                    }
                );
            }
        }
    }

    let rollback = rollback_commands(&outcomes);
    if !rollback.is_empty() {
        println!("\nRollback (paste any line to restore that file):");
        for cmd in &rollback {
            println!("  {cmd}");
        }
    }

    Ok(format!(
        "Danger flow applied to {written} file(s); see terminal for details."
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Legacy mux-only persistence (writes to AppState.config_path).
// ─────────────────────────────────────────────────────────────────────────────

pub fn persist_all(app: &AppState) -> Result<()> {
    let expanded_path = expand_path(app.config_path.to_string_lossy());

    if let Some(parent) = expanded_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut cfg = Config::default();
    for svc in &app.services {
        cfg.servers.insert(svc.name.clone(), svc.config.clone());
    }

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

    if expanded_path.exists() {
        let backup_path = expanded_path.with_extension("bak");
        safe_copy(&expanded_path, &backup_path)?;
    }

    std::fs::write(&expanded_path, serialized)
        .with_context(|| format!("failed to write {}", expanded_path.display()))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Confirm choice execution (called from the in-step Enter handler)
//
// Note: the alt-screen-leave dance for the danger flow happens inside
// `run_danger_auto_configure`. The UI handler in `keys.rs` calls into this
// function only for the choices that stay inside the TUI; danger is
// dispatched separately so the borrow on the terminal can be threaded in.
// ─────────────────────────────────────────────────────────────────────────────

pub fn execute_confirm_choice(app: &mut AppState) -> Result<bool> {
    match app.confirm_choice {
        ConfirmChoice::SafeGenerate => {
            let summary = run_safe_generate(app)?;
            app.message = summary;
            app.wizard_step = WizardStep::HealthCheck;
            app.active_panel = Panel::ServiceList;
            app.health_choice = HealthCheckChoice::Ok;
            Ok(false)
        }
        ConfirmChoice::SaveMuxOnly => {
            if !app.dry_run {
                persist_all(app)?;
            }
            app.wizard_step = WizardStep::HealthCheck;
            app.active_panel = Panel::ServiceList;
            app.health_choice = HealthCheckChoice::Ok;
            app.message = if app.dry_run {
                "STEP 4: Health Check (dry-run) — mux config would have been saved.".into()
            } else {
                "STEP 4: Health Check — mux config saved.".into()
            };
            Ok(false)
        }
        ConfirmChoice::CopyToClipboard => {
            let cfg = build_config_for_export(app);
            if let Ok(text) = toml::to_string_pretty(&cfg) {
                if let Ok(mut child) = std::process::Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
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
        ConfirmChoice::DangerAutoConfigure => {
            // Dispatched from `keys.rs` which has access to the terminal.
            // Reaching this branch means the caller forgot to special-case
            // it: surface a clear error so we never silently no-op on a
            // dangerous path.
            Err(anyhow::anyhow!(
                "DangerAutoConfigure must be dispatched via keys.rs, not execute_confirm_choice"
            ))
        }
        ConfirmChoice::Back => {
            app.wizard_step = WizardStep::ClientSelection;
            app.active_panel = Panel::ServiceList;
            let selected_count = app.clients.iter().filter(|c| c.selected).count();
            let total_count = app.clients.len();
            app.message = format!(
                "STEP 2: {} of {} clients selected | Space: toggle | n: next step | p: previous",
                selected_count, total_count
            );
            Ok(false)
        }
        ConfirmChoice::Exit => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::wizard::types::FormState;
    use crate::wizard::types::{
        AppState, ConfirmChoice, Field, HealthCheckChoice, HealthStatus, Panel, ServiceEntry,
        ServiceSource, WizardStep,
    };
    use tempfile::tempdir;

    fn make_app(tmp: &std::path::Path) -> AppState {
        AppState {
            wizard_step: WizardStep::Confirmation,
            config_path: tmp.join("mcp-mux.toml"),
            services: vec![ServiceEntry {
                name: "memory".into(),
                config: ServerConfig {
                    socket: Some(
                        tmp.join("sockets/memory.sock")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    cmd: Some("npx".into()),
                    args: Some(vec!["@modelcontextprotocol/server-memory".into()]),
                    env: None,
                    max_active_clients: Some(5),
                    tray: Some(false),
                    service_name: Some("memory".into()),
                    log_level: Some("info".into()),
                    lazy_start: Some(false),
                    max_request_bytes: Some(1_048_576),
                    request_timeout_ms: Some(30_000),
                    restart_backoff_ms: Some(1_000),
                    restart_backoff_max_ms: Some(30_000),
                    max_restarts: Some(5),
                    status_file: None,
                    heartbeat_interval_ms: Some(30_000),
                    heartbeat_timeout_ms: Some(30_000),
                    heartbeat_max_failures: Some(3),
                    heartbeat_enabled: Some(true),
                },
                health: HealthStatus::Unknown,
                dirty: false,
                source: ServiceSource::Config,
                pid: None,
                selected: true,
            }],
            selected_service: 0,
            clients: Vec::new(),
            selected_client: 0,
            form: FormState::default(),
            current_field: Field::ServiceName,
            editing: None,
            active_panel: Panel::ConfirmDialog,
            confirm_choice: ConfirmChoice::SafeGenerate,
            health_choice: HealthCheckChoice::Ok,
            message: String::new(),
            dry_run: false,
            pending_action: None,
        }
    }

    #[test]
    fn safe_generate_dry_run_does_not_write_files() {
        let dir = tempdir().expect("tempdir");
        let mut app = make_app(dir.path());
        app.dry_run = true;
        // override mux dir indirectly by HOME isn't easy here — we accept the
        // real default path is computed; the dry-run branch must short-circuit
        // before touching disk.
        let summary = run_safe_generate(&app).expect("safe-gen dry");
        assert!(summary.contains("dry-run") || summary.contains("Would"));
    }

    #[test]
    fn safe_generate_with_no_services_selected_is_noop() {
        let dir = tempdir().expect("tempdir");
        let mut app = make_app(dir.path());
        app.services[0].selected = false;
        let summary = run_safe_generate(&app).expect("safe-gen empty");
        assert!(summary.contains("No services"));
    }

    #[test]
    fn execute_confirm_safe_generate_routes_to_health_check_in_dry_run() {
        let dir = tempdir().expect("tempdir");
        let mut app = make_app(dir.path());
        app.dry_run = true;
        app.confirm_choice = ConfirmChoice::SafeGenerate;
        let exit = execute_confirm_choice(&mut app).expect("safe gen confirm");
        assert!(!exit);
        assert_eq!(app.wizard_step, WizardStep::HealthCheck);
    }
}
