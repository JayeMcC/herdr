//! Where a stack lives, and whether it can be reached right now.
//!
//! An agent assigned to a stack should run WHERE THAT STACK RUNS: inside the
//! stack's devcontainer, on the host that hosts it. Two reasons, one of which
//! is a correctness argument rather than a resource one:
//!
//! * Memory. A Claude session costs ~450-640 MB of footprint regardless of
//!   age, paid once per agent. Moving stack agents off the local machine moves
//!   that cost to the machine that already owns the work.
//! * Correctness. An agent on the wrong machine cannot see its own stack. A
//!   container lookup for a remote stack run locally returns "No such
//!   container", which reads exactly like a missing stack and is not — it is
//!   the wrong host. An agent colocated with its stack cannot make that
//!   mistake, because there is no other host for it to be on.
//!
//! Nothing here hardcodes a host table. The fleet registry is the only source
//! of stack placement, read through `bin/fleet --json`.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long to wait for a reachability probe.
///
/// Deliberately generous. A 10s deadline produced FALSE negatives against a
/// healthy host while the machine was thrashing, and a pane that keys on a
/// short timeout reports healthy remote agents as dead.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// One stack's placement, as the fleet registry reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackPlacement {
    /// Display name — what humans and `decisions/HUB.md` call it (`w1-s9`).
    /// Route on this and on the host, NEVER on a bare vessel id: two stacks
    /// called `s4` on different hosts is the ambiguity that makes an agent
    /// exec into the wrong container.
    pub name: String,
    /// `None` means the stack is local. That is the discriminator, not a
    /// separate boolean.
    pub ssh_host: Option<String>,
    /// The container name the REGISTRY believes in. Frequently wrong for
    /// renamed vessels, so it is only a hint — see
    /// [`StackPlacement::resolve_container`].
    pub registry_container: Option<String>,
    /// The stack's web port. This is the reliable container discriminator,
    /// because it is what the container actually publishes.
    pub web_port: Option<u16>,
}

/// Whether a host answered, as a union rather than a struct with optional
/// fields.
///
/// There is deliberately NO shape in which absent data can be mistaken for
/// good data. An intermittently-available host is NORMAL, not a fault, so a
/// pane on one must render as offline-but-expected, never as healthy and never
/// as a dead-looking-but-fine lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reachability {
    /// The host answered.
    Reachable,
    /// The host did not answer, and that is expected for this host.
    Offline { reason: String },
    /// The host did not answer, and it was supposed to be there.
    Unreachable { reason: String },
    /// We could not even ask. NOT the same as "not reachable": an unknown
    /// state must never be collapsed into either a healthy or a failed one.
    Unknown { reason: String },
}

impl Reachability {
    /// Only a host that actually answered may be spawned into.
    pub fn is_usable(&self) -> bool {
        matches!(self, Reachability::Reachable)
    }

    pub fn describe(&self) -> String {
        match self {
            Reachability::Reachable => "reachable".to_string(),
            Reachability::Offline { reason } => {
                format!("OFFLINE (expected for this host): {reason}")
            }
            Reachability::Unreachable { reason } => format!("UNREACHABLE: {reason}"),
            Reachability::Unknown { reason } => format!("UNKNOWN: {reason}"),
        }
    }
}

/// Why a stack could not be placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// The registry has no such stack. Distinct from "the host is down": a
    /// stack that does not exist and a stack whose machine is asleep are
    /// different facts and must not share an error.
    NotInRegistry { name: String, known: Vec<String> },
    /// The registry itself could not be read, so stack existence is unknown.
    RegistryUnavailable { reason: String },
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementError::NotInRegistry { name, known } => write!(
                f,
                "no stack named {name:?} in the fleet registry (known: {})",
                known.join(", ")
            ),
            PlacementError::RegistryUnavailable { reason } => write!(
                f,
                "cannot read the fleet registry, so stack placement is unknown: {reason}. \
                 This is not the same as the stack being absent"
            ),
        }
    }
}

impl StackPlacement {
    pub fn is_local(&self) -> bool {
        self.ssh_host.is_none()
    }

    /// The container this stack actually runs in, resolved on the host.
    ///
    /// The registry's container name is a HINT, not an answer. Vessels are
    /// renamed for humans (`s4` became `w1-s8`) while their on-host container
    /// keeps the original vessel id, so the registry can name a container that
    /// does not exist. Measured 2026-09-22: the registry reported
    /// `forwood-one_w1-s9_devcontainer-dev-1`, and the real container was
    /// `forwood-one_s5_devcontainer-dev-1`.
    ///
    /// The published web port IS reliable, because it is what the container
    /// publishes, so resolution goes by port and only falls back to the name.
    pub fn resolve_container(&self, runner: &dyn HostRunner) -> io::Result<String> {
        if let Some(port) = self.web_port {
            let script = format!(
                "docker ps --format '{{{{.Names}}}}|{{{{.Ports}}}}' | grep -E ':{port}->' | cut -d'|' -f1 | head -1"
            );
            if let Ok(found) = runner.run(self, &script) {
                let found = found.trim();
                if !found.is_empty() {
                    return Ok(found.to_string());
                }
            }
        }
        self.registry_container.clone().ok_or_else(|| {
            io::Error::other(format!(
                "could not resolve a container for stack {}: no container publishes its web port \
                 and the registry named none",
                self.name
            ))
        })
    }
}

