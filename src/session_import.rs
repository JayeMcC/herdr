//! Import existing upstream `herdr` sessions into this fork's own state.
//!
//! # Why this exists
//!
//! twodr keeps its own config dir, state dir and sockets (see
//! [`crate::config::config_dir`]) so it can run alongside herdr without
//! two servers writing one `session.json`. That independence means a fresh
//! twodr starts empty, so this module brings existing lanes across.
//!
//! # What is actually being moved
//!
//! A Claude Code session is NOT owned by herdr. It is its own process with its
//! own transcript at `~/.claude/projects/<slug>/<session-uuid>.jsonl`. herdr
//! owns the PANE, not the conversation. So an import does not transfer
//! ownership of a conversation; it copies the pane RECORD, including the agent
//! session uuid, which is the handle needed to re-open that same conversation.
//!
//! # Tier boundary (deliberate, investigated, not an oversight)
//!
//! * TIER 1 — RESUME (what this does): twodr records the uuid and can later
//!   open a pane running `claude --resume <uuid>`. Same conversation, full
//!   history, but a NEW process.
//! * TIER 2 — ADOPT THE LIVE PROCESS (not available): herdr does pass live PTY
//!   fds over `SCM_RIGHTS`, but only as a whole-server replacement that the
//!   OLD server initiates and which is gated on an exact version/protocol
//!   match. It cannot be driven per-lane by a peer binary. See
//!   `docs/next/website/src/content/docs/twodr-import.mdx`.
//!
//! # The hazard this module is designed around
//!
//! Two multiplexers both believing they own one session means two processes
//! appending to one transcript, which corrupts it unrecoverably. The transcript
//! IS the conversation. So an imported lane is recorded as
//! [`ImportedLaneState::Parked`]: a record that is explicitly NOT running, and
//! which twodr will not auto-resume. Ownership transfers only when a human
//! closes the herdr pane and then resumes in twodr — one owner at a time,
//! enforced by the imported lane having no process to begin with.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where an imported lane's conversation currently lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedLaneState {
    /// Imported as a record. No process. Safe to resume once the herdr pane
    /// hosting this uuid is closed.
    Parked,
}

/// One imported lane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedLane {
    /// The agent session uuid. This is the dedupe key and the resume handle.
    pub session_value: String,
    /// Persisted source tag, e.g. `herdr:claude`. This is a data value in the
    /// upstream format, NOT this binary's name; it must not be rewritten to
    /// `twodr:claude` or resume planning stops recognising it.
    pub source: String,
    pub agent: String,
    pub agent_name: Option<String>,
    pub cwd: PathBuf,
    pub workspace_label: Option<String>,
    pub state: ImportedLaneState,
}

impl ImportedLane {
    /// The command that re-opens this exact conversation.
    pub fn resume_argv(&self) -> Option<Vec<String>> {
        let session_ref = crate::agent_resume::AgentSessionRef::id(self.session_value.clone())?;
        crate::agent_resume::plan(&self.source, &self.agent, &session_ref).map(|plan| plan.argv)
    }
}

/// A pane present in the upstream snapshot that was NOT imported, and why.
/// Every skip is reported; a partial import must never look like a clean one.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedLane {
    pub reason: String,
    pub agent_name: Option<String>,
    pub cwd: Option<String>,
    /// Whether this skip means a CONVERSATION was left behind.
    ///
    /// A pane with no agent session is a plain shell: there is nothing to
    /// resume, so noting it is informational. An agent lane that could not be
    /// imported is a real loss and must fail the run. Collapsing the two would
    /// let a genuine failure exit 0 on a fleet that happens to contain shells.
    pub lost_conversation: bool,
}

/// The importer's durable store. Separate from `session.json` so an import can
/// never corrupt the live session state of either binary.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImportedLanes {
    #[serde(default)]
    pub version: u32,
    /// Keyed by session uuid — stable across renames. Display names are NOT
    /// unique in practice (two lanes claimed the same name on 2026-09-21), so
    /// keying on a name would silently merge distinct conversations.
    #[serde(default)]
    pub lanes: BTreeMap<String, ImportedLane>,
}

const IMPORT_STORE_VERSION: u32 = 1;

pub fn import_store_path() -> PathBuf {
    crate::config::config_dir().join("imported-lanes.json")
}

