use super::*;
use ratatui::{
    text::Line,
    widgets::{Paragraph, Widget},
};

pub(in crate::client::shell) fn collapsed_sidebar_sections(
    area: Rect,
) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return (Rect::default(), None, Rect::default());
    }
    if content.height < 7 {
        return (content, None, Rect::default());
    }
    let workspace_height = content.height.div_ceil(2);
    let divider_y = content.y + workspace_height;
    let detail_height = content.height.saturating_sub(workspace_height + 1);
    (
        Rect::new(content.x, content.y, content.width, workspace_height),
        Some(divider_y),
        Rect::new(content.x, divider_y + 1, content.width, detail_height),
    )
}

pub(crate) fn render_collapsed_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    for (index, workspace) in snapshot
        .workspaces
        .iter()
        .take(workspace_area.height as usize)
        .enumerate()
    {
        let rect = Rect::new(
            workspace_area.x,
            workspace_area.y + index as u16,
            workspace_area.width,
            1,
        );
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        let selection_background =
            if workspace.focused && palette.selection_bg == ratatui::style::Color::Reset {
                palette.active_row_bg
            } else {
                palette.selection_bg
            };
        if selected {
            buffer.set_style(rect, Style::default().bg(selection_background));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        let number_style = if selected {
            Style::default()
                .fg(palette.overlay1)
                .bg(selection_background)
        } else if workspace.focused {
            Style::default().fg(palette.text).bg(palette.active_row_bg)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            number_style,
        );
        let status = workspace.agent_status;
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(status, config.status_indicators),
            Style::default().fg(status_color(status, palette)),
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: false,
            group_toggle: None,
        });
    }

    if let Some(divider_y) = divider_y {
        put_text(
            buffer,
            workspace_area.x,
            divider_y,
            workspace_area.width,
            &"─".repeat(workspace_area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
    }

    let detail_content = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    for (index, pane_id) in super::ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .take(detail_content.height as usize)
        .enumerate()
    {
        let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
        else {
            continue;
        };
        let rect = Rect::new(
            detail_content.x,
            detail_content.y + index as u16,
            detail_content.width,
            1,
        );
        if agent.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            Style::default().fg(if agent.focused {
                palette.text
            } else {
                palette.overlay0
            }),
        );
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(agent.agent_status, config.status_indicators),
            Style::default().fg(status_color(agent.agent_status, palette)),
        );
        hits.agents.push((rect, pane_id));
    }
    hits.sidebar_toggle = if area.is_empty() || workspace_area.width == 0 {
        Rect::default()
    } else {
        Rect::new(
            workspace_area.x + workspace_area.width / 2,
            area.bottom().saturating_sub(1),
            1,
            1,
        )
    };
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "»",
        if super::super::global_menu::global_menu_attention(snapshot) {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        },
    );
}

