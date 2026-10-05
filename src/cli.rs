use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::{
    AgentStatus, ClientWindowTitleSetParams, EmptyParams, Method, PaneAgentState, ReadFormat,
    ReadSource, Request, SplitDirection,
};

macro_rules! print {
    ($($arg:tt)*) => {{
        crate::platform::begin_cli_output();
        std::print!($($arg)*);
    }};
}

macro_rules! println {
    ($($arg:tt)*) => {{
        crate::platform::begin_cli_output();
        std::println!($($arg)*);
    }};
}

// CLI messages are written with the upstream `herdr ` command prefix, which is
// kept as-is so upstream edits to them still merge. The prefix is swapped for
// this binary's real name when printed, so a usage line tells the user a
// command they can actually run. Only a leading `herdr ` and `` `herdr `` are
// rewritten; `herdr:` source tags, `herdr.dev` and `herdrdev/` are left alone.
macro_rules! eprintln {
    () => {{
        std::eprintln!();
    }};
    ($($arg:tt)*) => {{
        std::eprintln!("{}", crate::cli::command_name_text(&format!($($arg)*)));
    }};
}

pub(crate) fn command_name_text(text: &str) -> std::borrow::Cow<'_, str> {
    let name = crate::build_info::COMMAND_NAME;
    if name == "herdr" || !text.contains("herdr ") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("herdr ") {
        let starts_word = at == 0
            || !rest[..at].chars().next_back().is_some_and(|c| {
                c.is_alphanumeric() || c == '_' || c == '-' || c == '/' || c == '.'
            });
        out.push_str(&rest[..at]);
        out.push_str(if starts_word { name } else { "herdr" });
        out.push(' ');
        rest = &rest[at + "herdr ".len()..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

mod agent;
mod api;
mod completion;
mod integration;
mod machine;
mod notification;
mod pane;
mod plugin;
mod protocol_guard;
mod runtime;
mod server;
mod server_not_running;
mod spec;
mod stats;
mod status;
mod tab;
mod target;
mod workspace;
mod worktree;

const TERMINAL_SESSION_OBSERVE_USAGE: &str =
    "usage: herdr terminal session observe <target> [--cols N] [--rows N]";
const TERMINAL_SESSION_CONTROL_USAGE: &str =
    "usage: herdr terminal session control <target> [--takeover] [--cols N] [--rows N]";
pub(crate) const AGENT_HELP_FOOTER: &str = concat!(
    "Are you an AI? Use these resources ONLY IF your task specifically asks you to:\n",
    "  Help a human understand or set up Herdr for the first time:\n",
    "    https://herdr.dev/agent-guide.md\n",
    "  Debug or investigate a problem with Herdr:\n",
    "    https://herdr.dev/llms.txt\n",
    "  Control Herdr panes, agents, or workspaces:\n",
    "    SKIP if a Herdr skill is already in your context. Otherwise run: herdr --skill",
);

pub(crate) fn parse_token_assignment(raw: &str) -> Result<(String, Option<String>), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("token must use NAME=VALUE".into());
    };
    if key.is_empty() {
        return Err("token name must not be empty".into());
    }
    Ok((key.to_string(), Some(value.to_string())))
}

pub(crate) fn parse_env_assignment(raw: &str) -> Result<(String, String), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("env must use KEY=VALUE".into());
    };
    if key.is_empty() {
        return Err("env key must not be empty".into());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("env must not contain NUL bytes".into());
    }
    Ok((key.to_string(), value.to_string()))
}

pub enum CommandOutcome {
    Handled(i32),
    NotCli,
}

pub(super) fn print_read_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{response}");
        return Ok(1);
    }
    if let Some(text) = response["result"]["read"]["text"].as_str() {
        print!("{text}");
    }
    Ok(0)
}

pub(crate) fn maybe_run_machine(args: &[String]) -> Option<std::io::Result<CommandOutcome>> {
    target::maybe_run(args)
}

pub fn maybe_run(args: &[String]) -> std::io::Result<CommandOutcome> {
    let Some(command) = args.get(1).map(|arg| arg.as_str()) else {
        return Ok(CommandOutcome::NotCli);
    };

    if spec::print_requested_help(args)? {
        return Ok(CommandOutcome::Handled(0));
    }

    let exit_code = match command {
        "server" => {
            let Some(exit_code) = server::run_server_command(&args[2..])? else {
                return Ok(CommandOutcome::NotCli);
            };
            exit_code
        }
        "api" => api::run_api_command(&args[2..])?,
        "status" => status::run_status_command(&args[2..])?,
        "stats" => stats::run_stats_command(&args[2..])?,
        "completion" | "completions" => completion::run_completion_command(&args[2..])?,
        "config" => run_config_command(&args[2..])?,
        "channel" => run_channel_command(&args[2..])?,
        "machine" => machine::run_machine_command(&args[2..])?,
        "workspace" => workspace::run_workspace_command(&args[2..])?,
        "worktree" => worktree::run_worktree_command(&args[2..])?,
        "tab" => tab::run_tab_command(&args[2..])?,
        "notification" => notification::run_notification_command(&args[2..])?,
        "agent" => agent::run_agent_command(&args[2..])?,
        "terminal" => run_terminal_command(&args[2..])?,
        "pane" => pane::run_pane_command(&args[2..])?,
        "plugin" => plugin::run_plugin_command(&args[2..])?,
        "integration" => integration::run_integration_command(&args[2..])?,
        "session" => run_session_command(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    Ok(CommandOutcome::Handled(exit_code))
}

fn run_channel_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("set") => channel_set(&args[1..]),
        Some("show") if args.len() == 1 => {
            let config = crate::config::Config::load().config;
            println!("{}", config.update.channel.as_str());
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            print_channel_help();
            Ok(0)
        }
        _ => {
            print_channel_help();
            Ok(2)
        }
    }
}