pub fn load_imported() -> io::Result<ImportedLanes> {
    let path = import_store_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            io::Error::other(format!(
                "{} is not valid import state: {err}",
                path.display()
            ))
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(ImportedLanes {
            version: IMPORT_STORE_VERSION,
            lanes: BTreeMap::new(),
        }),
        Err(err) => Err(err),
    }
}

fn save_imported(state: &ImportedLanes) -> io::Result<()> {
    let path = import_store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    // Write-then-rename so an interrupted import cannot truncate existing
    // import state.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Outcome of one import run.
#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub source: String,
    /// Agent lanes found in the upstream snapshot.
    pub found: usize,
    /// Newly recorded this run.
    pub imported: usize,
    /// Already present (same uuid) — re-running is a no-op for these.
    pub already_present: usize,
    /// Present but with changed metadata, refreshed in place.
    pub updated: usize,
    pub skipped: Vec<SkippedLane>,
}

impl ImportReport {
    /// True when no CONVERSATION was left behind.
    ///
    /// Plain shell panes are reported but do not make a run incomplete; there
    /// is no conversation in them to lose.
    pub fn is_complete(&self) -> bool {
        !self.skipped.iter().any(|skip| skip.lost_conversation)
    }

    /// Agent lanes that could not be imported. Non-zero means data was lost.
    pub fn lost(&self) -> usize {
        self.skipped
            .iter()
            .filter(|skip| skip.lost_conversation)
            .count()
    }
}

/// The upstream snapshot path this importer reads. Read-only.
pub fn upstream_snapshot_path() -> PathBuf {
    crate::config::upstream_config_dir().join("session.json")
}

/// Read upstream herdr state and record its agent lanes.
///
/// NON-DESTRUCTIVE: opens the upstream snapshot read-only and never writes,
/// moves or mutates anything under the upstream directory. If this import is
/// wrong, the operator still has a working herdr.
///
/// RE-RUNNABLE: keyed on session uuid, so importing twice records each
/// conversation once.
pub fn import_from_upstream(dry_run: bool) -> io::Result<ImportReport> {
    let path = upstream_snapshot_path();
    let text = std::fs::read_to_string(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "cannot read upstream session state at {}: {err}",
                path.display()
            ),
        )
    })?;
    let snapshot: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| io::Error::other(format!("{} is not valid JSON: {err}", path.display())))?;

    let mut report = ImportReport {
        source: path.display().to_string(),
        ..Default::default()
    };
    let mut state = load_imported()?;
    state.version = IMPORT_STORE_VERSION;

    let workspaces = snapshot
        .get("workspaces")
        .and_then(|value| value.as_array())
        .ok_or_else(|| io::Error::other(format!("{} has no workspaces array", path.display())))?;

    for workspace in workspaces {
        let workspace_label = workspace
            .get("custom_name")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let tabs = workspace.get("tabs").and_then(|value| value.as_array());
        for tab in tabs.into_iter().flatten() {
            let panes = tab.get("panes").and_then(|value| value.as_object());
            for pane in panes.into_iter().flatten().map(|(_, pane)| pane) {
                let cwd = pane.get("cwd").and_then(|value| value.as_str());
                let agent_name = pane
                    .get("agent_name")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let Some(session) = pane.get("agent_session") else {
                    // A pane with no agent session is a plain shell, not a
                    // conversation. Not a failure, but still reported so the
                    // found/imported arithmetic is checkable.
                    report.skipped.push(SkippedLane {
                        reason: "pane has no agent session to resume (plain shell pane)".into(),
                        agent_name,
                        cwd: cwd.map(str::to_string),
                        lost_conversation: false,
                    });
                    continue;
                };
                report.found += 1;

                let value = session.get("value").and_then(|value| value.as_str());
                let source = session.get("source").and_then(|value| value.as_str());
                let agent = session.get("agent").and_then(|value| value.as_str());
                let kind = session.get("kind").and_then(|value| value.as_str());

                let (Some(value), Some(source), Some(agent)) = (value, source, agent) else {
                    report.skipped.push(SkippedLane {
                        reason: "agent session record is missing value/source/agent".into(),
                        agent_name,
                        cwd: cwd.map(str::to_string),
                        lost_conversation: true,
                    });
                    continue;
                };
                if kind != Some("id") {
                    report.skipped.push(SkippedLane {
                        reason: format!(
                            "agent session is kind {:?}, only id-resumable sessions are imported",
                            kind.unwrap_or("missing")
                        ),
                        agent_name,
                        cwd: cwd.map(str::to_string),
                        lost_conversation: true,
                    });
                    continue;
                }
                let Some(cwd) = cwd else {
                    report.skipped.push(SkippedLane {
                        reason: "pane record has no cwd to resume in".into(),
                        agent_name,
                        cwd: None,
                        lost_conversation: true,
                    });
                    continue;
                };

                let lane = ImportedLane {
                    session_value: value.to_string(),
                    source: source.to_string(),
                    agent: agent.to_string(),
                    agent_name,
                    cwd: PathBuf::from(cwd),
                    workspace_label: workspace_label.clone(),
                    state: ImportedLaneState::Parked,
                };

                // Reject anything this build could not actually resume, rather
                // than recording a lane that fails only when the operator tries
                // to use it.
                if lane.resume_argv().is_none() {
                    report.skipped.push(SkippedLane {
                        reason: format!(
                            "no resume command is known for source {source} / agent {agent}"
                        ),
                        agent_name: lane.agent_name.clone(),
                        cwd: Some(cwd.to_string()),
                        lost_conversation: true,
                    });
                    continue;
                }

                match state.lanes.get(&lane.session_value) {
                    Some(existing)
                        if existing.cwd == lane.cwd
                            && existing.agent_name == lane.agent_name
                            && existing.source == lane.source =>
                    {
                        report.already_present += 1;
                    }
                    Some(_) => {
                        report.updated += 1;
                        if !dry_run {
                            state.lanes.insert(lane.session_value.clone(), lane);
                        }
                    }
                    None => {
                        report.imported += 1;
                        if !dry_run {
                            state.lanes.insert(lane.session_value.clone(), lane);
                        }
                    }
                }
            }
        }
    }

    if !dry_run {
        save_imported(&state)?;
    }
    Ok(report)
}

