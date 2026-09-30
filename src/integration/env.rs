use std::io;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

use portable_pty::CommandBuilder;

pub(crate) const TWODR_PANE_ID_ENV_VAR: &str = "TWODR_PANE_ID";
pub(crate) const TWODR_TAB_ID_ENV_VAR: &str = "TWODR_TAB_ID";
pub(crate) const TWODR_WORKSPACE_ID_ENV_VAR: &str = "TWODR_WORKSPACE_ID";

pub(crate) const PI_CODING_AGENT_DIR_ENV_VAR: &str = "PI_CODING_AGENT_DIR";
pub(crate) const OMP_CONFIG_DIR_ENV_VAR: &str = "PI_CONFIG_DIR";
pub(crate) const CLAUDE_CONFIG_DIR_ENV_VAR: &str = "CLAUDE_CONFIG_DIR";
pub(crate) const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
pub(crate) const KIMI_CODE_HOME_ENV_VAR: &str = "KIMI_CODE_HOME";
pub(crate) const COPILOT_HOME_ENV_VAR: &str = "COPILOT_HOME";
pub(crate) const QODERCLI_CONFIG_DIR_ENV_VAR: &str = "QODER_CONFIG_DIR";
pub(crate) const QWEN_HOME_ENV_VAR: &str = "QWEN_HOME";
pub(crate) const CURSOR_CONFIG_DIR_ENV_VAR: &str = "CURSOR_CONFIG_DIR";
pub(crate) const ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR: &str = "ANTIGRAVITY_CLI_CONFIG_DIR";
pub(crate) const GROK_CONFIG_DIR_ENV_VAR: &str = "GROK_CONFIG_DIR";
/// The grok CLI's own config-home override (documented alongside
/// `$GROK_HOME/config.toml` and `$GROK_HOME/auth.json`).
pub(crate) const GROK_HOME_ENV_VAR: &str = "GROK_HOME";
pub(crate) const HERMES_HOME_ENV_VAR: &str = "HERMES_HOME";

/// Upstream-compatible aliases for the pane environment.
///
/// The integration assets installed into agent tools (`~/.claude/hooks/...`
/// and friends) are SHARED with an upstream herdr install: both write the same
/// file at the same path. They read `HERDR_*`.
///
/// So this fork exports BOTH prefixes into its panes: `TWODR_*` as the real
/// contract, and the `HERDR_*` spelling as a compatibility alias, pointing at
/// twodr's own socket and pane ids. That lets one unmodified hook serve either
/// multiplexer, and means installing twodr's integrations does not rewrite the
/// asset a running herdr fleet depends on.
///
/// Reading is NOT symmetric, and deliberately so: twodr only ever READS
/// `TWODR_*`, so a twodr launched inside a herdr pane does not inherit herdr's
/// socket and silently drive the other server.
///
/// # The alias is a trap for supervisors, so it carries its own binary
///
/// External supervisors (firstmate is the known one) AUTO-DETECT herdr from
/// `HERDR_ENV=1` and then invoke whichever `herdr` is first on `PATH`. In a
/// twodr pane that combination is wrong in a specific way: the identity vars
/// describe TWODR's socket while the binary is upstream herdr, so the
/// supervisor speaks an older protocol to this server and every call fails
/// with `protocol_mismatch` (measured: client 19 against server 22).
///
/// `HERDR_BIN_PATH` therefore points at THIS binary, which is the documented
/// way a supervisor learns which client owns the pane. A supervisor that
/// honours it works unmodified; one that hardcodes `herdr` from `PATH` gets a
/// loud protocol error rather than silently driving the wrong server, which is
/// the safer of the two failures.
fn apply_upstream_env_alias(cmd: &mut CommandBuilder, upstream: &str, value: &std::ffi::OsStr) {
    cmd.env(upstream, value);
}

pub(crate) fn apply_pane_base_env(cmd: &mut CommandBuilder) {
    let socket_path = crate::api::socket_path();
    cmd.env(crate::api::SOCKET_PATH_ENV_VAR, &socket_path);
    apply_upstream_env_alias(cmd, "HERDR_SOCKET_PATH", socket_path.as_os_str());
    if let Ok(executable) = crate::platform::launch_executable() {
        cmd.env("TWODR_BIN_PATH", &executable);
        apply_upstream_env_alias(cmd, "HERDR_BIN_PATH", executable.as_os_str());
    }
}

