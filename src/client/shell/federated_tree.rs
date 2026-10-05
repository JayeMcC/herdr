//! One tier tree across every machine.
//!
//! Operator ruling 2026-10-05, verbatim: "Yeah I guess it's good to know where
//! an agent is hosted. I just don't want it to break the tree sorting. It
//! shouldn't be the primary identifier".
//!
//! With more than one machine connected, the sidebar used to list each machine
//! first and that machine's spaces under it, so the tier tree (assistant,
//! infra, meta, work) was split once per machine. This module merges the
//! machines instead: spaces group by their tier name across machines, so a
//! `work/o-x` on the M4 sits under the same `work` heading as the Air's
//! `work/o-y`. The machine becomes an attribute of each row, never a group.
//!
//! Pure: it takes the cached snapshots and the collapse state and returns rows
//! in render order. Rendering and hit-testing live in `endpoint_sidebar`.

use std::collections::{HashMap, HashSet};

use super::*;

/// One row of the merged spaces list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FederatedRow {
    /// A space on one machine.
    Workspace {
        endpoint: usize,
        index: usize,
        indented: bool,
    },
    /// A tier heading shared by every machine whose spaces fall under it.
    Heading { key: String, label: String },
}

/// The tier group a space belongs to across machines: `work` for a space
/// labelled `work` or `work/<x>`, `None` for any other space.
fn tier_of(label: &str) -> Option<&str> {
    let parent = label.split_once('/').map_or(label, |(parent, _)| parent);
    (!parent.is_empty()).then_some(parent)
}

/// Collapse key of a merged tier group. The same `space:` namespace the
/// single-machine sidebar uses, so a group collapsed before a second machine
/// connected stays collapsed after.
pub(super) fn tier_key(tier: &str) -> String {
    format!("space:{tier}")
}

