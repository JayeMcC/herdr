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
    /// Space heading drawn on its own line above this row (tree mode only).
    pub(super) heading: Option<String>,
}

impl AgentRow {
    /// Lines this row occupies, its heading included.
    pub(super) fn line_count(&self) -> usize {
        self.rows.len().max(1) + usize::from(self.heading.is_some())
    }
}

/// One row of the agent panel in tree order: the pane to render and how deep it
/// sits. Depth is what the renderer turns into indentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentTreeRow {
    pub(super) pane_id: String,
    pub(super) depth: usize,
    /// The space heading shown above this row: set on the first row of each
    /// space group, `None` on every other row.
    pub(super) heading: Option<String>,
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
            heading: None,
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
        heading: None,
    }));

    rows
}

/// The fleet's tier spaces, in the order the operator reads them.
const TIER_SPACES: [&str; 4] = ["assistant", "infra", "meta", "work"];
/// Heading for agents whose space is not in the snapshot.
const UNPLACED_SPACE: &str = "other";

/// The space an agent is grouped under: the top-level part of its space's
/// label, so an agent in `work/o-dev-5068` groups under `work`.
fn top_level_space<'a>(snapshot: &'a ClientShellSnapshot, workspace_id: &str) -> &'a str {
    snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .map(|workspace| {
            workspace
                .label
                .split_once('/')
                .map_or(workspace.label.as_str(), |(parent, _)| parent)
        })
        .filter(|label| !label.is_empty())
        .unwrap_or(UNPLACED_SPACE)
}

/// Tree mode: agents grouped by space first (tier spaces in tier order, then
/// any other space alphabetically, then agents with no known space), each
/// group the same parent forest `tree_ordered_rows` builds.
///
/// A subtree is placed by its ROOT's space, so a worker always renders under
/// its orchestrator even when its tab sits in a different space — splitting a
/// worker away from the agent that spawned it is exactly what the tree exists
/// to prevent. Grouping only reorders whole subtrees, so every agent still
/// appears exactly once.
pub(super) fn space_grouped_tree_rows(snapshot: &ClientShellSnapshot) -> Vec<AgentTreeRow> {
    let forest = tree_ordered_rows(&snapshot.agents.iter().collect::<Vec<_>>());
    let space_of = |pane_id: &str| {
        snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
            .map_or(UNPLACED_SPACE, |agent| {
                top_level_space(snapshot, &agent.workspace_id)
            })
    };
    let mut subtrees: Vec<(&str, Vec<AgentTreeRow>)> = Vec::new();
    for row in forest {
        if row.depth == 0 || subtrees.is_empty() {
            subtrees.push((space_of(&row.pane_id), vec![row]));
        } else if let Some((_, subtree)) = subtrees.last_mut() {
            subtree.push(row);
        }
    }
    let rank = |space: &str| match TIER_SPACES.iter().position(|tier| *tier == space) {
        Some(position) => (0, position, String::new()),
        None if space == UNPLACED_SPACE => (2, 0, String::new()),
        None => (1, 0, space.to_lowercase()),
    };
    // Stable: subtrees keep their alphabetical order within a space.
    subtrees.sort_by_key(|(space, _)| rank(space));
    let mut rows = Vec::with_capacity(snapshot.agents.len());
    let mut previous: Option<&str> = None;
    for (space, subtree) in subtrees {
        for (index, mut row) in subtree.into_iter().enumerate() {
            if index == 0 && previous != Some(space) {
                row.heading = Some(space.to_string());
            }
            rows.push(row);
        }
        previous = Some(space);
    }
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
    Some(space_grouped_tree_rows(snapshot))
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

    let mut rows = agent_rows(snapshot, config, None);
    for row in &mut rows {
        row.keep_full_tab_label(area.width);
    }
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
        AgentRow::line_count,
        |buffer, rect, row, hits| {
            // The heading line is a label, not the agent: clicking it must not
            // focus the first agent under it.
            let heading_lines = u16::from(row.heading.is_some()).min(rect.height);
            hits.agents.push((
                Rect::new(
                    rect.x,
                    rect.y + heading_lines,
                    rect.width,
                    rect.height - heading_lines,
                ),
                row.pane_id.clone(),
            ));
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
                built.heading = row.heading;
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
        heading: None,
    })
}