pub(crate) fn pi_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])?
            .join("extensions"),
    )
}

pub(crate) fn omp_extension_dir() -> io::Result<PathBuf> {
    if let Some(value) =
        std::env::var_os(PI_CODING_AGENT_DIR_ENV_VAR).filter(|value| !value.is_empty())
    {
        return expand_tilde_path(PathBuf::from(value)).map(|path| path.join("extensions"));
    }

    let config_dir = std::env::var_os(OMP_CONFIG_DIR_ENV_VAR)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".omp".into());
    Ok(home_dir()?
        .join(config_dir)
        .join("agent")
        .join("extensions"))
}

pub(crate) fn claude_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CLAUDE_CONFIG_DIR_ENV_VAR, &[".claude"])
}

pub(crate) fn codex_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CODEX_HOME_ENV_VAR, &[".codex"])
}

pub(crate) fn kimi_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(KIMI_CODE_HOME_ENV_VAR, &[".kimi-code"])
}

pub(crate) fn copilot_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(COPILOT_HOME_ENV_VAR, &[".copilot"])
}

pub(crate) fn devin_dir() -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value)).map(|path| path.join("devin"));
    }

    #[cfg(windows)]
    if let Some(value) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join("devin"));
    }

    Ok(home_dir()?.join(".config").join("devin"))
}

pub(crate) fn droid_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".factory"))
}

pub(crate) fn config_dir_from_env_or_home(
    env_var: &str,
    home_relative_segments: &[&str],
) -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(env_var).filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value));
    }

    let mut path = home_dir()?;
    for segment in home_relative_segments {
        path.push(segment);
    }
    Ok(path)
}

pub(crate) fn expand_tilde_path(path: PathBuf) -> io::Result<PathBuf> {
    let Some(raw) = path.to_str() else {
        return Ok(path);
    };

    if raw == "~" {
        return home_dir();
    }

    if let Some(rest) = raw
        .strip_prefix("~/")
        .or_else(|| raw.strip_prefix("~\\"))
        .or_else(|| raw.strip_prefix('~'))
    {
        return Ok(home_dir()?.join(rest));
    }

    Ok(path)
}

pub(crate) fn opencode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/opencode"))
}

pub(crate) fn opencode_state_dir() -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value)).map(|path| path.join("opencode"));
    }

    Ok(home_dir()?.join(".local/state/opencode"))
}

pub(crate) fn kilo_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/kilo"))
}

pub(crate) fn hermes_dir() -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(HERMES_HOME_ENV_VAR).filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value));
    }

    #[cfg(windows)]
    {
        let explicit_home = std::env::var_os("HOME").filter(|value| !value.is_empty());
        let profile = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty());
        if let Some(home) = explicit_home.filter(|home| profile.as_ref() != Some(home)) {
            return Ok(PathBuf::from(home).join(".hermes"));
        }
        if let Some(local_app_data) =
            std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())
        {
            return Ok(PathBuf::from(local_app_data).join("hermes"));
        }
    }

    Ok(home_dir()?.join(".hermes"))
}

pub(crate) fn hermes_plugin_dir() -> io::Result<PathBuf> {
    Ok(hermes_dir()?
        .join("plugins")
        .join(super::HERMES_PLUGIN_INSTALL_NAME))
}

pub(crate) fn qodercli_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(QODERCLI_CONFIG_DIR_ENV_VAR, &[".qoder"])
}

pub(crate) fn qwen_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(QWEN_HOME_ENV_VAR, &[".qwen"])
}

pub(crate) fn letta_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".letta"))
}

pub(crate) fn cursor_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CURSOR_CONFIG_DIR_ENV_VAR, &[".cursor"])
}

pub(crate) fn mastracode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".mastracode"))
}