/// Whether a transcript file exists for this uuid, so the report can say
/// plainly which imported lanes actually resolve to a real conversation.
pub fn transcript_exists(session_value: &str) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let projects = Path::new(&home).join(".claude").join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return false;
    };
    let needle = format!("{session_value}.jsonl");
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.path().join(&needle).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_key_is_the_uuid_not_the_display_name() {
        // Two lanes claimed the same display name on 2026-09-21. Keying on a
        // name would merge two distinct conversations into one record.
        let a = ImportedLane {
            session_value: "11111111-1111-1111-1111-111111111111".into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            agent_name: Some("assistant".into()),
            cwd: PathBuf::from("/tmp/a"),
            workspace_label: None,
            state: ImportedLaneState::Parked,
        };
        let mut b = a.clone();
        b.session_value = "22222222-2222-2222-2222-222222222222".into();
        b.cwd = PathBuf::from("/tmp/b");

        let mut lanes = BTreeMap::new();
        lanes.insert(a.session_value.clone(), a);
        lanes.insert(b.session_value.clone(), b);
        assert_eq!(lanes.len(), 2, "same name must not collapse two uuids");
    }

    #[test]
    fn imported_claude_lane_resumes_the_same_conversation() {
        let lane = ImportedLane {
            session_value: "abcdabcd-1234-1234-1234-abcdabcdabcd".into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            agent_name: Some("w-twodr".into()),
            cwd: PathBuf::from("/tmp"),
            workspace_label: None,
            state: ImportedLaneState::Parked,
        };
        assert_eq!(
            lane.resume_argv(),
            Some(vec![
                "claude".to_string(),
                "--resume".to_string(),
                "abcdabcd-1234-1234-1234-abcdabcdabcd".to_string(),
            ])
        );
    }

    #[test]
    fn upstream_source_tag_is_data_and_must_not_be_rebranded() {
        // Rewriting the persisted "herdr:claude" tag to "twodr:claude" would
        // make resume planning stop recognising the record.
        let lane = ImportedLane {
            session_value: "abcdabcd-1234-1234-1234-abcdabcdabcd".into(),
            source: "twodr:claude".into(),
            agent: "claude".into(),
            agent_name: None,
            cwd: PathBuf::from("/tmp"),
            workspace_label: None,
            state: ImportedLaneState::Parked,
        };
        assert_eq!(lane.resume_argv(), None);
    }

    #[test]
    fn an_import_with_skips_is_not_complete() {
        let mut report = ImportReport::default();
        assert!(report.is_complete());
        report.skipped.push(SkippedLane {
            reason: "plain shell pane".into(),
            agent_name: None,
            cwd: None,
            lost_conversation: false,
        });
        assert!(
            report.is_complete(),
            "a shell pane has no conversation to lose and must not fail the run"
        );
        report.skipped.push(SkippedLane {
            reason: "no cwd".into(),
            agent_name: None,
            cwd: None,
            lost_conversation: true,
        });
        assert!(
            !report.is_complete(),
            "a partial import must not read as clean"
        );
        assert_eq!(report.lost(), 1);
    }
}

