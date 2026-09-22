use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
    /// Nesting depth in tree mode; 0 in every other mode, so the existing
    /// modes render exactly as they did before.
    pub(super) depth: usize,
}

/// One row of the agent panel in tree order: the pane to render and how deep it
/// sits. Depth is what the renderer turns into indentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentTreeRow {
    pub(super) pane_id: String,
    pub(super) depth: usize,
}

/// The name a tree row sorts under, and the one a child names as its parent.
fn tree_sort_key(agent: &crate::protocol::ClientShellAgent) -> String {
    agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.terminal_title_stripped.as_deref())
        .or(agent.terminal_title.as_deref())
        .unwrap_or("")
        .to_lowercase()
}

/// Arrange agents as a forest: every agent under the agent that spawned it,
/// siblings alphabetical within each level.
///
/// Three properties matter more than the shape of the tree, because each one
/// is a way rows could VANISH — and a panel that hides a working agent is worse
/// than a panel with no tree at all:
///
///  1. An agent whose parent is unknown — unrecorded, already exited, or simply
///     never set — is a ROOT. It is not dropped and not hidden under anything.
///     This is the common case, not the exception: every agent started by hand,
///     and every agent predating the parent field, has no parent.
///  2. A cycle cannot starve a row. Parenting is validated at spawn so the
///     graph should already be a forest, but a restored session could still
///     present one, and "should not happen" is not a reason to lose an agent.
///     Any agent not reached by the descent is emitted at the end.
///  3. Every input appears exactly once. The count out equals the count in.
fn tree_ordered_rows(agents: &[&crate::protocol::ClientShellAgent]) -> Vec<AgentTreeRow> {
    use std::collections::{HashMap, HashSet};

    // Index by name so a child can find its parent. An agent with no name
    // cannot BE a parent (nothing can name it), but is perfectly fine as a
    // child or a root.
    let by_name: HashMap<&str, usize> = agents
        .iter()
        .enumerate()
        .filter_map(|(index, agent)| agent.name.as_deref().map(|name| (name, index)))
        .collect();

    let parent_of = |index: usize| -> Option<usize> {
        let parent = agents[index].parent_agent.as_deref()?;
        by_name
            .get(parent)
            .copied()
            // An agent naming itself as parent is not renderable as a child of
            // itself; treat it as a root rather than dropping it.
            .filter(|resolved| *resolved != index)
    };

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for index in 0..agents.len() {
        match parent_of(index) {
            Some(parent) => children.entry(parent).or_default().push(index),
            None => roots.push(index),
        }
    }

    let sort_siblings = |siblings: &mut Vec<usize>| {
        // Case-insensitive, because a fleet named by convention mixes cases and
        // a byte-order sort would file every capitalised agent above every
        // lowercase one. Pane id breaks ties so the order is stable rather than
        // dependent on however the snapshot happened to arrive.
        siblings.sort_by(|left, right| {
            tree_sort_key(agents[*left])
                .cmp(&tree_sort_key(agents[*right]))
                .then_with(|| agents[*left].pane_id.cmp(&agents[*right].pane_id))
        });
    };

    sort_siblings(&mut roots);
    for siblings in children.values_mut() {
        sort_siblings(siblings);
    }

    let mut rows = Vec::with_capacity(agents.len());
    let mut emitted = HashSet::new();
    // Explicit stack rather than recursion: depth is bounded only by what a
    // restored session contains, and a deep chain must not blow the stack.
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|root| (root, 0)).collect();
    while let Some((index, depth)) = stack.pop() {
        if !emitted.insert(index) {
            continue;
        }
        rows.push(AgentTreeRow {
            pane_id: agents[index].pane_id.clone(),
            depth,
        });
        if let Some(kids) = children.get(&index) {
            for child in kids.iter().rev() {
                stack.push((*child, depth + 1));
            }
        }
    }

    // Anything the descent never reached is inside a cycle. Emit it at the root
    // so it stays visible and reachable by keyboard.
    let mut stranded = (0..agents.len())
        .filter(|index| !emitted.contains(index))
        .collect::<Vec<_>>();
    sort_siblings(&mut stranded);
    rows.extend(stranded.into_iter().map(|index| AgentTreeRow {
        pane_id: agents[index].pane_id.clone(),
        depth: 0,
    }));

    rows
}