pub(crate) fn render_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    let (workspace_area, detail_area) =
        crate::ui::expanded_sidebar_sections(area, state.sidebar_section_split);
    hits.sidebar_section_divider =
        crate::ui::sidebar_section_divider_rect(area, state.sidebar_section_split);
    put_text(
        buffer,
        workspace_area.x,
        workspace_area.y,
        workspace_area.width,
        " spaces",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let entries = workspace_entries(snapshot, state.collapsed_groups);
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    let row_heights = entries
        .iter()
        .map(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .map(|workspace| {
                    workspace_rows(
                        workspace,
                        displayed_workspace_status(snapshot, workspace, state.collapsed_groups),
                        entry.indented,
                        &config.spaces,
                    )
                    .len()
                    .max(1)
                    .min(u16::MAX as usize) as u16
                })
                .unwrap_or(1)
        })
        .collect::<Vec<_>>();
    let gaps = entries
        .iter()
        .enumerate()
        .map(|(index, _)| {
            entries
                .get(index + 1)
                .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap)
        })
        .collect::<Vec<_>>();
    let mut metrics = super::scroll::list_scroll_metrics(
        &row_heights,
        &gaps,
        body.height,
        *state.workspace_scroll,
    );
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = entries
            .iter()
            .position(|entry| snapshot.workspaces[entry.index].focused)
        {
            *state.workspace_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
            );
        }
    }
    hits.workspace_max_scroll = metrics.max_offset_from_bottom;
    hits.workspace_scroll_metrics = Some(metrics);
    *state.workspace_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (entry_position, entry) in entries.iter().enumerate().skip(*state.workspace_scroll) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let status = displayed_workspace_status(snapshot, workspace, state.collapsed_groups);
        let rows = workspace_rows(workspace, status, entry.indented, &config.spaces);
        let row_height = (rows.len().max(1).min(u16::MAX as usize) as u16).min(body.height);
        if y.saturating_add(row_height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, row_height);
        let selected = state.selected_workspace_id.is_some_and(|target| {
            target.matches(state.active_endpoint_id, &workspace.workspace_id)
        });
        let dragged = state.dragged_workspace_id == Some(workspace.workspace_id.as_str());
        if selected {
            buffer.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if dragged {
            buffer.set_style(rect, Style::default().bg(palette.surface1));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        render_workspace_rows(
            buffer,
            rect,
            workspace,
            status,
            config.status_indicators,
            entry,
            rows,
            true,
            selected,
            dragged,
            palette,
        );
        let group_toggle = render_parent_group_toggle(
            buffer,
            rect,
            snapshot,
            entry.index,
            state.collapsed_groups,
            palette,
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: entry.indented,
            group_toggle,
        });
        let gap = entries
            .get(entry_position + 1)
            .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap);
        y = y.saturating_add(row_height + gap);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    if let Some(row) = state.workspace_drop_indicator_row.filter(|row| {
        *row >= workspace_area.y.saturating_add(1)
            && *row < workspace_area.bottom().saturating_sub(1)
    }) {
        put_text(
            buffer,
            body.x,
            row,
            body.width,
            &"─".repeat(body.width as usize),
            Style::default().fg(palette.accent),
        );
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            5.min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            " new",
            Style::default().fg(palette.overlay0),
        );
        let attention = super::super::global_menu::global_menu_attention(snapshot);
        let launcher_width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(launcher_width),
            footer_y,
            launcher_width,
            1,
        );
        if attention {
            let start_x = workspace_area.right().saturating_sub(6);
            put_text(
                buffer,
                start_x,
                footer_y,
                2,
                "● ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            put_text(
                buffer,
                start_x.saturating_add(2),
                footer_y,
                4,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        } else {
            put_right_text(
                buffer,
                workspace_area,
                footer_y,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        }
    }

    super::render_agent_panel(
        buffer,
        detail_area,
        snapshot,
        config,
        state.agent_scroll,
        hits,
    );

    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
}

/// Which sidebar group a space belongs to, and whether it heads that group.
///
/// Two kinds of group exist. A **worktree group** is a repository checkout
/// with its linked worktrees under it, keyed by the repository key. A **space
/// group** is a space labelled `<parent>` with the spaces labelled
/// `<parent>/<child>` under it — the operator's tiered layout, where `work`
/// holds one `work/<orchestrator>` space per orchestrator. Space-group keys are
/// namespaced (`space:<parent>`) so a space can never share collapse state with
/// a repository whose key happens to read the same.
///
/// Worktree grouping wins: a workspace already in a worktree group is never
/// regrouped by its label. A group needs its head AND at least one child; a
/// `work/x` whose `work` space does not exist stays a top-level row, visible,
/// rather than hanging off a group with no head.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarGroup {
    key: String,
    head: bool,
}

const SPACE_GROUP_PREFIX: &str = "space:";

fn sidebar_groups<'a>(snapshot: &'a ClientShellSnapshot) -> Vec<Option<SidebarGroup>> {
    // A worktree group's head is its FIRST non-linked member. A second
    // non-linked checkout of the same repository renders under it, so the
    // group is emitted once — never a second head repeating the group.
    let mut worktree_members = HashMap::<&str, usize>::new();
    let mut worktree_heads = HashMap::<&str, usize>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            *worktree_members.entry(&worktree.key).or_default() += 1;
            if !worktree.is_linked_worktree {
                worktree_heads.entry(&worktree.key).or_insert(index);
            }
        }
    }
    let in_worktree_group =
        |workspace: &'a ClientShellWorkspace| -> Option<&'a crate::protocol::ClientShellWorktree> {
            workspace.worktree.as_ref().filter(|worktree| {
                worktree_members
                    .get(worktree.key.as_str())
                    .copied()
                    .unwrap_or(0)
                    >= 2
                    && worktree_heads.contains_key(worktree.key.as_str())
            })
        };

    // Space heads: the FIRST space carrying a given label, so a duplicate label
    // renders as a child of the first rather than as a second head.
    let mut space_heads = HashMap::<&str, usize>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if in_worktree_group(workspace).is_none() && !workspace.label.contains('/') {
            space_heads.entry(workspace.label.as_str()).or_insert(index);
        }
    }
    let space_parent = |workspace: &'a ClientShellWorkspace| -> Option<&'a str> {
        let (parent, child) = workspace.label.split_once('/')?;
        (!parent.is_empty() && !child.is_empty() && space_heads.contains_key(parent))
            .then_some(parent)
    };
    let mut space_children = HashMap::<&str, usize>::new();
    for workspace in &snapshot.workspaces {
        if in_worktree_group(workspace).is_none() {
            if let Some(parent) = space_parent(workspace) {
                *space_children.entry(parent).or_default() += 1;
            }
        }
    }

    snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| {
            if let Some(worktree) = in_worktree_group(workspace) {
                return Some(SidebarGroup {
                    key: worktree.key.clone(),
                    head: worktree_heads.get(worktree.key.as_str()) == Some(&index),
                });
            }
            if let Some(parent) = space_parent(workspace) {
                return Some(SidebarGroup {
                    key: format!("{SPACE_GROUP_PREFIX}{parent}"),
                    head: false,
                });
            }
            (space_heads.get(workspace.label.as_str()) == Some(&index)
                && space_children.contains_key(workspace.label.as_str()))
            .then(|| SidebarGroup {
                key: format!("{SPACE_GROUP_PREFIX}{}", workspace.label),
                head: true,
            })
        })
        .collect()
}

pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let groups = sidebar_groups(snapshot);
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, group) in groups.iter().enumerate() {
        if let Some(group) = group {
            members.entry(group.key.as_str()).or_default().push(index);
        }
    }
    // A group renders where its HEAD sits, so a child listed ahead of its
    // parent is pulled under it rather than dragging the whole group up the
    // sidebar. Every group has a head by construction (`sidebar_groups` only
    // groups under an existing head), so skipping children here cannot drop
    // one.
    let mut entries = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let Some(group) = group else {
            entries.push(WorkspaceEntry {
                index,
                indented: false,
                last_child: false,
            });
            continue;
        };
        if !group.head {
            continue;
        }
        let Some(group_members) = members.get(group.key.as_str()) else {
            continue;
        };
        let parent = index;
        entries.push(WorkspaceEntry {
            index: parent,
            indented: false,
            last_child: false,
        });
        if collapsed_groups.contains(&group.key) {
            if let Some(active) = group_members
                .iter()
                .copied()
                .find(|member| *member != parent && snapshot.workspaces[*member].focused)
            {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| *member != parent)
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
            });
        }
    }
    entries
}

/// The collapse key of the group this space HEADS, if it heads one.
pub(in crate::client::shell) fn parent_group_key(
    snapshot: &ClientShellSnapshot,
    index: usize,
) -> Option<String> {
    sidebar_groups(snapshot)
        .into_iter()
        .nth(index)
        .flatten()
        .filter(|group| group.head)
        .map(|group| group.key)
}

/// Every space in the group this space heads, head included.
pub(in crate::client::shell) fn group_members(
    snapshot: &ClientShellSnapshot,
    index: usize,
) -> Vec<usize> {
    let groups = sidebar_groups(snapshot);
    let Some(key) = groups
        .get(index)
        .and_then(Option::as_ref)
        .filter(|group| group.head)
        .map(|group| group.key.clone())
    else {
        return vec![index];
    };
    groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.as_ref().is_some_and(|group| group.key == key))
        .map(|(member, _)| member)
        .collect()
}

pub(in crate::client::shell) fn render_parent_group_toggle(
    buffer: &mut Buffer,
    workspace_rect: Rect,
    snapshot: &ClientShellSnapshot,
    workspace_index: usize,
    collapsed_groups: &HashSet<String>,
    palette: &Palette,
) -> Option<(Rect, String)> {
    let key = parent_group_key(snapshot, workspace_index)?;
    let toggle = Rect::new(
        workspace_rect.right().saturating_sub(1),
        workspace_rect.y,
        1,
        1,
    );
    put_text(
        buffer,
        toggle.x,
        toggle.y,
        toggle.width,
        if collapsed_groups.contains(&key) {
            "▸"
        } else {
            "▾"
        },
        Style::default().fg(palette.accent),
    );
    Some((toggle, key))
}