// ---------------------------------------------------------------------------
// One owner at a time
// ---------------------------------------------------------------------------

/// Why a lane may not be materialised right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipBlock {
    /// Upstream herdr still records this session against one of its panes.
    /// Resuming it would put a second process on one transcript.
    HeldByUpstream { agent_name: Option<String> },
    /// This fork already materialised the lane and has not released it.
    AlreadyMaterialised { pane_id: String },
}

impl std::fmt::Display for OwnershipBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnershipBlock::HeldByUpstream { agent_name } => write!(
                f,
                "herdr still holds this session{}. Close that herdr pane first: \
                 two processes appending to one transcript corrupts the conversation",
                agent_name
                    .as_deref()
                    .map(|name| format!(" in its `{name}` pane"))
                    .unwrap_or_default()
            ),
            OwnershipBlock::AlreadyMaterialised { pane_id } => {
                write!(f, "already materialised in this session at pane {pane_id}")
            }
        }
    }
}

/// Session uuids upstream herdr currently records against a live pane.
///
/// Read from herdr's `session.json` rather than its socket on purpose. The
/// socket is version-gated: a newer client is refused by an older server with
/// `protocol_mismatch`, which would make the safety check fail exactly when the
/// two installs differ most. The state file has no such gate, and it is the
/// same record herdr itself restores from.
///
/// Returns `None` when upstream state cannot be read at all. That is NOT
/// "nothing is held" — it is "unknown", and callers must refuse rather than
/// assume the lane is free.
pub fn upstream_held_sessions() -> Option<BTreeMap<String, Option<String>>> {
    let text = std::fs::read_to_string(upstream_snapshot_path()).ok()?;
    let snapshot: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut held = BTreeMap::new();
    for workspace in snapshot.get("workspaces")?.as_array()? {
        let Some(tabs) = workspace.get("tabs").and_then(|value| value.as_array()) else {
            continue;
        };
        for tab in tabs {
            let Some(panes) = tab.get("panes").and_then(|value| value.as_object()) else {
                continue;
            };
            for pane in panes.values() {
                let Some(value) = pane
                    .get("agent_session")
                    .and_then(|session| session.get("value"))
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };
                let name = pane
                    .get("agent_name")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                held.insert(value.to_string(), name);
            }
        }
    }
    Some(held)
}