/// Tree rows for the panel, or `None` when the active sort is not the tree.
pub(super) fn agent_tree_rows(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Option<Vec<AgentTreeRow>> {
    if snapshot.agent_view_label.is_some() || sort != crate::config::AgentPanelSortConfig::Tree {
        return None;
    }
    Some(tree_ordered_rows(
        &snapshot.agents.iter().collect::<Vec<_>>(),
    ))
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    if let Some(rows) = agent_tree_rows(snapshot, sort) {
        // Navigation reads the same order it renders: if the panel shows a
        // child under its parent, ctrl-n must move there and not to whatever
        // the unsorted snapshot happened to list next.
        return rows.into_iter().map(|row| row.pane_id).collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
    ) {
        return;
    }

    let rows = agent_rows(snapshot, config, None);
    render_agent_list(
        buffer,
        area,
        &rows,
        snapshot
            .agent_view_label
            .as_ref()
            .map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.rows.len(),
        |buffer, rect, row, hits| {
            hits.agents.push((rect, row.pane_id.clone()));
            render_agent_row(buffer, rect, row, config);
        },
    );
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> bool {
    if area.height == 0 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(config.palette.surface_dim),
    );
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "grouped",
        crate::config::AgentPanelSortConfig::Priority => "priority",
        crate::config::AgentPanelSortConfig::Alphabetical => "a-z",
        crate::config::AgentPanelSortConfig::Tree => "tree",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    rows: &[T],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    hits.agent_body = body;
    if body.is_empty() || rows.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let row_heights = rows
        .iter()
        .map(|row| row_lines(row).max(1).min(u16::MAX as usize) as u16)
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    let metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        render_row(buffer, rect, row, hits);
        y = y
            .saturating_add(height)
            .saturating_add(if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            });
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    if let Some(tree) = agent_tree_rows(snapshot, config.agent_panel_sort) {
        return tree
            .into_iter()
            .filter_map(|row| {
                let mut built = agent_row(snapshot, &row.pane_id, config, machine)?;
                built.depth = row.depth;
                Some(built)
            })
            .collect();
    }
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
        depth: 0,
    })
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    let icon = (
        status_icon(row.status, config.status_indicators),
        Style::default().fg(status_color(row.status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    // Two columns per level, on top of the row's existing indent. Capped so a
    // deep chain cannot push an agent's name off a narrow sidebar — a row that
    // is indented past the edge is a row you cannot read, which is the same
    // failure as hiding it.
    let depth_indent = (row.depth * 2).min(rect.width.saturating_sub(8) as usize);
    for (index, tokens) in rows.iter().take(rect.height as usize).enumerate() {
        let indent = depth_indent + if index == 0 { 1 } else { 3 };
        let mut spans = vec![ratatui::text::Span::raw(" ".repeat(indent))];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            rect.width.saturating_sub(indent as u16) as usize,
        ));
        Paragraph::new(Line::from(spans)).style(row_style).render(
            Rect::new(rect.x, rect.y + index as u16, rect.width, 1),
            buffer,
        );
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}

#[cfg(test)]
mod tree_tests {
    use super::{tree_ordered_rows, AgentTreeRow};
    use crate::api::schema::AgentStatus;
    use crate::protocol::ClientShellAgent;

    fn agent(name: &str, parent: Option<&str>) -> ClientShellAgent {
        ClientShellAgent {
            // Distinct from the name so an assertion on pane ids cannot pass by
            // accidentally comparing names to themselves.
            pane_id: format!("pane-{name}"),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some(name.into()),
            parent_agent: parent.map(str::to_string),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    fn order(agents: &[ClientShellAgent]) -> Vec<AgentTreeRow> {
        tree_ordered_rows(&agents.iter().collect::<Vec<_>>())
    }

    /// (pane_id, depth) pairs, which is exactly what the renderer consumes.
    fn shape(rows: &[AgentTreeRow]) -> Vec<(&str, usize)> {
        rows.iter()
            .map(|row| (row.pane_id.as_str(), row.depth))
            .collect()
    }

    #[test]
    fn children_nest_under_the_agent_that_spawned_them() {
        // The operator's ask: assistant -> orchestrators -> workers. Input is
        // deliberately in the WRONG order, so passing requires actually
        // rebuilding the hierarchy rather than echoing the input back.
        let agents = [
            agent("worker-b", Some("orchestrator")),
            agent("assistant", None),
            agent("orchestrator", Some("assistant")),
            agent("worker-a", Some("orchestrator")),
        ];
        assert_eq!(
            shape(&order(&agents)),
            vec![
                ("pane-assistant", 0),
                ("pane-orchestrator", 1),
                ("pane-worker-a", 2),
                ("pane-worker-b", 2),
            ]
        );
    }

    #[test]
    fn siblings_sort_alphabetically_regardless_of_case() {
        // "Banana" and "apple" DISCRIMINATE the two sorts: byte order puts
        // every uppercase letter before every lowercase one, so a
        // case-sensitive sort yields [Banana, apple] while case-insensitive
        // yields [apple, Banana]. A pair like ["zeta", "Alpha"] sorts
        // identically either way and would pass with the bug present — that
        // exact mistake was made once in this repo already.
        let agents = [
            agent("root", None),
            agent("Banana", Some("root")),
            agent("apple", Some("root")),
        ];
        assert_eq!(
            shape(&order(&agents)),
            vec![("pane-root", 0), ("pane-apple", 1), ("pane-Banana", 1)]
        );
    }

    #[test]
    fn an_agent_with_no_parent_renders_at_the_root() {
        // The hard requirement: an unparented agent must never vanish. This is
        // the common case — every hand-started agent, and every agent that
        // predates the parent field.
        let agents = [agent("loner", None), agent("other", None)];
        assert_eq!(
            shape(&order(&agents)),
            vec![("pane-loner", 0), ("pane-other", 0)]
        );
    }

    #[test]
    fn an_agent_whose_parent_is_missing_renders_at_the_root() {
        // A parent that exited, or was never in this snapshot. The child must
        // surface at the root rather than be hidden under a row that is not
        // there — the failure mode is a working agent you cannot see.
        let agents = [agent("orphan", Some("ghost")), agent("present", None)];
        let rows = order(&agents);
        assert_eq!(rows.len(), 2, "no agent may be dropped: {rows:?}");
        assert_eq!(shape(&rows), vec![("pane-orphan", 0), ("pane-present", 0)]);
    }

    #[test]
    fn a_cycle_still_renders_every_agent() {
        // Parenting is validated at spawn, so a cycle should not occur — but a
        // restored session could still present one, and "should not happen" is
        // not a reason to lose an agent from the panel.
        let agents = [
            agent("a", Some("b")),
            agent("b", Some("a")),
            agent("free", None),
        ];
        let rows = order(&agents);
        assert_eq!(rows.len(), 3, "a cycle must not starve a row: {rows:?}");
        let panes = rows
            .iter()
            .map(|row| row.pane_id.as_str())
            .collect::<Vec<_>>();
        for expected in ["pane-a", "pane-b", "pane-free"] {
            assert!(
                panes.contains(&expected),
                "{expected} missing from {panes:?}"
            );
        }
    }

    #[test]
    fn an_agent_naming_itself_as_its_parent_is_a_root() {
        let agents = [agent("self", Some("self"))];
        assert_eq!(shape(&order(&agents)), vec![("pane-self", 0)]);
    }

    #[test]
    fn every_agent_appears_exactly_once() {
        // The invariant that covers the failure modes no individual case
        // names: the count out equals the count in, with no duplicates.
        let agents = [
            agent("assistant", None),
            agent("meta", Some("assistant")),
            agent("orchestrator", Some("assistant")),
            agent("worker", Some("orchestrator")),
            agent("orphan", Some("ghost")),
            agent("loner", None),
        ];
        let rows = order(&agents);
        assert_eq!(rows.len(), agents.len());
        let mut panes = rows
            .iter()
            .map(|row| row.pane_id.clone())
            .collect::<Vec<_>>();
        panes.sort();
        panes.dedup();
        assert_eq!(panes.len(), agents.len(), "a pane was emitted twice");
    }

    #[test]
    fn a_deep_chain_keeps_increasing_depth() {
        // Depth is what becomes indentation, so it has to keep counting past
        // the two levels the tier model names.
        let agents = [
            agent("l0", None),
            agent("l1", Some("l0")),
            agent("l2", Some("l1")),
            agent("l3", Some("l2")),
        ];
        assert_eq!(
            shape(&order(&agents)),
            vec![
                ("pane-l0", 0),
                ("pane-l1", 1),
                ("pane-l2", 2),
                ("pane-l3", 3),
            ]
        );
    }
}