pub(in crate::client::shell) fn displayed_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> crate::api::schema::AgentStatus {
    let Some(index) = snapshot
        .workspaces
        .iter()
        .position(|candidate| candidate.workspace_id == workspace.workspace_id)
    else {
        return workspace.agent_status;
    };
    let Some(key) = parent_group_key(snapshot, index) else {
        return workspace.agent_status;
    };
    if !collapsed_groups.contains(&key) {
        return workspace.agent_status;
    }
    group_members(snapshot, index)
        .into_iter()
        .map(|member| snapshot.workspaces[member].agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(workspace.agent_status)
}

pub(in crate::client::shell) fn workspace_rows(
    workspace: &ClientShellWorkspace,
    status: crate::api::schema::AgentStatus,
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<crate::ui::ResolvedToken>> {
    let label = if indented && !workspace.custom_label {
        workspace
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
            .unwrap_or(&workspace.label)
    } else {
        &workspace.label
    };
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    crate::ui::sidebar_space_rows(
        config,
        crate::ui::SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: status_text(status),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
}

pub(in crate::client::shell) fn render_workspace_rows(
    buffer: &mut Buffer,
    area: Rect,
    workspace: &ClientShellWorkspace,
    status: crate::api::schema::AgentStatus,
    indicators: crate::config::StatusIndicatorStyle,
    entry: &WorkspaceEntry,
    rows: Vec<Vec<crate::ui::ResolvedToken>>,
    endpoint_active: bool,
    selected: bool,
    dragged: bool,
    palette: &Palette,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            let prefix = if row_index == 0 {
                if entry.last_child {
                    "   └─ "
                } else {
                    "   ├─ "
                }
            } else if entry.last_child {
                "        "
            } else {
                "   │    "
            };
            x = put_segment(
                buffer,
                x,
                y,
                area.right(),
                prefix,
                Style::default().fg(palette.overlay0),
            );
        } else if row_index == 0 {
            x = x.saturating_add(1);
        } else {
            x = x.saturating_add(3);
        }
        let highlighted = endpoint_active && workspace.focused || dragged;
        let workspace_style = Style::default()
            .fg(if highlighted {
                palette.text
            } else {
                palette.subtext0
            })
            .add_modifier(if highlighted {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let secondary_style = Style::default().fg(if endpoint_active && workspace.focused {
            palette.mauve
        } else {
            palette.overlay0
        });
        let spans = crate::ui::resolved_token_spans(
            row,
            (
                status_icon(status, indicators),
                Style::default().fg(status_color(status, palette)),
            ),
            Style::default().fg(status_color(status, palette)),
            workspace_style,
            secondary_style,
            Style::default().fg(palette.overlay1),
            palette,
            area.right().saturating_sub(2).saturating_sub(x) as usize,
        );
        Paragraph::new(Line::from(spans)).render(
            Rect::new(x, y, area.right().saturating_sub(2).saturating_sub(x), 1),
            buffer,
        );
    }

    let background = if selected {
        Some(palette.selection_bg)
    } else if dragged {
        Some(palette.surface1)
    } else if endpoint_active && workspace.focused {
        Some(palette.active_row_bg)
    } else {
        None
    };
    if let Some(background) = background {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_bg(background);
            }
        }
    }
}

#[cfg(test)]
mod space_group_tests {
    use super::{displayed_workspace_status, parent_group_key, workspace_entries};
    use crate::api::schema::AgentStatus;
    use crate::protocol::{ClientShellSnapshot, ClientShellWorkspace, ClientShellWorktree};
    use std::collections::HashSet;

    fn space(id: &str, label: &str) -> ClientShellWorkspace {
        let mut workspace = crate::client::shell::tests::snapshot().workspaces[0].clone();
        workspace.workspace_id = id.into();
        workspace.label = label.into();
        workspace.custom_label = true;
        workspace.focused = false;
        workspace.worktree = None;
        workspace
    }

    fn fleet(spaces: Vec<ClientShellWorkspace>) -> ClientShellSnapshot {
        let mut snapshot = crate::client::shell::tests::snapshot();
        snapshot.workspaces = spaces;
        snapshot
    }

    /// (label, indented) in render order: what the operator sees.
    fn shape(snapshot: &ClientShellSnapshot, collapsed: &HashSet<String>) -> Vec<(String, bool)> {
        workspace_entries(snapshot, collapsed)
            .into_iter()
            .map(|entry| {
                (
                    snapshot.workspaces[entry.index].label.clone(),
                    entry.indented,
                )
            })
            .collect()
    }

    fn row(label: &str, indented: bool) -> (String, bool) {
        (label.into(), indented)
    }

    #[test]
    fn a_slash_label_nests_under_the_space_it_names() {
        // Deliberately out of order: a child listed before its parent must still
        // render under it, and an unrelated space between them must not split
        // the group.
        let snapshot = fleet(vec![
            space("w1", "work/o-b"),
            space("w2", "assistant"),
            space("w3", "work"),
            space("w4", "work/o-a"),
            space("w5", "meta"),
        ]);
        assert_eq!(
            shape(&snapshot, &HashSet::new()),
            vec![
                row("assistant", false),
                row("work", false),
                row("work/o-b", true),
                row("work/o-a", true),
                row("meta", false),
            ]
        );
    }

    #[test]
    fn a_slash_label_with_no_parent_space_is_a_top_level_row() {
        // The parent space is missing (not yet created, or closed). The child
        // must stay visible rather than vanish into a group with no head.
        let snapshot = fleet(vec![space("w1", "meta"), space("w2", "work/o-a")]);
        assert_eq!(
            shape(&snapshot, &HashSet::new()),
            vec![row("meta", false), row("work/o-a", false)]
        );
    }

    #[test]
    fn a_parent_space_with_no_children_has_no_toggle() {
        let snapshot = fleet(vec![space("w1", "work"), space("w2", "meta")]);
        assert_eq!(parent_group_key(&snapshot, 0), None);
    }

    #[test]
    fn collapsing_the_parent_space_hides_its_children_but_keeps_the_focused_one() {
        let mut snapshot = fleet(vec![
            space("w1", "work"),
            space("w2", "work/o-a"),
            space("w3", "work/o-b"),
        ]);
        snapshot.workspaces[2].focused = true;
        let key = parent_group_key(&snapshot, 0).expect("work heads a group");
        let collapsed = HashSet::from([key]);
        assert_eq!(
            shape(&snapshot, &collapsed),
            vec![row("work", false), row("work/o-b", true)]
        );
    }

    #[test]
    fn a_collapsed_parent_space_reports_its_most_urgent_child() {
        let mut snapshot = fleet(vec![space("w1", "work"), space("w2", "work/o-a")]);
        snapshot.workspaces[0].agent_status = AgentStatus::Idle;
        snapshot.workspaces[1].agent_status = AgentStatus::Blocked;
        let key = parent_group_key(&snapshot, 0).expect("work heads a group");
        let collapsed = HashSet::from([key]);
        assert_eq!(
            displayed_workspace_status(&snapshot, &snapshot.workspaces[0], &collapsed),
            AgentStatus::Blocked
        );
    }

    #[test]
    fn a_space_group_key_cannot_collide_with_a_worktree_group_key() {
        // Worktree group keys are repository paths; a space literally labelled
        // with one must not share its collapse state.
        let snapshot = fleet(vec![space("w1", "work"), space("w2", "work/o-a")]);
        let key = parent_group_key(&snapshot, 0).expect("work heads a group");
        assert_ne!(key, "work");
        assert!(!key.starts_with('/'));
    }

    #[test]
    fn worktree_grouping_still_wins_for_a_workspace_in_a_worktree_group() {
        let mut repo = space("w1", "repo");
        repo.worktree = Some(ClientShellWorktree {
            key: "/repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let mut linked = space("w2", "repo/feature");
        linked.worktree = Some(ClientShellWorktree {
            key: "/repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        let snapshot = fleet(vec![repo, linked]);
        assert_eq!(
            shape(&snapshot, &HashSet::new()),
            vec![row("repo", false), row("repo/feature", true)]
        );
        assert_eq!(parent_group_key(&snapshot, 0).as_deref(), Some("/repo"));
    }

    #[test]
    fn two_checkouts_of_one_repo_share_one_head_and_no_row_repeats() {
        // Two NON-linked workspaces on the same repository key: the first is
        // the head, the second renders under it — never a second head that
        // re-emits the whole group.
        let checkout = |id: &str, label: &str, linked: bool| {
            let mut workspace = space(id, label);
            workspace.worktree = Some(ClientShellWorktree {
                key: "/repo".into(),
                label: "repo".into(),
                is_linked_worktree: linked,
            });
            workspace
        };
        let snapshot = fleet(vec![
            checkout("w1", "repo", false),
            checkout("w2", "repo-again", false),
            checkout("w3", "repo/feature", true),
        ]);
        assert_eq!(
            shape(&snapshot, &HashSet::new()),
            vec![
                row("repo", false),
                row("repo-again", true),
                row("repo/feature", true),
            ]
        );
    }

    #[test]
    fn every_space_appears_exactly_once() {
        let snapshot = fleet(vec![
            space("w1", "work/o-a"),
            space("w2", "work"),
            space("w3", "work/o-a"),
            space("w4", "infra"),
            space("w5", "work/"),
        ]);
        let entries = workspace_entries(&snapshot, &HashSet::new());
        let mut indices = entries.iter().map(|entry| entry.index).collect::<Vec<_>>();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }
}