/// Runs a shell snippet on the machine that hosts a stack.
///
/// Abstracted so placement logic is testable without a network, and so the
/// Windows/WSL path (where the host shell is PowerShell and the Linux shell is
/// reached through `wsl`) is one implementation rather than branches smeared
/// through the caller.
pub trait HostRunner {
    fn run(&self, placement: &StackPlacement, script: &str) -> io::Result<String>;
}

/// Reads stack placement from the fleet registry.
///
/// `bin/fleet --json` is the canonical fleet-wide enumerator. The hub's own
/// `/api/stacks` returns LOCAL stacks only (measured: 1 row against fleet's 8),
/// so it is not a substitute.
pub fn load_placements(fleet_bin: &PathBuf) -> Result<Vec<StackPlacement>, PlacementError> {
    let output = Command::new(fleet_bin)
        .arg("--json")
        .output()
        .map_err(|err| PlacementError::RegistryUnavailable {
            reason: format!("could not run {}: {err}", fleet_bin.display()),
        })?;
    if !output.status.success() {
        return Err(PlacementError::RegistryUnavailable {
            reason: format!(
                "{} exited {}: {}",
                fleet_bin.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        PlacementError::RegistryUnavailable {
            reason: format!("fleet did not return JSON: {err}"),
        }
    })?;
    let rows = rows
        .as_array()
        .ok_or_else(|| PlacementError::RegistryUnavailable {
            reason: "fleet JSON was not an array".to_string(),
        })?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(StackPlacement {
                name: row.get("name")?.as_str()?.to_string(),
                ssh_host: row
                    .get("sshHost")
                    .and_then(|host| host.as_str())
                    .map(str::to_string),
                registry_container: row
                    .get("container")
                    .and_then(|container| container.as_str())
                    .map(str::to_string),
                web_port: row
                    .get("webPort")
                    .and_then(|port| port.as_u64())
                    .and_then(|port| u16::try_from(port).ok()),
            })
        })
        .collect())
}

/// Find one stack by display name.
pub fn find_placement(fleet_bin: &PathBuf, name: &str) -> Result<StackPlacement, PlacementError> {
    let placements = load_placements(fleet_bin)?;
    placements
        .iter()
        .find(|placement| placement.name == name)
        .cloned()
        .ok_or_else(|| PlacementError::NotInRegistry {
            name: name.to_string(),
            known: placements.into_iter().map(|p| p.name).collect(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeHost {
        output: String,
        seen: RefCell<Vec<String>>,
    }

    impl HostRunner for FakeHost {
        fn run(&self, _placement: &StackPlacement, script: &str) -> io::Result<String> {
            self.seen.borrow_mut().push(script.to_string());
            Ok(self.output.clone())
        }
    }

    fn renamed_vessel() -> StackPlacement {
        // Exactly the live shape measured on 2026-09-22: the registry names a
        // container that does not exist on the host.
        StackPlacement {
            name: "w1-s9".into(),
            ssh_host: Some("desktop-cr8et6u.tail81a3b9.ts.net".into()),
            registry_container: Some("forwood-one_w1-s9_devcontainer-dev-1".into()),
            web_port: Some(5182),
        }
    }

    #[test]
    fn container_resolves_by_port_not_by_the_registrys_name() {
        let host = FakeHost {
            output: "forwood-one_s5_devcontainer-dev-1\n".into(),
            seen: RefCell::new(Vec::new()),
        };
        let resolved = renamed_vessel().resolve_container(&host).unwrap();
        assert_eq!(resolved, "forwood-one_s5_devcontainer-dev-1");
        assert!(
            host.seen.borrow()[0].contains(":5182->"),
            "resolution must key on the published port"
        );
    }

    #[test]
    fn container_falls_back_to_the_registry_name_when_no_port_matches() {
        let host = FakeHost {
            output: "\n".into(),
            seen: RefCell::new(Vec::new()),
        };
        assert_eq!(
            renamed_vessel().resolve_container(&host).unwrap(),
            "forwood-one_w1-s9_devcontainer-dev-1"
        );
    }

    #[test]
    fn a_missing_stack_and_an_unreadable_registry_are_different_errors() {
        // Collapsing these is the "No such container" trap: a stack that does
        // not exist and a registry we could not read are different facts.
        let missing = PlacementError::NotInRegistry {
            name: "s99".into(),
            known: vec!["s1".into()],
        };
        let unreadable = PlacementError::RegistryUnavailable {
            reason: "hub unreachable".into(),
        };
        assert_ne!(missing, unreadable);
        assert!(missing.to_string().contains("no stack named"));
        assert!(unreadable
            .to_string()
            .contains("not the same as the stack being absent"));
    }

    #[test]
    fn only_a_reachable_host_is_usable_and_unknown_is_not_healthy() {
        assert!(Reachability::Reachable.is_usable());
        // An intermittent host is normal, but it is still not spawnable.
        assert!(!Reachability::Offline {
            reason: "asleep".into()
        }
        .is_usable());
        assert!(!Reachability::Unreachable {
            reason: "timeout".into()
        }
        .is_usable());
        // The critical one: unknown must never read as healthy.
        assert!(!Reachability::Unknown {
            reason: "no probe".into()
        }
        .is_usable());
    }

    #[test]
    fn a_local_stack_is_identified_by_absent_ssh_host() {
        let local = StackPlacement {
            name: "dedicated_base_stack".into(),
            ssh_host: None,
            registry_container: None,
            web_port: None,
        };
        assert!(local.is_local());
        assert!(!renamed_vessel().is_local());
    }
}