/// Agent names must match the server's `[a-z][a-z0-9_-]{0,31}` rule, so an
/// imported display name is sanitised rather than rejected.
pub fn sanitized_agent_name(lane: &ImportedLane) -> String {
    let raw = lane.agent_name.clone().unwrap_or_else(|| {
        format!(
            "lane-{}",
            &lane.session_value[..8.min(lane.session_value.len())]
        )
    });
    let mut name: String = raw
        .chars()
        .map(|ch| {
            let lower = ch.to_ascii_lowercase();
            if lower.is_ascii_lowercase() || lower.is_ascii_digit() || matches!(lower, '-' | '_') {
                lower
            } else {
                '-'
            }
        })
        .collect();
    while !name.is_empty() && !name.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        name.remove(0);
    }
    if name.is_empty() {
        name = format!(
            "lane-{}",
            &lane.session_value[..8.min(lane.session_value.len())]
        );
    }
    name.truncate(32);
    name
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    fn lane(name: Option<&str>) -> ImportedLane {
        ImportedLane {
            session_value: "dea56ee9-0146-407a-8fa2-e484575a3a38".into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            agent_name: name.map(str::to_string),
            cwd: PathBuf::from("/tmp"),
            workspace_label: None,
            state: ImportedLaneState::Parked,
        }
    }

    #[test]
    fn agent_names_are_coerced_to_the_servers_rule() {
        assert_eq!(sanitized_agent_name(&lane(Some("r-4086-s2"))), "r-4086-s2");
        // A leading digit is illegal for the server, so it is trimmed rather
        // than silently failing at start time.
        assert_eq!(sanitized_agent_name(&lane(Some("4086-epic"))), "epic");
        assert_eq!(
            sanitized_agent_name(&lane(Some("W-2erdr Fork"))),
            "w-2erdr-fork"
        );
        // No usable name at all still yields a legal, uuid-derived one.
        assert_eq!(sanitized_agent_name(&lane(None)), "lane-dea56ee9");
        assert_eq!(sanitized_agent_name(&lane(Some("---"))), "lane-dea56ee9");
    }

    #[test]
    fn a_held_lane_reports_who_holds_it() {
        let block = OwnershipBlock::HeldByUpstream {
            agent_name: Some("infra-fleet-health".into()),
        };
        let message = block.to_string();
        assert!(message.contains("infra-fleet-health"));
        assert!(message.contains("corrupts the conversation"));
    }
}

#[cfg(test)]
mod existing_store_tests {
    use super::*;

    /// A store that already holds rows, deserialized from the on-disk shape.
    ///
    /// Every other test in this file builds state from empty, which is exactly
    /// the shape that cannot catch a migration or compatibility fault: a
    /// schema change can leave all fresh-state tests green while every real
    /// store on disk fails to load. (Live cost 2026-09-22 on the hub: a
    /// `CREATE TABLE IF NOT EXISTS` no-opped on an existing table, the new
    /// index referenced a column that did not exist, and the service failed to
    /// boot while its suite stayed green because every test opened a fresh
    /// database.)
    fn store_with_existing_rows() -> &'static str {
        r#"{
          "version": 1,
          "lanes": {
            "dea56ee9-0146-407a-8fa2-e484575a3a38": {
              "session_value": "dea56ee9-0146-407a-8fa2-e484575a3a38",
              "source": "herdr:claude",
              "agent": "claude",
              "agent_name": "r-declined-pr-sweep",
              "cwd": "/Users/jayemccracken/proj/forwood-one-review",
              "workspace_label": null,
              "state": "parked"
            }
          }
        }"#
    }

    #[test]
    fn a_store_written_by_an_earlier_build_still_loads() {
        let parsed: ImportedLanes = serde_json::from_str(store_with_existing_rows())
            .expect("an existing on-disk store must still deserialize");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.lanes.len(), 1);
        let lane = parsed
            .lanes
            .get("dea56ee9-0146-407a-8fa2-e484575a3a38")
            .expect("existing row must survive a load");
        assert_eq!(lane.agent_name.as_deref(), Some("r-declined-pr-sweep"));
        assert_eq!(lane.state, ImportedLaneState::Parked);
        // The resume handle is the whole point of the record: if a schema
        // change breaks it, the conversation becomes unreachable.
        assert_eq!(
            lane.resume_argv(),
            Some(vec![
                "claude".to_string(),
                "--resume".to_string(),
                "dea56ee9-0146-407a-8fa2-e484575a3a38".to_string(),
            ])
        );
    }

    #[test]
    fn re_importing_over_existing_rows_updates_rather_than_duplicates() {
        let mut parsed: ImportedLanes =
            serde_json::from_str(store_with_existing_rows()).expect("existing store must load");
        let before = parsed.lanes.len();

        // Same uuid, changed metadata — the re-run case against a NON-empty
        // store, which is where a keying mistake shows up and an empty-store
        // test cannot.
        let mut moved = parsed
            .lanes
            .values()
            .next()
            .cloned()
            .expect("fixture has one lane");
        moved.cwd = PathBuf::from("/somewhere/else");
        parsed.lanes.insert(moved.session_value.clone(), moved);

        assert_eq!(parsed.lanes.len(), before, "same uuid must not add a row");
        assert_eq!(
            parsed.lanes.values().next().map(|lane| lane.cwd.clone()),
            Some(PathBuf::from("/somewhere/else")),
            "the existing row must be updated in place"
        );
    }
}