fn channel_set(args: &[String]) -> std::io::Result<i32> {
    let Some(channel) = parse_channel_set_arg(args) else {
        eprintln!("usage: herdr channel set <stable|preview>");
        return Ok(2);
    };

    if let Some(reason) = channel_set_rejection(
        channel,
        crate::update::preview_channel_rejection_for_current_install(),
    ) {
        eprintln!("{reason}.");
        return Ok(1);
    }

    let path = crate::config::config_path();
    let content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if let Err(err) = content.parse::<toml::Value>() {
        eprintln!(
            "config file at {} is invalid TOML: {err}. Fix it before changing the update channel.",
            path.display()
        );
        return Ok(1);
    }

    let updated = crate::config::upsert_section_value(
        &content,
        "update",
        "channel",
        &format!("\"{channel}\""),
    );
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "changing the update channel would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)?;
    println!(
        "Herdr update channel set to {channel} in {}.",
        path.display()
    );

    match channel_set_install_action(
        crate::update::package_manager_channel_update_guidance_for_current_install(),
    ) {
        ChannelSetInstallAction::PrintGuidance(guidance) => {
            println!("{guidance}");
            return Ok(0);
        }
        ChannelSetInstallAction::RunSelfUpdate => {}
    }

    crate::platform::end_cli_output();
    if let Err(err) = crate::update::self_update(crate::update::SelfUpdateOptions::default()) {
        eprintln!("update failed: {err}");
        eprintln!("Run `herdr update` to retry.");
        return Ok(1);
    }

    Ok(0)
}

fn parse_channel_set_arg(args: &[String]) -> Option<&str> {
    let channel = args.first().map(|arg| arg.as_str())?;
    if args.len() == 1 && matches!(channel, "stable" | "preview") {
        Some(channel)
    } else {
        None
    }
}

fn channel_set_rejection(
    channel: &str,
    install_rejection: Option<&'static str>,
) -> Option<&'static str> {
    if channel == "preview" {
        return install_rejection;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelSetInstallAction {
    RunSelfUpdate,
    PrintGuidance(&'static str),
}

fn channel_set_install_action(
    package_manager_guidance: Option<&'static str>,
) -> ChannelSetInstallAction {
    match package_manager_guidance {
        Some(guidance) => ChannelSetInstallAction::PrintGuidance(guidance),
        None => ChannelSetInstallAction::RunSelfUpdate,
    }
}

fn print_channel_help() {
    eprintln!("herdr channel commands:");
    eprintln!("  herdr channel show                  print the configured update channel");
    eprintln!("  herdr channel set <stable|preview>  choose the update channel");
}

fn run_config_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_config_help();
        return Ok(2);
    };

    match subcommand {
        "check" => config_check(&args[1..]),
        "reset-keys" => config_reset_keys(&args[1..]),
        "help" | "--help" | "-h" => {
            print_config_help();
            Ok(0)
        }
        _ => {
            print_config_help();
            Ok(2)
        }
    }
}

fn config_check(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {}
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            eprintln!("usage: herdr config check");
            return Ok(0);
        }
        _ => {
            eprintln!("usage: herdr config check");
            return Ok(2);
        }
    }

    let diagnostics = crate::config::Config::load().diagnostics;
    if diagnostics.is_empty() {
        println!("config: ok");
    } else {
        println!("config: issues found");
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
    }

    Ok(i32::from(!diagnostics.is_empty()))
}

fn config_reset_keys(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr config reset-keys");
        return Ok(2);
    }

    let path = crate::config::config_path();
    if !path.exists() {
        println!(
            "No config file found at {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let content = std::fs::read_to_string(&path)?;
    let parsed = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "config file at {} is invalid TOML: {err}. Fix it manually or move it aside to use defaults.",
                path.display()
            );
            return Ok(1);
        }
    };
    let Some(table) = parsed.as_table() else {
        eprintln!(
            "config file at {} is invalid TOML: top-level config must be a table.",
            path.display()
        );
        return Ok(1);
    };

    if !table.contains_key("keys") {
        println!(
            "No [keys] config found in {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let (updated, removed) = crate::config::remove_keybinding_config_sections(&content);
    if !removed {
        eprintln!(
            "could not safely remove keybinding config from {} without rewriting comments; edit the file manually or remove the top-level keys setting.",
            path.display()
        );
        return Ok(1);
    }
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "removing keybinding config would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }

    let backup_path = key_config_backup_path(&path);
    std::fs::copy(&path, &backup_path)?;
    std::fs::write(&path, updated)?;

    println!("Created backup: {}", backup_path.display());
    println!(
        "Removed [keys], [keys.indexed], and [[keys.command]] from {}.",
        path.display()
    );
    println!("Built-in v2 keybindings will apply after Herdr restarts or reloads config.");
    println!("If a Herdr server is running, run `herdr server reload-config` to apply this now.");
    println!(
        "To restore: cp {} {}",
        backup_path.display(),
        path.display()
    );
    Ok(0)
}

fn key_config_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!("{file_name}.bak-keybind-v2-{timestamp}"))
}