/// Keep a tab label whole (operator 2026-09-29: a tab using a stack is labelled
/// `<lane> · <stack>`, and the panel must show all of it).
///
/// The shared token fitter shrinks or drops a label that does not fit, which on
/// a 26-column sidebar cuts the stack name off — the one part the operator
/// asked to see. So when a row's tab label is wider than the row allows, the
/// label leaves that row and is wrapped onto lines of its own, breaking at
/// ` · ` first so the stack reads as one unit, and mid-word only when a single
/// part is wider than a whole line. The rest of the row is left exactly as it
/// was, and a label that fits is not touched.
pub(super) fn fit_full_tab_label(
    rows: Vec<Vec<crate::ui::ResolvedToken>>,
    width: usize,
) -> Vec<Vec<crate::ui::ResolvedToken>> {
    use crate::ui::{ResolvedToken, ResolvedTokenKind};
    let width = width.max(1);
    let fits = |row: &[ResolvedToken], width: usize| {
        let mut total = 0;
        for (index, token) in row.iter().enumerate() {
            if index > 0 {
                total += display_width(crate::ui::token_separator(&row[index - 1], token));
            }
            total += match &token.kind {
                ResolvedTokenKind::StateIcon => 1,
                ResolvedTokenKind::StateText(text)
                | ResolvedTokenKind::Machine(text)
                | ResolvedTokenKind::Workspace(text)
                | ResolvedTokenKind::Tab(text)
                | ResolvedTokenKind::Pane(text)
                | ResolvedTokenKind::Agent(text)
                | ResolvedTokenKind::TerminalTitle(text)
                | ResolvedTokenKind::Branch(text)
                | ResolvedTokenKind::Custom(text) => display_width(text),
                ResolvedTokenKind::GitStatus { .. } => 0,
            };
        }
        total <= width
    };
    // Only the first line of a row gets `width`; every later line is indented
    // two more columns by `render_agent_row`.
    let continuation = width.saturating_sub(2).max(1);
    let mut out: Vec<Vec<ResolvedToken>> = Vec::with_capacity(rows.len());
    for row in rows {
        let line_width = if out.is_empty() { width } else { continuation };
        let Some(tab_index) = row
            .iter()
            .position(|token| matches!(token.kind, ResolvedTokenKind::Tab(_)))
        else {
            out.push(row);
            continue;
        };
        if fits(&row, line_width) {
            out.push(row);
            continue;
        }
        let mut row = row;
        let tab = row.remove(tab_index);
        let ResolvedTokenKind::Tab(label) = &tab.kind else {
            out.push(row);
            continue;
        };
        if !row.is_empty() {
            out.push(row);
        }
        for line in wrap_label(label, continuation) {
            out.push(vec![ResolvedToken::new(
                ResolvedTokenKind::Tab(line),
                tab.style,
            )]);
        }
    }
    out
}

impl AgentRow {
    /// Apply `fit_full_tab_label` at the width this row's FIRST line renders
    /// in: the panel width less the first-line indent (`depth_indent + 1`,
    /// the arithmetic `render_agent_row` uses). Continuation lines indent two
    /// more columns, so they get that much less.
    pub(super) fn keep_full_tab_label(&mut self, panel_width: u16) {
        let depth_indent = (self.depth * 2).min(panel_width.saturating_sub(8) as usize);
        let width = (panel_width as usize).saturating_sub(depth_indent + 1);
        self.rows = fit_full_tab_label(std::mem::take(&mut self.rows), width);
    }
}

/// Wrap at ` · ` boundaries, then by display width for any part still too wide.
fn wrap_label(label: &str, width: usize) -> Vec<String> {
    let mut parts = label.split(" · ");
    let mut lines = Vec::new();
    let mut current = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let joined = format!("{current} · {part}");
        if display_width(&joined) <= width {
            current = joined;
        } else {
            lines.push(std::mem::take(&mut current));
            current = format!("· {part}");
        }
    }
    lines.push(current);
    lines
        .into_iter()
        .flat_map(|line| {
            let mut chunks = Vec::new();
            let mut chunk = String::new();
            for character in line.chars() {
                let candidate = format!("{chunk}{character}");
                if display_width(&candidate) > width && !chunk.is_empty() {
                    chunks.push(std::mem::take(&mut chunk));
                }
                chunk.push(character);
            }
            chunks.push(chunk);
            chunks
        })
        .collect()
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
    let mut rect = rect;
    if let Some(heading) = row.heading.as_deref().filter(|_| rect.height > 0) {
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(" {heading}"),
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        );
        rect = Rect::new(rect.x, rect.y + 1, rect.width, rect.height - 1);
    }
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

#[cfg(test)]
mod space_tree_tests {
    use super::{space_grouped_tree_rows, AgentTreeRow};
    use crate::protocol::{ClientShellAgent, ClientShellSnapshot};