/// Merge every machine's spaces into one tier tree.
///
/// - A group forms under one heading for a tier name when that tier has a
///   `<tier>/<child>` space on any machine, or when two or more spaces (on any
///   machines) carry the bare tier name. That matches the single-machine
///   sidebar, which derives a `work` heading for a lone `work/o-a`. A space
///   labelled exactly the tier name is shown as a child, so two machines'
///   `work` spaces both stay reachable under the one heading.
/// - Any other space renders as a plain top-level row, as with one machine.
/// - Groups and lone spaces keep the order of their first appearance, scanning
///   machines in catalog order (Local first), so adding a machine never
///   reorders the Air's existing rows.
/// - Within a group, members keep that same order.
/// - Every space appears exactly once, collapsed or not, unless its group is
///   collapsed, in which case only the focused member (if any) stays visible.
pub(super) fn federated_rows<'a>(
    endpoints: &'a [ClientShellEndpoint],
    collapsed: &HashSet<String>,
    focused: Option<(usize, usize)>,
) -> Vec<FederatedRow> {
    // Every (endpoint, workspace) in catalog order, and the tier it groups by.
    let mut spaces: Vec<(usize, usize, Option<&'a str>)> = Vec::new();
    for (endpoint, machine) in endpoints.iter().enumerate() {
        let Some(snapshot) = machine.snapshot.as_deref() else {
            continue;
        };
        for (index, workspace) in snapshot.workspaces.iter().enumerate() {
            spaces.push((endpoint, index, tier_of(&workspace.label)));
        }
    }
    let mut members: HashMap<&'a str, usize> = HashMap::new();
    let mut has_child: HashSet<&'a str> = HashSet::new();
    for (endpoint, index, tier) in &spaces {
        let Some(tier) = tier else { continue };
        *members.entry(tier).or_default() += 1;
        let label = endpoints[*endpoint]
            .snapshot
            .as_deref()
            .map_or("", |snapshot| snapshot.workspaces[*index].label.as_str());
        if label.contains('/') {
            has_child.insert(tier);
        }
    }
    let grouped_tiers: HashSet<&str> = members
        .iter()
        .filter(|(tier, count)| has_child.contains(*tier) || **count >= 2)
        .map(|(tier, _)| *tier)
        .collect();
    let grouped = |tier: Option<&'a str>| tier.filter(|tier| grouped_tiers.contains(tier));

    let mut rows = Vec::new();
    let mut emitted: HashSet<&'a str> = HashSet::new();
    for &(endpoint, index, tier) in &spaces {
        let Some(tier) = grouped(tier) else {
            rows.push(FederatedRow::Workspace {
                endpoint,
                index,
                indented: false,
            });
            continue;
        };
        if !emitted.insert(tier) {
            continue;
        }
        let key = tier_key(tier);
        let is_collapsed = collapsed.contains(&key);
        rows.push(FederatedRow::Heading {
            key,
            label: tier.to_string(),
        });
        for &(member_endpoint, member_index, member_tier) in &spaces {
            if grouped(member_tier) != Some(tier) {
                continue;
            }
            if is_collapsed && focused != Some((member_endpoint, member_index)) {
                continue;
            }
            rows.push(FederatedRow::Workspace {
                endpoint: member_endpoint,
                index: member_index,
                indented: true,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{federated_rows, tier_key, FederatedRow};
    use crate::client::endpoint::{ClientEndpointId, ProfileId};
    use crate::client::shell::ClientShellEndpoint;
    use std::collections::HashSet;

    fn machine(id: ClientEndpointId, label: &str, spaces: &[&str]) -> ClientShellEndpoint {
        let mut endpoint = crate::client::shell::endpoints::local_endpoint();
        endpoint.endpoint_id = id;
        endpoint.label = label.into();
        let mut snapshot = crate::client::shell::tests::snapshot();
        let template = snapshot.workspaces[0].clone();
        snapshot.workspaces = spaces
            .iter()
            .enumerate()
            .map(|(number, label)| {
                let mut workspace = template.clone();
                workspace.workspace_id = format!("ws_{number}");
                workspace.label = (*label).into();
                workspace.focused = false;
                workspace
            })
            .collect();
        endpoint.snapshot = Some(Box::new(snapshot));
        endpoint
    }

    fn remote() -> ClientEndpointId {
        ClientEndpointId::Ssh(ProfileId::parse("8c855fe37b0e5b399606ef9ac72a2627").unwrap())
    }

    /// (heading or "<machine>:<space>", indented) in render order.
    fn shape(endpoints: &[ClientShellEndpoint], rows: &[FederatedRow]) -> Vec<(String, bool)> {
        rows.iter()
            .map(|row| match row {
                FederatedRow::Heading { label, .. } => (format!("[{label}]"), false),
                FederatedRow::Workspace {
                    endpoint,
                    index,
                    indented,
                } => {
                    let machine = &endpoints[*endpoint];
                    let snapshot = machine.snapshot.as_deref().unwrap();
                    (
                        format!("{}:{}", machine.label, snapshot.workspaces[*index].label),
                        *indented,
                    )
                }
            })
            .collect()
    }

    fn row(text: &str, indented: bool) -> (String, bool) {
        (text.into(), indented)
    }

    #[test]
    fn a_remote_worker_joins_the_air_work_tier_and_no_machine_row_exists() {
        let endpoints = [
            machine(
                ClientEndpointId::Local,
                "Local",
                &["assistant", "infra", "meta", "work/o-dev-5068"],
            ),
            machine(remote(), "m4", &["work/w-scratch"]),
        ];
        let rows = federated_rows(&endpoints, &HashSet::new(), None);
        assert_eq!(
            shape(&endpoints, &rows),
            vec![
                row("Local:assistant", false),
                row("Local:infra", false),
                row("Local:meta", false),
                row("[work]", false),
                row("Local:work/o-dev-5068", true),
                row("m4:work/w-scratch", true),
            ]
        );
    }

    #[test]
    fn the_air_tree_order_is_unchanged_by_adding_a_machine() {
        let air = || {
            machine(
                ClientEndpointId::Local,
                "Local",
                &[
                    "assistant",
                    "infra",
                    "meta",
                    "work/o-a",
                    "work/o-b",
                    "scratch",
                ],
            )
        };
        let alone = [air()];
        let with_m4 = [air(), machine(remote(), "m4", &["work/w-1", "infra"])];
        let air_only = |endpoints: &[ClientShellEndpoint]| {
            shape(endpoints, &federated_rows(endpoints, &HashSet::new(), None))
                .into_iter()
                .filter(|(text, _)| text.starts_with("Local:"))
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
        };
        assert_eq!(air_only(&alone), air_only(&with_m4));
    }

    #[test]
    fn the_same_tier_on_two_machines_is_one_heading() {
        let endpoints = [
            machine(ClientEndpointId::Local, "Local", &["infra"]),
            machine(remote(), "m4", &["infra"]),
        ];
        let rows = federated_rows(&endpoints, &HashSet::new(), None);
        assert_eq!(
            shape(&endpoints, &rows),
            vec![
                row("[infra]", false),
                row("Local:infra", true),
                row("m4:infra", true),
            ]
        );
    }

    #[test]
    fn collapsing_a_tier_hides_every_machine_member_but_the_focused_one() {
        let endpoints = [
            machine(ClientEndpointId::Local, "Local", &["work/o-a"]),
            machine(remote(), "m4", &["work/w-1", "work/w-2"]),
        ];
        let collapsed = HashSet::from([tier_key("work")]);
        let rows = federated_rows(&endpoints, &collapsed, Some((1, 1)));
        assert_eq!(
            shape(&endpoints, &rows),
            vec![row("[work]", false), row("m4:work/w-2", true)]
        );
    }

    #[test]
    fn every_space_on_every_machine_appears_exactly_once() {
        let endpoints = [
            machine(
                ClientEndpointId::Local,
                "Local",
                &["work", "work/o-a", "meta", "lone"],
            ),
            machine(remote(), "m4", &["work/w-1", "meta", "other", "work"]),
        ];
        let rows = federated_rows(&endpoints, &HashSet::new(), None);
        let mut seen = rows
            .iter()
            .filter_map(|row| match row {
                FederatedRow::Workspace {
                    endpoint, index, ..
                } => Some((*endpoint, *index)),
                FederatedRow::Heading { .. } => None,
            })
            .collect::<Vec<_>>();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                (0, 0),
                (0, 1),
                (0, 2),
                (0, 3),
                (1, 0),
                (1, 1),
                (1, 2),
                (1, 3)
            ]
        );
    }

    #[test]
    fn a_machine_with_no_snapshot_yet_adds_nothing_and_drops_nothing() {
        let mut offline = machine(remote(), "m4", &[]);
        offline.snapshot = None;
        let endpoints = [
            machine(ClientEndpointId::Local, "Local", &["assistant", "work/o-a"]),
            offline,
        ];
        let rows = federated_rows(&endpoints, &HashSet::new(), None);
        assert_eq!(
            shape(&endpoints, &rows),
            vec![
                row("Local:assistant", false),
                row("[work]", false),
                row("Local:work/o-a", true),
            ]
        );
    }
}

/// One agent in the merged tree: which machine, which agent of that machine's
/// snapshot, how deep it sits, and the tier heading drawn above it (first row
/// of each tier only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FederatedAgentRow {
    pub(super) endpoint: usize,
    pub(super) agent: usize,
    pub(super) depth: usize,
    pub(super) heading: Option<String>,
}

/// Tree mode across machines: every machine's agents in ONE tier-grouped
/// forest, the same shape `space_grouped_tree_rows` gives one machine.
///
/// - Tiers in tier order (assistant, infra, meta, work), then other spaces
///   alphabetically, then agents with no known space. Each tier heads once,
///   however many machines have agents in it.
/// - A parent resolves on the SAME machine first, so two machines that both run
///   an `infra` lane never cross-wire. A parent named on another machine is
///   then searched there, so an Air orchestrator's M4 worker nests under it.
/// - Positions, never pane ids, identify a row: pane ids repeat across
///   machines (both have a `w1:p1`), so keying by them would merge two agents.
/// - Every agent appears exactly once; a cycle or unknown parent is a root.
pub(super) fn federated_agent_tree(endpoints: &[ClientShellEndpoint]) -> Vec<FederatedAgentRow> {
    // Flatten every (machine, agent) with what the tree needs from it.
    struct Node<'a> {
        endpoint: usize,
        agent: usize,
        name: Option<&'a str>,
        parent: Option<&'a str>,
        sort_key: String,
        tie: (&'a str, usize),
        tier: String,
    }
    let mut nodes = Vec::new();
    for (endpoint, machine) in endpoints.iter().enumerate() {
        let Some(snapshot) = machine.snapshot.as_deref() else {
            continue;
        };
        for (agent_index, agent) in snapshot.agents.iter().enumerate() {
            let tier = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == agent.workspace_id)
                .map(|workspace| {
                    workspace
                        .label
                        .split_once('/')
                        .map_or(workspace.label.as_str(), |(tier, _)| tier)
                        .to_string()
                })
                .filter(|tier| !tier.is_empty())
                .unwrap_or_else(|| UNPLACED.to_string());
            nodes.push(Node {
                endpoint,
                agent: agent_index,
                name: agent.name.as_deref(),
                parent: agent.parent_agent.as_deref(),
                sort_key: agent
                    .name
                    .as_deref()
                    .or(agent.display_agent.as_deref())
                    .or(agent.terminal_title_stripped.as_deref())
                    .or(agent.terminal_title.as_deref())
                    .unwrap_or("")
                    .to_lowercase(),
                tie: (agent.pane_id.as_str(), endpoint),
                tier,
            });
        }
    }

    let mut by_name: HashMap<(usize, &str), usize> = HashMap::new();
    let mut by_name_anywhere: HashMap<&str, usize> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(name) = node.name {
            by_name.entry((node.endpoint, name)).or_insert(index);
            by_name_anywhere.entry(name).or_insert(index);
        }
    }
    let parent_of = |index: usize| -> Option<usize> {
        let node = &nodes[index];
        let parent = node.parent?;
        by_name
            .get(&(node.endpoint, parent))
            .or_else(|| by_name_anywhere.get(parent))
            .copied()
            .filter(|resolved| *resolved != index)
    };
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for index in 0..nodes.len() {
        match parent_of(index) {
            Some(parent) => children.entry(parent).or_default().push(index),
            None => roots.push(index),
        }
    }
    let order = |left: &usize, right: &usize| {
        nodes[*left]
            .sort_key
            .cmp(&nodes[*right].sort_key)
            .then_with(|| nodes[*left].tie.cmp(&nodes[*right].tie))
    };
    roots.sort_by(order);
    for siblings in children.values_mut() {
        siblings.sort_by(order);
    }

    // Depth-first per root; a subtree goes to its ROOT's tier.
    let mut subtrees: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    let mut emitted: HashSet<usize> = HashSet::new();
    let walk = |root: usize,
                emitted: &mut HashSet<usize>,
                subtrees: &mut Vec<(String, Vec<(usize, usize)>)>| {
        let mut stack = vec![(root, 0usize)];
        let mut subtree = Vec::new();
        while let Some((index, depth)) = stack.pop() {
            if !emitted.insert(index) {
                continue;
            }
            subtree.push((index, depth));
            if let Some(kids) = children.get(&index) {
                for child in kids.iter().rev() {
                    stack.push((*child, depth + 1));
                }
            }
        }
        if !subtree.is_empty() {
            subtrees.push((nodes[root].tier.clone(), subtree));
        }
    };
    for root in roots {
        walk(root, &mut emitted, &mut subtrees);
    }
    let mut stranded = (0..nodes.len())
        .filter(|index| !emitted.contains(index))
        .collect::<Vec<_>>();
    stranded.sort_by(order);
    for index in stranded {
        walk(index, &mut emitted, &mut subtrees);
    }

    let rank = |tier: &str| match TIERS.iter().position(|known| *known == tier) {
        Some(position) => (0, position, String::new()),
        None if tier == UNPLACED => (2, 0, String::new()),
        None => (1, 0, tier.to_lowercase()),
    };
    subtrees.sort_by_key(|(tier, _)| rank(tier));
    let mut rows = Vec::with_capacity(nodes.len());
    let mut previous: Option<String> = None;
    for (tier, subtree) in subtrees {
        for (position, (index, depth)) in subtree.into_iter().enumerate() {
            let heading =
                (position == 0 && previous.as_deref() != Some(tier.as_str())).then(|| tier.clone());
            rows.push(FederatedAgentRow {
                endpoint: nodes[index].endpoint,
                agent: nodes[index].agent,
                depth,
                heading,
            });
        }
        previous = Some(tier);
    }
    rows
}