fn run_terminal_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_terminal_help();
        return Ok(2);
    };

    match subcommand {
        "attach" => terminal_attach(&args[1..]),
        "session" => terminal_session(&args[1..]),
        "title" => terminal_title(&args[1..]),
        "help" | "--help" | "-h" => {
            print_terminal_help();
            Ok(0)
        }
        _ => {
            print_terminal_help();
            Ok(2)
        }
    }
}

fn run_session_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_session_help();
        return Ok(2);
    };

    match subcommand {
        "list" => session_list(&args[1..]),
        "import" => session_import(&args[1..]),
        "imported" => session_imported(&args[1..]),
        "materialise" | "materialize" => session_materialise(&args[1..]),
        "attach" => session_attach_help(&args[1..]),
        "stop" => session_stop(&args[1..]),
        "delete" => session_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_session_help();
            Ok(0)
        }
        _ => {
            print_session_help();
            Ok(2)
        }
    }
}

fn session_attach_help(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("usage: herdr session attach <name>");
        return Ok(0);
    }
    eprintln!("usage: herdr session attach <name>");
    Ok(2)
}

fn session_list(args: &[String]) -> std::io::Result<i32> {
    let json = match parse_session_json_only(args, "usage: herdr session list [--json]") {
        Ok(json) => json,
        Err(code) => return Ok(code),
    };

    let sessions = crate::session::list_sessions()?;
    if json {
        _print_json(&serde_json::json!({
            "sessions": sessions,
        }));
    } else {
        print_session_table(&sessions);
    }
    Ok(0)
}

fn session_stop(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session stop <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let target = match crate::session::parse_target_name(&name) {
        Ok(target) => target,
        Err(message) => {
            print_session_error("invalid_session_name", &message);
            return Ok(1);
        }
    };
    match crate::session::stop_session(target.as_deref()) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "stopped": true,
                    "session": session,
                }));
            } else {
                println!("stopped session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_stop_failed", &message);
            Ok(1)
        }
    }
}

fn session_delete(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session delete <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    match crate::session::delete_session(&name) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "deleted": true,
                    "session": session,
                }));
            } else {
                println!("deleted session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_delete_failed", &message);
            Ok(1)
        }
    }
}

fn terminal_attach(args: &[String]) -> std::io::Result<i32> {
    let (terminal_id, takeover) = match parse_attach_target(
        args,
        "usage: herdr terminal attach <terminal_id> [--takeover]",
    ) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    crate::client::run_terminal_attach(terminal_id, takeover)?;
    Ok(0)
}

fn terminal_session(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("control") => terminal_session_control(&args[1..]),
        Some("observe") => terminal_session_observe(&args[1..]),
        Some("help" | "--help" | "-h") => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{TERMINAL_SESSION_CONTROL_USAGE}");
            eprintln!("{TERMINAL_SESSION_OBSERVE_USAGE}");
            Ok(2)
        }
    }
}

fn terminal_session_control(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_CONTROL_USAGE,
        "control",
        true,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_control(
        options.target,
        options.takeover,
        options.cols,
        options.rows,
    )?;
    Ok(0)
}

fn terminal_session_observe(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_terminal_session_options(
        args,
        TERMINAL_SESSION_OBSERVE_USAGE,
        "observe",
        false,
    )? {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };

    crate::client::run_terminal_session_observe(options.target, options.cols, options.rows)?;
    Ok(0)
}

struct TerminalSessionOptions {
    target: String,
    cols: u16,
    rows: u16,
    takeover: bool,
}

fn parse_terminal_session_options(
    args: &[String],
    usage: &str,
    command: &str,
    allow_takeover: bool,
) -> std::io::Result<Result<TerminalSessionOptions, i32>> {
    if matches!(
        args.first().map(|arg| arg.as_str()),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{usage}");
        return Ok(Err(0));
    }
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Ok(Err(2));
    };

    let mut cols = 120;
    let mut rows = 40;
    let mut takeover = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--takeover" if allow_takeover => {
                takeover = true;
                i += 1;
            }
            "--cols" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{usage}");
                    return Ok(Err(2));
                };
                cols = parse_terminal_dimension(value, "--cols")?;
                i += 2;
            }
            "--rows" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{usage}");
                    return Ok(Err(2));
                };
                rows = parse_terminal_dimension(value, "--rows")?;
                i += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Ok(Err(0));
            }
            other => {
                eprintln!("unknown terminal session {command} option: {other}");
                eprintln!("{usage}");
                return Ok(Err(2));
            }
        }
    }

    Ok(Ok(TerminalSessionOptions {
        target: target.clone(),
        cols,
        rows,
        takeover,
    }))
}

fn parse_terminal_dimension(raw: &str, flag: &str) -> std::io::Result<u16> {
    let parsed = raw.parse::<u16>().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} must be an integer between 1 and {}", u16::MAX),
        )
    })?;
    if parsed == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{flag} must be greater than 0"),
        ));
    }
    Ok(parsed)
}