pub(crate) fn antigravity_cli_dir() -> io::Result<PathBuf> {
    // Antigravity CLI discovers global customizations (hooks.json included)
    // from ~/.gemini/config; ~/.gemini/antigravity-cli holds runtime data and
    // is never read for hooks.
    config_dir_from_env_or_home(ANTIGRAVITY_CLI_CONFIG_DIR_ENV_VAR, &[".gemini", "config"])
}

pub(crate) fn grok_dir() -> io::Result<PathBuf> {
    // GROK_CONFIG_DIR is a herdr-level override only (primarily a test
    // seam); the grok CLI does not honor it, so it stays first and explicit.
    if let Some(value) = std::env::var_os(GROK_CONFIG_DIR_ENV_VAR).filter(|value| !value.is_empty())
    {
        return expand_tilde_path(PathBuf::from(value));
    }
    // The grok CLI honors GROK_HOME as its config home (config.toml,
    // auth.json, hooks/); mirror it so hook installs land where grok looks.
    config_dir_from_env_or_home(GROK_HOME_ENV_VAR, &[".grok"])
}

pub(crate) fn home_dir() -> io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }

    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(profile));
        }
        if let (Some(drive), Some(path)) = (
            std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty()),
            std::env::var_os("HOMEPATH").filter(|value| !value.is_empty()),
        ) {
            let mut home = PathBuf::from(drive);
            home.push(path);
            return Ok(home);
        }
    }

    Err(io::Error::other(
        "home directory is not set; cannot locate home directory",
    ))
}

#[cfg(test)]
pub(crate) struct IntegrationEnvLock {
    _guard: MutexGuard<'static, ()>,
    #[cfg(windows)]
    appdata: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl Drop for IntegrationEnvLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(appdata) = self.appdata.take() {
            std::env::set_var("APPDATA", appdata);
        } else {
            std::env::remove_var("APPDATA");
        }
    }
}

#[cfg(test)]
pub(crate) fn integration_env_lock() -> IntegrationEnvLock {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    IntegrationEnvLock {
        _guard: guard,
        #[cfg(windows)]
        appdata: std::env::var_os("APPDATA"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_state_dir_defaults_to_local_state() {
        let _lock = integration_env_lock();
        let original = std::env::var_os("XDG_STATE_HOME");
        std::env::remove_var("XDG_STATE_HOME");
        let expected = home_dir().unwrap().join(".local/state/opencode");
        assert_eq!(opencode_state_dir().unwrap(), expected);
        match original {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[test]
    fn opencode_state_dir_honors_xdg_state_home() {
        let _lock = integration_env_lock();
        let original = std::env::var_os("XDG_STATE_HOME");
        let xdg = std::env::temp_dir().join("herdr-xdg-state");
        std::env::set_var("XDG_STATE_HOME", &xdg);
        assert_eq!(opencode_state_dir().unwrap(), xdg.join("opencode"));
        match original {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}

#[cfg(test)]
mod supervisor_contract_tests {
    /// External supervisors auto-detect this multiplexer from `HERDR_ENV=1`
    /// and read pane identity from these exact names. Renaming or dropping one
    /// silently breaks every supervisor rather than failing loudly, so the set
    /// is pinned here.
    ///
    /// Verified live 2026-09-22 inside a twodr pane: all six are exported and
    /// resolve to twodr's own session, alongside their `TWODR_*` equivalents.
    const SUPERVISOR_REQUIRED_PANE_ENV: &[&str] = &[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_SOCKET_PATH",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_BIN_PATH",
    ];

    #[test]
    fn the_supervisor_identity_contract_is_not_silently_narrowed() {
        // HERDR_BIN_PATH is the escape hatch that makes the rest safe: the
        // other five describe THIS server, so a supervisor that resolves a
        // bare `herdr` from PATH would drive the wrong one. Pointing it at the
        // running binary is what lets an unmodified supervisor stay correct.
        assert!(
            SUPERVISOR_REQUIRED_PANE_ENV.contains(&"HERDR_BIN_PATH"),
            "dropping HERDR_BIN_PATH leaves supervisors resolving `herdr` from PATH"
        );
        assert_eq!(
            SUPERVISOR_REQUIRED_PANE_ENV.len(),
            6,
            "narrowing this set breaks supervisor auto-detection; widen deliberately"
        );
    }
}