/// The tier spaces in the order the operator reads them. Same list and order
/// as the one-machine tree in `agent_sidebar`.
const TIERS: [&str; 4] = ["assistant", "infra", "meta", "work"];
const UNPLACED: &str = "other";

#[cfg(test)]
mod agent_tree_tests {
    use super::{federated_agent_tree, FederatedAgentRow};
    use crate::client::endpoint::{ClientEndpointId, ProfileId};
    use crate::client::shell::ClientShellEndpoint;
    use crate::protocol::ClientShellAgent;

    fn agent(name: &str, workspace: &str, parent: Option<&str>) -> ClientShellAgent {
        ClientShellAgent {
            // Deliberately the SAME pane id on every machine: pane ids repeat
            // across machines, and the tree must not merge them.
            pane_id: "w1:p1".into(),
            workspace_id: workspace.into(),
            tab_id: "tab_1".into(),
            name: Some(name.into()),
            parent_agent: parent.map(str::to_string),
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Idle,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    fn machine(
        local: bool,
        spaces: &[(&str, &str)],
        agents: Vec<ClientShellAgent>,
    ) -> ClientShellEndpoint {
        let mut endpoint = crate::client::shell::endpoints::local_endpoint();
        if !local {
            endpoint.endpoint_id = ClientEndpointId::Ssh(
                ProfileId::parse("8c855fe37b0e5b399606ef9ac72a2627").unwrap(),
            );
            endpoint.label = "m4".into();
        }
        let mut snapshot = crate::client::shell::tests::snapshot();
        let template = snapshot.workspaces[0].clone();
        snapshot.workspaces = spaces
            .iter()
            .map(|(id, label)| {
                let mut workspace = template.clone();
                workspace.workspace_id = (*id).into();
                workspace.label = (*label).into();
                workspace
            })
            .collect();
        snapshot.agents = agents;
        endpoint.snapshot = Some(Box::new(snapshot));
        endpoint
    }

    /// (heading, "<machine>:<name>", depth) in render order.
    fn shape(
        endpoints: &[ClientShellEndpoint],
        rows: &[FederatedAgentRow],
    ) -> Vec<(Option<String>, String, usize)> {
        rows.iter()
            .map(|row| {
                let machine = &endpoints[row.endpoint];
                let name = machine.snapshot.as_deref().unwrap().agents[row.agent]
                    .name
                    .clone()
                    .unwrap();
                (
                    row.heading.clone(),
                    format!("{}:{name}", machine.label),
                    row.depth,
                )
            })
            .collect()
    }

    fn line(heading: Option<&str>, who: &str, depth: usize) -> (Option<String>, String, usize) {
        (heading.map(str::to_string), who.to_string(), depth)
    }

    #[test]
    fn an_m4_worker_nests_under_its_air_orchestrator_in_the_work_tier() {
        let endpoints = [
            machine(
                true,
                &[("wA", "assistant"), ("wM", "meta"), ("wO", "work/o-dev")],
                vec![
                    agent("assistant", "wA", None),
                    agent("m-lane", "wM", None),
                    agent("o-dev", "wO", None),
                ],
            ),
            machine(
                false,
                &[("wO", "work/o-dev")],
                vec![agent("w-scratch", "wO", Some("o-dev"))],
            ),
        ];
        assert_eq!(
            shape(&endpoints, &federated_agent_tree(&endpoints)),
            vec![
                line(Some("assistant"), "Local:assistant", 0),
                line(Some("meta"), "Local:m-lane", 0),
                line(Some("work"), "Local:o-dev", 0),
                line(None, "m4:w-scratch", 1),
            ]
        );
    }

    #[test]
    fn the_same_lane_name_on_two_machines_never_cross_wires() {
        // Both machines run an `infra` lane, each with its own worker. Each
        // worker must nest under ITS machine's lane, not the first one found.
        let endpoints = [
            machine(
                true,
                &[("wI", "infra")],
                vec![
                    agent("infra", "wI", None),
                    agent("w-air", "wI", Some("infra")),
                ],
            ),
            machine(
                false,
                &[("wI", "infra")],
                vec![
                    agent("infra", "wI", None),
                    agent("w-m4", "wI", Some("infra")),
                ],
            ),
        ];
        let rows = federated_agent_tree(&endpoints);
        let shaped = shape(&endpoints, &rows);
        assert_eq!(rows.len(), 4, "no agent dropped or merged: {shaped:?}");
        let pos = |who: &str| shaped.iter().position(|(_, w, _)| w == who).unwrap();
        assert_eq!(pos("Local:w-air"), pos("Local:infra") + 1);
        assert_eq!(pos("m4:w-m4"), pos("m4:infra") + 1);
        assert_eq!(
            shaped
                .iter()
                .filter(|(h, _, _)| h.as_deref() == Some("infra"))
                .count(),
            1
        );
    }

    #[test]
    fn identical_pane_ids_on_two_machines_are_two_rows() {
        let endpoints = [
            machine(true, &[("wM", "meta")], vec![agent("a", "wM", None)]),
            machine(false, &[("wM", "meta")], vec![agent("b", "wM", None)]),
        ];
        let rows = federated_agent_tree(&endpoints);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].endpoint, rows[1].endpoint);
    }

    #[test]
    fn the_air_agent_order_is_unchanged_by_adding_a_machine() {
        let air = || {
            machine(
                true,
                &[("wA", "assistant"), ("wI", "infra"), ("wO", "work/o-a")],
                vec![
                    agent("o-a", "wO", None),
                    agent("assistant", "wA", None),
                    agent("i-x", "wI", None),
                    agent("w-1", "wO", Some("o-a")),
                ],
            )
        };
        let local_only = |endpoints: &[ClientShellEndpoint]| {
            shape(endpoints, &federated_agent_tree(endpoints))
                .into_iter()
                .filter(|(_, who, _)| who.starts_with("Local:"))
                .map(|(_, who, depth)| (who, depth))
                .collect::<Vec<_>>()
        };
        let alone = [air()];
        let with_m4 = [
            air(),
            machine(
                false,
                &[("wO", "work/o-a"), ("wI", "infra")],
                vec![
                    agent("w-remote", "wO", Some("o-a")),
                    agent("i-remote", "wI", None),
                ],
            ),
        ];
        assert_eq!(local_only(&alone), local_only(&with_m4));
    }
}