fn terminal_title(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("set") => {
            if args.len() != 2 {
                eprintln!("usage: herdr terminal title set <title>");
                return Ok(2);
            }
            print_response(&send_request(&Request {
                id: "cli:terminal:title:set".into(),
                method: Method::ClientWindowTitleSet(ClientWindowTitleSetParams {
                    title: args[1].clone(),
                }),
            })?)
        }
        Some("clear") => {
            if args.len() != 1 {
                eprintln!("usage: herdr terminal title clear");
                return Ok(2);
            }
            print_response(&send_request(&Request {
                id: "cli:terminal:title:clear".into(),
                method: Method::ClientWindowTitleClear(EmptyParams::default()),
            })?)
        }
        Some("help" | "--help" | "-h") => {
            eprintln!("usage: herdr terminal title set <title>");
            eprintln!("       herdr terminal title clear");
            Ok(0)
        }
        _ => {
            eprintln!("usage: herdr terminal title set <title>");
            eprintln!("       herdr terminal title clear");
            Ok(2)
        }
    }
}

pub(super) fn parse_attach_target(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Err(2);
    };
    let mut takeover = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--takeover" => takeover = true,
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Err(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Err(2);
            }
        }
    }
    Ok((target.clone(), takeover))
}

pub(super) fn print_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(response).unwrap());
    Ok(0)
}

pub(super) fn send_ok_request(method: Method) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:request".into(),
        method,
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    Ok(0)
}

pub(super) fn send_request(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = target::api_client()?;
    ensure_server_protocol_compatible(&client, &request.id)?;
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

pub(super) fn send_request_unchecked(request: &Request) -> std::io::Result<serde_json::Value> {
    let client = target::api_client()?;
    client
        .request_value(request)
        .map_err(|err| map_server_not_running_or_io(err, &request.id, &client))
}

fn ensure_server_protocol_compatible(client: &ApiClient, request_id: &str) -> std::io::Result<()> {
    let status = client
        .status()
        .map_err(|err| map_server_not_running_or_io(err, request_id, client))?;
    let server_protocol = status
        .protocol
        .ok_or_else(|| std::io::Error::other("server ping did not include a protocol version"))?;
    let Some(response) =
        protocol_guard::mismatch_response(request_id, server_protocol, &target::restart_guidance())
    else {
        return Ok(());
    };

    eprintln!(
        "{}",
        serde_json::to_string(&response).map_err(std::io::Error::other)?
    );
    Err(protocol_guard::reported_error())
}

pub(crate) fn protocol_mismatch_was_reported(err: &std::io::Error) -> bool {
    protocol_guard::was_reported(err)
}

pub(crate) fn server_not_running_was_reported(err: &std::io::Error) -> bool {
    server_not_running::was_reported(err)
}

/// Returns the `ErrorResponse` carried by a `server_not_running` marker, if any,
/// so the edge that surfaces the error can print it exactly once (deferred
/// printing: recovering callers like plugin offline fallback print nothing).
pub(crate) fn server_not_running_reported_response(
    err: &std::io::Error,
) -> Option<&crate::api::schema::ErrorResponse> {
    server_not_running::reported_response(err)
}

/// True when an io::Error indicates nothing is listening on the API socket.
/// Classify by `ErrorKind` only: Windows named pipes surface different raw
/// errno values than Unix domain sockets but the same error kinds.
pub(super) fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Maps an `ApiClientError` from a socket command into the io::Error that
/// bubbles up to `main`. A dead-server connect failure is reported as a
/// friendly `server_not_running` JSON error plus a recognizable marker; all
/// other errors fall through unchanged so existing handling is preserved.
fn map_server_not_running_or_io(
    err: ApiClientError,
    request_id: &str,
    client: &ApiClient,
) -> std::io::Error {
    if target::is_remote() {
        return target::remote_error(api_client_error_to_io(err));
    }
    match err {
        ApiClientError::Io(io_err) if server_not_running_error(&io_err) => {
            server_not_running::reported_error(server_not_running::response(
                request_id,
                &client.socket_path(),
            ))
        }
        err => api_client_error_to_io(err),
    }
}

fn api_client_error_to_io(err: ApiClientError) -> std::io::Error {
    match err {
        ApiClientError::Io(err) => err,
        err => std::io::Error::other(err),
    }
}

pub(super) fn normalize_workspace_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_tab_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn normalize_pane_id(value: &str) -> String {
    value.to_string()
}

pub(super) fn parse_split_direction(value: &str) -> std::io::Result<SplitDirection> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err(std::io::Error::other(format!(
            "invalid split direction: {value}"
        ))),
    }
}

pub(super) fn parse_read_source(value: &str) -> std::io::Result<ReadSource> {
    match value {
        "visible" => Ok(ReadSource::Visible),
        "recent" => Ok(ReadSource::Recent),
        "recent-unwrapped" | "recent_unwrapped" => Ok(ReadSource::RecentUnwrapped),
        "detection" => Ok(ReadSource::Detection),
        _ => Err(std::io::Error::other(format!(
            "invalid read source: {value}"
        ))),
    }
}

pub(super) fn parse_read_format(value: &str) -> std::io::Result<ReadFormat> {
    match value {
        "text" => Ok(ReadFormat::Text),
        "ansi" => Ok(ReadFormat::Ansi),
        _ => Err(std::io::Error::other(format!(
            "invalid read format: {value}"
        ))),
    }
}

fn parse_agent_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "done" => Ok(AgentStatus::Done),
        "unknown" => Ok(AgentStatus::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, done, or unknown)"
        ))),
    }
}