    fn fleet(
        spaces: &[(&str, &str)],
        agents: &[(&str, &str, Option<&str>)],
    ) -> ClientShellSnapshot {
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
        snapshot.agents = agents
            .iter()
            .map(|(name, workspace, parent)| ClientShellAgent {
                pane_id: format!("pane-{name}"),
                workspace_id: (*workspace).into(),
                tab_id: "tab_1".into(),
                name: Some((*name).into()),
                parent_agent: parent.map(str::to_string),
                display_agent: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: crate::api::schema::AgentStatus::Idle,
                state_change_seq: 0,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
            })
            .collect();
        snapshot
    }

    /// (heading, pane, depth): the heading is shown above the row that carries it.
    fn shape(rows: &[AgentTreeRow]) -> Vec<(Option<&str>, &str, usize)> {
        rows.iter()
            .map(|row| (row.heading.as_deref(), row.pane_id.as_str(), row.depth))
            .collect()
    }

    #[test]
    fn agents_group_by_space_in_tier_order_with_workers_under_their_orchestrator() {
        // The operator's example: spaces in tier order, not snapshot order, and
        // each orchestrator's workers nested under it, alphabetical per level.
        let snapshot = fleet(
            &[
                ("wW", "work"),
                ("wM", "meta"),
                ("wA", "assistant"),
                ("w5068", "work/o-dev-5068"),
                ("w4086", "work/o-4086-review"),
            ],
            &[
                ("w-walk-per-ac", "w4086", Some("o-4086-review")),
                ("m-hub-tier-rules", "wM", None),
                ("o-dev-5068", "w5068", None),
                ("w-bind-reuse", "w4086", Some("o-4086-review")),
                ("o-4086-review", "w4086", None),
                ("w-fix-redirect", "w5068", Some("o-dev-5068")),
                ("assistant", "wA", None),
            ],
        );
        assert_eq!(
            shape(&space_grouped_tree_rows(&snapshot)),
            vec![
                (Some("assistant"), "pane-assistant", 0),
                (Some("meta"), "pane-m-hub-tier-rules", 0),
                (Some("work"), "pane-o-4086-review", 0),
                (None, "pane-w-bind-reuse", 1),
                (None, "pane-w-walk-per-ac", 1),
                (None, "pane-o-dev-5068", 0),
                (None, "pane-w-fix-redirect", 1),
            ]
        );
    }

    #[test]
    fn a_worker_stays_under_its_parent_even_in_another_space() {
        // Nesting is the operator's ask; a worker whose tab sits elsewhere
        // still renders under its orchestrator rather than being split off.
        let snapshot = fleet(
            &[("wM", "meta"), ("wW", "work")],
            &[("m-lane", "wM", None), ("w-task", "wW", Some("m-lane"))],
        );
        assert_eq!(
            shape(&space_grouped_tree_rows(&snapshot)),
            vec![(Some("meta"), "pane-m-lane", 0), (None, "pane-w-task", 1)]
        );
    }

    #[test]
    fn an_unknown_space_sorts_after_the_tiers_and_an_orphan_agent_is_kept() {
        let snapshot = fleet(
            &[("wZ", "scratch"), ("wI", "infra")],
            &[
                ("probe", "wZ", None),
                ("i-fleet", "wI", None),
                ("lost", "w-gone", None),
            ],
        );
        let rows = space_grouped_tree_rows(&snapshot);
        assert_eq!(rows.len(), 3, "no agent may be dropped: {rows:?}");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("infra"), "pane-i-fleet", 0),
                (Some("scratch"), "pane-probe", 0),
                (Some("other"), "pane-lost", 0),
            ]
        );
    }
}

#[cfg(test)]
mod space_tree_render_tests {
    use crate::client::shell::tests::snapshot;
    use crate::client::shell::{ClientShellConfig, ClientShellState};
    use crate::config::{AgentPanelSortConfig, Config};
    use crate::protocol::ClientShellAgent;

    #[test]
    fn tree_mode_draws_a_space_heading_and_keeps_it_out_of_the_click_target() {
        let mut config = Config::default();
        config.ui.agent_panel_sort = AgentPanelSortConfig::Tree;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        let mut fleet = snapshot();
        fleet.workspaces[0].label = "work/o-dev-5068".into();
        let mut work = fleet.workspaces[0].clone();
        work.workspace_id = "ws_work".into();
        work.label = "work".into();
        work.focused = false;
        fleet.workspaces.push(work);
        fleet.agents = vec![ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("o-dev-5068".into()),
            parent_agent: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Working,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }];
        state.set_snapshot(Box::new(fleet));
        state.set_pane_surface(crate::client::shell::tests::surface());
        let frame = state.compose(106, 40).expect("composed frame");
        let lines = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let (agent_hit, _) = state.hits.agents.first().cloned().expect("agent hit");
        let heading_row = agent_hit.y as usize - 1;
        assert!(
            lines[heading_row].trim_start().starts_with("work"),
            "heading line above the agent: {:?}",
            lines[heading_row]
        );
    }
}

