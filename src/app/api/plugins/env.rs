use crate::api::schema::InstalledPluginInfo;

pub(super) fn plugin_config_dir(plugin_id: &str) -> std::path::PathBuf {
    crate::plugin_paths::plugin_config_dir(plugin_id)
}

pub(super) fn plugin_state_dir(plugin_id: &str) -> std::path::PathBuf {
    crate::plugin_paths::plugin_state_dir(plugin_id)
}

pub(super) fn ensure_plugin_user_dirs(plugin: &InstalledPluginInfo) -> std::io::Result<()> {
    crate::plugin_paths::ensure_plugin_user_dirs(&plugin.plugin_id)
}

/// Plugins written for upstream read `HERDR_*`; plugins written for this fork
/// read `TWODR_*`. Every `HERDR_` entry is published under both names so either
/// kind keeps working. Only this fork's own entries are twinned; a user's
/// launch env is left exactly as given.
pub(super) fn with_twodr_names(env: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(env.len() * 2);
    for (key, value) in env {
        if let Some(rest) = key.strip_prefix("HERDR_") {
            out.push((format!("TWODR_{rest}"), value.clone()));
        }
        out.push((key, value));
    }
    out
}

pub(super) fn plugin_path_env(plugin: &InstalledPluginInfo) -> Vec<(String, String)> {
    let config_dir = plugin_config_dir(&plugin.plugin_id);
    let state_dir = plugin_state_dir(&plugin.plugin_id);

    vec![
        ("HERDR_PLUGIN_ROOT".to_string(), plugin.plugin_root.clone()),
        (
            "HERDR_PLUGIN_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        ),
        (
            "HERDR_PLUGIN_STATE_DIR".to_string(),
            state_dir.display().to_string(),
        ),
    ]
}