pub(super) fn parse_pane_agent_state(value: &str) -> std::io::Result<PaneAgentState> {
    match value {
        "idle" => Ok(PaneAgentState::Idle),
        "working" => Ok(PaneAgentState::Working),
        "blocked" => Ok(PaneAgentState::Blocked),
        "unknown" => Ok(PaneAgentState::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid pane agent state: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

pub(super) fn parse_u32_flag(flag: &str, value: &str) -> std::io::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

pub(super) fn parse_u64_flag(flag: &str, value: &str) -> std::io::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

/// Expand `--flag=value` tokens into separate `--flag` and `value` tokens so
/// the hand-rolled subcommand parsers accept the same `--flag=value` form the
/// clap-generated help and completions imply. Only `value_options` are split:
/// boolean and unknown options keep their attached value so they still reach
/// the parser's unknown-option branch.
pub(super) fn expand_equals_args(args: &[String], value_options: &[&str]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(args.len());
    for arg in args {
        match arg.split_once('=') {
            Some((flag, value)) if value_options.contains(&flag) => {
                expanded.push(flag.to_string());
                expanded.push(value.to_string());
            }
            _ => expanded.push(arg.clone()),
        }
    }
    expanded
}

fn parse_session_json_only(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn parse_session_name_and_json(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let mut name = None;
    let mut json = false;
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
        } else if !options_ended && arg == "--json" {
            json = true;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("{usage}");
            return Err(2);
        }
    }

    let Some(name) = name else {
        eprintln!("{usage}");
        return Err(2);
    };
    Ok((name, json))
}

fn print_session_table(sessions: &[crate::session::SessionInfo]) {
    println!("{:<20} {:<8} {:<48} socket", "name", "status", "directory");
    for session in sessions {
        println!(
            "{:<20} {:<8} {:<48} {}",
            session.name,
            if session.running {
                "running"
            } else {
                "stopped"
            },
            session.session_dir,
            session.socket_path
        );
    }
}

fn print_session_error(code: &str, message: &str) {
    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .unwrap()
    );
}

fn print_config_help() {
    eprintln!("herdr config commands:");
    eprintln!("  herdr config check  validate config.toml and print diagnostics");
    eprintln!("  herdr config reset-keys  back up config.toml and remove custom keybindings");
}

fn print_terminal_help() {
    eprintln!("herdr terminal commands:");
    eprintln!("  herdr terminal attach <terminal_id> [--takeover]");
    eprintln!("  herdr terminal session control <target> [--takeover] [--cols N] [--rows N]");
    eprintln!("  herdr terminal session observe <target> [--cols N] [--rows N]");
    eprintln!("  herdr terminal title set <title>");
    eprintln!("  herdr terminal title clear");
    eprintln!("  detach from direct attach with ctrl+b q; send literal ctrl+b with ctrl+b ctrl+b");
}

fn print_session_help() {
    let name = crate::build_info::COMMAND_NAME;
    eprintln!("{name} session commands:");
    eprintln!("  {name} session list [--json]");
    eprintln!("  {name} session attach <name>");
    eprintln!("  {name} session stop <name> [--json]");
    eprintln!("  {name} session delete <name> [--json]");
    eprintln!("  {name} session import [--dry-run] [--json]   Import existing herdr lanes");
    eprintln!("  {name} session imported [--json]             List imported lanes");
    eprintln!("  {name} session materialise <NAME|UUID|--all> [--json]");
    eprintln!("      Open a pane for an imported lane and resume its agent there");
    eprintln!("  use 'default' as <name> to target the default session for stop");
}

/// Import upstream herdr lanes into this fork's own state.
///
/// Exit codes are deliberate: a partial import must never exit 0 quietly.
///   0 = every agent lane found was accounted for
///   1 = at least one lane was skipped, or nothing was found
///   2 = the upstream state could not be read at all
fn session_import(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|arg| arg == "--json");
    let dry_run = args.iter().any(|arg| arg == "--dry-run");

    let report = match crate::session_import::import_from_upstream(dry_run) {
        Ok(report) => report,
        Err(err) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type": "session_import", "error": err.to_string()})
                );
            } else {
                eprintln!("import failed: {err}");
            }
            return Ok(2);
        }
    };

    let accounted = report.imported + report.already_present + report.updated;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "type": "session_import",
                "dry_run": dry_run,
                "source": report.source,
                "found": report.found,
                "imported": report.imported,
                "already_present": report.already_present,
                "updated": report.updated,
                "accounted": accounted,
                "complete": report.is_complete(),
                "lost": report.lost(),
                "skipped": report.skipped
                    .iter()
                    .map(|skip| serde_json::json!({
                        "reason": skip.reason,
                        "agent_name": skip.agent_name,
                        "cwd": skip.cwd,
                    }))
                    .collect::<Vec<_>>(),
            })
        );
    } else {
        let name = crate::build_info::COMMAND_NAME;
        if dry_run {
            println!("dry run — nothing was written");
        }
        println!("source: {} (read-only; not modified)", report.source);
        println!(
            "agent lanes: {} of {} accounted for ({} new, {} already present, {} refreshed)",
            accounted, report.found, report.imported, report.already_present, report.updated
        );
        if !report.skipped.is_empty() {
            println!(
                "skipped {} ({} with a conversation to lose):",
                report.skipped.len(),
                report.lost()
            );
            for skip in &report.skipped {
                println!(
                    "  - {}{} [{}]: {}",
                    if skip.lost_conversation { "LOST " } else { "" },
                    skip.agent_name.as_deref().unwrap_or("(unnamed)"),
                    skip.cwd.as_deref().unwrap_or("(no cwd)"),
                    skip.reason
                );
            }
        }
        println!();
        println!("Imported lanes are RECORDS, not running processes.");
        println!("A live PTY cannot be transferred, so nothing here is attached to a");
        println!("running agent. Each lane carries its session uuid, so the SAME");
        println!("conversation continues by resuming it in a fresh pane:");
        println!("  {name} session imported        # see the resume command per lane");
        println!();
        println!("Close the herdr pane hosting a lane BEFORE resuming it here.");
        println!("Two processes appending to one transcript corrupts the conversation.");
    }

    if report.found == 0 || !report.is_complete() {
        return Ok(1);
    }
    Ok(0)
}