#[cfg(test)]
mod full_tab_label_tests {
    use super::fit_full_tab_label;
    use crate::ui::{ResolvedToken, ResolvedTokenKind};

    fn text_of(rows: &[Vec<ResolvedToken>]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .filter_map(|token| match &token.kind {
                        ResolvedTokenKind::Tab(text) | ResolvedTokenKind::Workspace(text) => {
                            Some(text.clone())
                        }
                        ResolvedTokenKind::StateIcon => Some("*".into()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    }

    fn row(kinds: Vec<ResolvedTokenKind>) -> Vec<ResolvedToken> {
        kinds
            .into_iter()
            .map(|kind| ResolvedToken::new(kind, Default::default()))
            .collect()
    }

    #[test]
    fn a_tab_label_that_fits_is_left_alone() {
        let rows = vec![row(vec![
            ResolvedTokenKind::StateIcon,
            ResolvedTokenKind::Tab("o-dev · s1".into()),
        ])];
        assert_eq!(
            text_of(&fit_full_tab_label(rows.clone(), 40)),
            text_of(&rows)
        );
    }

    #[test]
    fn a_long_tab_label_moves_to_its_own_lines_and_is_never_cut() {
        let label = "o-4088-4090-triage · w1-s9";
        let rows = vec![row(vec![
            ResolvedTokenKind::StateIcon,
            ResolvedTokenKind::Workspace("work".into()),
            ResolvedTokenKind::Tab(label.into()),
        ])];
        let fitted = fit_full_tab_label(rows, 16);
        let lines = text_of(&fitted);
        assert_eq!(lines[0], "*|work", "the rest of the row stays: {lines:?}");
        // A break at " · " consumes the space before the dot, so compare the
        // visible characters: nothing of the label may be dropped or cut.
        let visible = |text: &str| {
            text.chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>()
        };
        assert_eq!(
            visible(&lines[1..].concat()),
            visible(label),
            "every character of the label survives: {lines:?}"
        );
        for line in &lines[1..] {
            assert!(
                unicode_width::UnicodeWidthStr::width(line.as_str()) <= 14,
                "a continuation line fits the width less its 2-column indent: {lines:?}"
            );
        }
    }

    #[test]
    fn the_stack_suffix_is_kept_together_when_it_fits_on_a_line() {
        // Break at the " · " separator first, so the stack name reads as one
        // unit rather than being split mid-word.
        let rows = vec![row(vec![
            ResolvedTokenKind::StateIcon,
            ResolvedTokenKind::Tab("o-4086-review · s2".into()),
        ])];
        let lines = text_of(&fit_full_tab_label(rows, 16));
        assert!(
            lines.contains(&"· s2".to_string()),
            "stack kept whole: {lines:?}"
        );
    }
}

#[cfg(test)]
mod full_tab_label_render_tests {
    use crate::client::shell::tests::{snapshot, surface};
    use crate::client::shell::{ClientShellConfig, ClientShellState};
    use crate::config::Config;
    use crate::protocol::ClientShellAgent;

    fn frame_text(state: &mut ClientShellState, width: u16) -> String {
        let frame = state.compose(width, 40).expect("composed frame");
        frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_default_sidebar_shows_every_character_of_a_stack_tab_label() {
        // Default config: 26-column sidebar, where the shared fitter used to
        // cut `<lane> · <stack>` short and drop the stack.
        let label = "o-4088-4090-triage · w1-s9";
        let mut fleet = snapshot();
        fleet.tabs[0].label = label.into();
        fleet.tabs[0].custom_label = true;
        fleet.agents = vec![ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("o-4088-4090-triage".into()),
            parent_agent: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Working,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }];
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(fleet));
        state.set_pane_surface(surface());
        let text = frame_text(&mut state, 106);
        let sidebar = text
            .lines()
            .map(|line| line.chars().take(26).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        for piece in ["o-4088-4090-tri", "w1-s9"] {
            assert!(
                sidebar.contains(piece),
                "{piece} missing from sidebar:\n{sidebar}"
            );
        }
        assert!(
            !sidebar.contains('…'),
            "no part of the label may be ellipsised:\n{sidebar}"
        );
    }
}