/// Open a real pane for an imported lane and resume its agent in it.
///
/// This is the half that turns a recorded lane back into a running
/// conversation: create a tab at the lane's recorded cwd, then start the agent
/// in that tab's pane with `--resume <uuid>` so it continues the SAME
/// transcript rather than beginning a new one.
///
/// # One owner at a time, enforced rather than documented
///
/// Before touching anything, this REFUSES any lane whose session uuid upstream
/// herdr still records against one of its panes. Two processes appending to one
/// transcript corrupts the conversation unrecoverably, so the check is a hard
/// gate, not a warning: the lane is skipped and the command exits non-zero.
///
/// It also refuses when upstream state cannot be read at all. Unreadable is
/// treated as "unknown", never as "free" — assuming a lane is free is exactly
/// the mistake that corrupts a transcript.
fn session_materialise(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|arg| arg == "--json");
    let all = args.iter().any(|arg| arg == "--all");
    let selector = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(String::as_str);

    if selector.is_none() && !all {
        eprintln!(
            "usage: {} session materialise <NAME|UUID|--all>",
            crate::build_info::COMMAND_NAME
        );
        return Ok(2);
    }

    let state = crate::session_import::load_imported()?;
    if state.lanes.is_empty() {
        eprintln!(
            "no imported lanes; run `{} session import` first",
            crate::build_info::COMMAND_NAME
        );
        return Ok(1);
    }

    let selected: Vec<_> = state
        .lanes
        .values()
        .filter(|lane| match selector {
            None => true,
            Some(want) => {
                lane.session_value == want
                    || lane.agent_name.as_deref() == Some(want)
                    || crate::session_import::sanitized_agent_name(lane) == want
            }
        })
        .cloned()
        .collect();

    if selected.is_empty() {
        eprintln!("no imported lane matches {:?}", selector.unwrap_or("--all"));
        return Ok(1);
    }

    // Unreadable upstream state is "unknown", not "free".
    let Some(held) = crate::session_import::upstream_held_sessions() else {
        eprintln!(
            "refusing to materialise: cannot read herdr's session state, so it is unknown \
             which sessions it still holds. Resuming a session herdr is hosting would put two \
             processes on one transcript."
        );
        return Ok(1);
    };

    // This fork's own live panes count as owners too: materialising a lane
    // twice would put two of OUR processes on one transcript, which is the
    // same corruption by a different route.
    // Our own live panes count as owners too: materialising a lane twice puts
    // two of OUR processes on one transcript, the same corruption by another
    // route.
    //
    // A census read that FAILS is not an empty census. Treating an error as
    // "nothing is held" is the shape that grants two callers one slot, because
    // the check is then blind to everything it could not observe. So a failed
    // read refuses rather than proceeding.
    let mut self_held: std::collections::BTreeMap<String, String> = Default::default();
    let census = send_request(&Request {
        id: "cli:session:materialise:list".into(),
        method: Method::AgentList(crate::api::schema::EmptyParams::default()),
    });
    let census = match census {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "refusing to materialise: cannot read this session's own agent list ({err}), so it \
                 is unknown which sessions are already running here. Resuming a session that is \
                 already live would put two processes on one transcript."
            );
            return Ok(1);
        }
    };
    {
        let value = census;
        if let Some(agents) = value
            .get("result")
            .and_then(|result| result.get("agents"))
            .and_then(|agents| agents.as_array())
        {
            for agent in agents {
                let pane = agent
                    .get("pane_id")
                    .and_then(|pane| pane.as_str())
                    .unwrap_or("unknown");
                // A live pane reports its agent_session only AFTER the agent
                // boots and its SessionStart hook fires. Between `agent start`
                // returning and that hook landing, a pane is already resuming a
                // uuid the census cannot yet name, so keying ownership solely
                // on agent_session is blind to exactly the window in which a
                // double-materialise would happen.
                //
                // The PANE record carries the same session id and exists from
                // pane creation, so it is read below as a second source and
                // closes that window.
                if let Some(session) = agent
                    .get("agent_session")
                    .and_then(|session| session.get("value"))
                    .and_then(|value| value.as_str())
                {
                    self_held.insert(session.to_string(), pane.to_string());
                }
            }
        }
    }

    // Second source: panes. A pane record carries agent_session as soon as the
    // pane knows it, which can precede the agent appearing in the agent census.
    // A failed read is again a refusal, not an empty result.
    match send_request(&Request {
        id: "cli:session:materialise:panes".into(),
        method: Method::PaneList(Default::default()),
    }) {
        Ok(value) => {
            if let Some(panes) = value
                .get("result")
                .and_then(|result| result.get("panes"))
                .and_then(|panes| panes.as_array())
            {
                for pane in panes {
                    let pane_id = pane
                        .get("pane_id")
                        .and_then(|pane_id| pane_id.as_str())
                        .unwrap_or("unknown");
                    if let Some(session) = pane
                        .get("agent_session")
                        .and_then(|session| session.get("value"))
                        .and_then(|value| value.as_str())
                    {
                        self_held
                            .entry(session.to_string())
                            .or_insert_with(|| pane_id.to_string());
                    }
                }
            }
        }
        Err(err) => {
            eprintln!(
                "refusing to materialise: cannot read this session's panes ({err}), so it is \
                 unknown which sessions are already running here."
            );
            return Ok(1);
        }
    }

    let mut opened = Vec::new();
    let mut refused = Vec::new();

    for lane in selected {
        if let Some(pane_id) = self_held.get(&lane.session_value) {
            refused.push((
                lane.clone(),
                crate::session_import::OwnershipBlock::AlreadyMaterialised {
                    pane_id: pane_id.clone(),
                }
                .to_string(),
            ));
            continue;
        }
        if let Some(agent_name) = held.get(&lane.session_value) {
            refused.push((
                lane.clone(),
                crate::session_import::OwnershipBlock::HeldByUpstream {
                    agent_name: agent_name.clone(),
                }
                .to_string(),
            ));
            continue;
        }

        let Some(argv) = lane.resume_argv() else {
            refused.push((
                lane.clone(),
                "no resume command is known for this lane".to_string(),
            ));
            continue;
        };
        // resume_argv() leads with the executable; the server supplies that.
        let agent_args: Vec<String> = argv.into_iter().skip(1).collect();
        let name = crate::session_import::sanitized_agent_name(&lane);

        let tab = match send_request(&Request {
            id: "cli:session:materialise:tab".into(),
            method: Method::TabCreate(crate::api::schema::TabCreateParams {
                workspace_id: None,
                cwd: Some(lane.cwd.display().to_string()),
                focus: false,
                label: Some(name.clone()),
                env: Default::default(),
            }),
        }) {
            Ok(value) => value,
            Err(err) => {
                refused.push((lane.clone(), format!("could not create a tab: {err}")));
                continue;
            }
        };

        let Some(pane_id) = tab
            .get("result")
            .and_then(|result| result.get("root_pane"))
            .and_then(|pane| pane.get("pane_id"))
            .and_then(|pane_id| pane_id.as_str())
        else {
            refused.push((
                lane.clone(),
                "tab was created but reported no pane".to_string(),
            ));
            continue;
        };

        match send_request(&Request {
            id: "cli:session:materialise:agent".into(),
            method: Method::AgentStart(crate::api::schema::AgentStartParams {
                name: name.clone(),
                kind: lane.agent.clone(),
                pane_id: pane_id.to_string(),
                args: agent_args,
                timeout_ms: None,
                // An imported lane has no recorded spawner: upstream's session
                // snapshot stores cwd, agent name and session id, but nothing
                // about which agent started it. Inventing an edge here would
                // put a wrong branch in the tree, which is worse than a root —
                // a lane at the root reads as "parent unknown", while a wrong
                // parent reads as fact. Materialised lanes therefore render as
                // roots until something that knows the real parentage records
                // it.
                parent_agent: None,
            }),
        }) {
            Ok(value) if value.get("error").is_none() => {
                opened.push((lane.clone(), name, pane_id.to_string()));
            }
            Ok(value) => {
                let message = value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(|message| message.as_str())
                    .unwrap_or("agent start failed")
                    .to_string();
                refused.push((lane.clone(), message));
            }
            Err(err) => refused.push((lane.clone(), format!("agent start failed: {err}"))),
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "type": "session_materialise",
                "opened": opened.iter().map(|(lane, name, pane)| serde_json::json!({
                    "session_value": lane.session_value,
                    "agent_name": name,
                    "pane_id": pane,
                    "cwd": lane.cwd,
                })).collect::<Vec<_>>(),
                "refused": refused.iter().map(|(lane, reason)| serde_json::json!({
                    "session_value": lane.session_value,
                    "agent_name": lane.agent_name,
                    "reason": reason,
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        for (lane, name, pane) in &opened {
            println!("materialised {name} at pane {pane}");
            println!("  resumed session {}", lane.session_value);
            println!("  cwd {}", lane.cwd.display());
        }
        for (lane, reason) in &refused {
            println!(
                "REFUSED {}: {reason}",
                lane.agent_name.as_deref().unwrap_or(&lane.session_value)
            );
        }
    }

    if !refused.is_empty() {
        return Ok(1);
    }
    Ok(0)
}

/// List lanes already imported, with the command that resumes each.
fn session_imported(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|arg| arg == "--json");
    let state = crate::session_import::load_imported()?;

    if json {
        let lanes = state
            .lanes
            .values()
            .map(|lane| {
                serde_json::json!({
                    "session_value": lane.session_value,
                    "source": lane.source,
                    "agent": lane.agent,
                    "agent_name": lane.agent_name,
                    "cwd": lane.cwd,
                    "workspace_label": lane.workspace_label,
                    "state": lane.state,
                    "resume_argv": lane.resume_argv(),
                    "transcript_found":
                        crate::session_import::transcript_exists(&lane.session_value),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::json!({
                "type": "session_imported",
                "count": lanes.len(),
                "lanes": lanes,
            })
        );
        return Ok(0);
    }

    if state.lanes.is_empty() {
        println!("no imported lanes");
        println!("run: {} session import", crate::build_info::COMMAND_NAME);
        return Ok(0);
    }

    println!(
        "{} imported lane(s), all parked (no running process):",
        state.lanes.len()
    );
    for lane in state.lanes.values() {
        let transcript = if crate::session_import::transcript_exists(&lane.session_value) {
            "transcript found"
        } else {
            "TRANSCRIPT NOT FOUND"
        };
        println!();
        println!(
            "  {}  [{}]",
            lane.agent_name.as_deref().unwrap_or("(unnamed)"),
            transcript
        );
        println!("    session: {}", lane.session_value);
        println!("    cwd:     {}", lane.cwd.display());
        if let Some(argv) = lane.resume_argv() {
            println!("    resume:  {}", argv.join(" "));
        }
    }
    Ok(0)
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}

#[cfg(test)]
mod command_name_text_tests {
    use super::command_name_text;

    #[test]
    fn a_usage_line_names_the_binary_that_is_running() {
        let name = crate::build_info::COMMAND_NAME;
        assert_eq!(
            command_name_text("usage: herdr workspace close <workspace_id>"),
            format!("usage: {name} workspace close <workspace_id>")
        );
        assert_eq!(
            command_name_text("run `herdr session attach work` again"),
            format!("run `{name} session attach work` again")
        );
        assert_eq!(
            command_name_text("herdr plugin commands:"),
            format!("{name} plugin commands:")
        );
    }

    #[test]
    fn data_and_upstream_names_are_left_alone() {
        for text in [
            "source herdr:claude",
            "see https://herdr.dev/docs",
            "github.com/herdrdev/herdr issues",
            "upstream-herdr binary",
            "no herdr here",
        ] {
            let expected = if text == "no herdr here" {
                format!("no {} here", crate::build_info::COMMAND_NAME)
            } else {
                text.to_string()
            };
            assert_eq!(command_name_text(text), expected, "{text}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_channel_set_argument() {
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string()]),
            Some("preview")
        );
        assert_eq!(
            super::parse_channel_set_arg(&["stable".to_string()]),
            Some("stable")
        );
        assert_eq!(super::parse_channel_set_arg(&["nightly".to_string()]), None);
        assert_eq!(
            super::parse_channel_set_arg(&["preview".to_string(), "stable".to_string()]),
            None
        );
    }

    #[test]
    fn channel_set_only_applies_package_rejection_to_preview() {
        assert_eq!(
            super::channel_set_rejection("preview", Some("no preview")),
            Some("no preview")
        );
        assert_eq!(
            super::channel_set_rejection("stable", Some("no preview")),
            None
        );
        assert_eq!(super::channel_set_rejection("preview", None), None);
    }

    #[test]
    fn channel_set_skips_self_update_for_package_manager_guidance() {
        assert_eq!(
            super::channel_set_install_action(Some("use package manager")),
            super::ChannelSetInstallAction::PrintGuidance("use package manager")
        );
        assert_eq!(
            super::channel_set_install_action(None),
            super::ChannelSetInstallAction::RunSelfUpdate
        );
    }

    #[test]
    fn session_name_parser_accepts_option_terminator() {
        for name in ["-h", "--json"] {
            assert_eq!(
                super::parse_session_name_and_json(&["--".to_string(), name.to_string()], "usage",),
                Ok((name.to_string(), false))
            );
        }
    }

    #[test]
    fn parse_env_assignment_accepts_empty_values() {
        assert_eq!(
            super::parse_env_assignment("HERDR_ROLE=").unwrap(),
            ("HERDR_ROLE".to_string(), String::new())
        );
    }

    #[test]
    fn parse_env_assignment_requires_key_value_separator() {
        assert_eq!(
            super::parse_env_assignment("HERDR_ROLE").unwrap_err(),
            "env must use KEY=VALUE"
        );
    }

    #[test]
    fn maps_dead_server_connect_failure_to_friendly_error() {
        use crate::api::client::{ApiClient, ApiClientError};

        let client = ApiClient::local();
        let socket = client.socket_path().display().to_string();

        // The helper does NOT print; it returns a recognizable marker carrying
        // the ErrorResponse so the surfacing edge can print it exactly once.
        let mapped = super::map_server_not_running_or_io(
            ApiClientError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
            "cli:workspace:create",
            &client,
        );

        let response = super::server_not_running::reported_response(&mapped)
            .expect("dead-server connect failure should carry a server_not_running response");
        assert_eq!(response.id, "cli:workspace:create");
        assert_eq!(response.error.code, "server_not_running");
        assert!(response.error.message.contains(&socket));

        // The mapping is recognizable without string matching.
        assert!(super::server_not_running::was_reported(&mapped));
    }

    #[test]
    fn classifier_ignores_unrelated_io_kinds() {
        use crate::api::client::{ApiClient, ApiClientError};

        let client = ApiClient::local();
        let mapped = super::map_server_not_running_or_io(
            ApiClientError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            "cli:workspace:create",
            &client,
        );
        assert!(!super::server_not_running::was_reported(&mapped));
    }

    #[test]
    fn expand_equals_args_splits_value_options_only() {
        // Known value options split; values may contain `=`. Boolean and
        // unknown options keep the attached form so parsers still reject them.
        let args = vec![
            "--match=a=b".to_string(),
            "name=value".to_string(),
            "--raw=value".to_string(),
            "--bogus=value".to_string(),
            "--timeout=5000".to_string(),
        ];
        assert_eq!(
            super::expand_equals_args(&args, &["--match", "--timeout"]),
            vec![
                "--match",
                "a=b",
                "name=value",
                "--raw=value",
                "--bogus=value",
                "--timeout",
                "5000",
            ]
        );
    }
}
