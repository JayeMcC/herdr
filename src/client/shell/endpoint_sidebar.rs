use super::render::{display_width, put_right_text, put_text, ShellRenderState};
use super::*;

fn collapsed_groups_for_endpoint<'a>(
    state: &'a ShellRenderState<'_>,
    endpoint_id: &ClientEndpointId,
) -> Option<&'a HashSet<String>> {
    if endpoint_id.is_local() {
        Some(state.collapsed_groups)
    } else {
        state.remote_collapsed_groups.get(endpoint_id)
    }
}

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    super::render::render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = super::sidebar::collapsed_sidebar_sections(area);
    // Collapsed: every machine's spaces as one numbered strip, in the merged
    // tier order and with no machine rows, so the narrow sidebar never
    // re-groups by machine either. Nothing is collapsed here: the strip lists
    // every space so each stays one click away.
    let spaces = super::federated_tree::federated_rows(state.endpoints, &HashSet::new(), None)
        .into_iter()
        .filter_map(|row| match row {
            super::federated_tree::FederatedRow::Workspace {
                endpoint, index, ..
            } => Some((endpoint, index)),
            super::federated_tree::FederatedRow::Heading { .. } => None,
        })
        .collect::<Vec<_>>();
    let reveal = std::mem::take(state.reveal_navigation_workspace);
    let selected_row = reveal
        .then(|| {
            state.selected_workspace_id.and_then(|target| {
                spaces.iter().position(|(endpoint, index)| {
                    let machine = &state.endpoints[*endpoint];
                    machine.snapshot.as_deref().is_some_and(|snapshot| {
                        target.matches(
                            &machine.endpoint_id,
                            &snapshot.workspaces[*index].workspace_id,
                        )
                    })
                })
            })
        })
        .flatten();
    let height = usize::from(workspace_area.height);
    let max_scroll = spaces.len().saturating_sub(height);
    *state.workspace_scroll = (*state.workspace_scroll).min(max_scroll);
    if let Some(row) = selected_row {
        if row < *state.workspace_scroll {
            *state.workspace_scroll = row;
        } else if row >= state.workspace_scroll.saturating_add(height) {
            *state.workspace_scroll = row.saturating_add(1).saturating_sub(height).min(max_scroll);
        }
    }
    hits.workspace_max_scroll = max_scroll;
    let mut y = workspace_area.y;
    for (endpoint_index, index) in spaces.into_iter().skip(*state.workspace_scroll) {
        if y >= workspace_area.bottom() {
            break;
        }
        let endpoint = &state.endpoints[endpoint_index];
        let Some(snapshot) = endpoint.snapshot.as_deref() else {
            continue;
        };
        let Some(workspace) = snapshot.workspaces.get(index) else {
            continue;
        };
        let active = &endpoint.endpoint_id == state.active_endpoint_id;
        {
            let rect = Rect::new(workspace_area.x, y, workspace_area.width, 1);
            let focused = active && workspace.focused;
            let selected = state.selected_workspace_id.is_some_and(|target| {
                target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
            });
            let selection_background = if palette.selection_bg == ratatui::style::Color::Reset {
                palette.active_row_bg
            } else {
                palette.selection_bg
            };
            if selected {
                buffer.set_style(rect, Style::default().bg(selection_background));
            } else if focused {
                buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
            }
            let stale = endpoint.status != ClientEndpointStatus::Online;
            let number = format!(" {}", workspace.number);
            let number_width = super::render::display_width(&number).min(rect.width);
            let dim = if stale {
                Modifier::DIM
            } else {
                Modifier::empty()
            };
            put_text(
                buffer,
                rect.x,
                rect.y,
                number_width,
                &number,
                Style::default()
                    .fg(if focused && !stale {
                        palette.text
                    } else {
                        palette.overlay0
                    })
                    .add_modifier(dim),
            );
            put_text(
                buffer,
                rect.x.saturating_add(number_width),
                rect.y,
                rect.width.saturating_sub(number_width),
                status_icon(workspace.agent_status, config.status_indicators),
                Style::default()
                    .fg(if stale {
                        palette.overlay0
                    } else {
                        status_color(workspace.agent_status, palette)
                    })
                    .add_modifier(dim),
            );
            hits.workspaces.push(WorkspaceHit {
                rect,
                endpoint_id: endpoint.endpoint_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                indented: false,
                group_toggle: None,
            });
            y = y.saturating_add(1);
        }
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
    super::endpoint_agents::render_collapsed(
        buffer,
        detail_area,
        state.endpoints,
        state.active_endpoint_id,
        config,
        hits,
    );
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
        Style::default().fg(palette.overlay0),
    );
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    active_snapshot: Option<&ClientShellSnapshot>,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    super::render::render_sidebar_background(buffer, area, palette);
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

    let empty_collapsed_groups = HashSet::new();

    enum Row {
        Workspace {
            endpoint: usize,
            entry: WorkspaceEntry,
        },
        Heading {
            key: String,
            label: String,
        },
    }
    // THE TIER TREE IS THE GROUPING, WHATEVER THE NUMBER OF MACHINES
    // (operator 2026-10-05: where an agent runs "shouldn't be the primary
    // identifier"). Every machine's spaces merge under shared tier headings;
    // no machine row is drawn. A merged heading collapses through the local
    // collapse set, under the same `space:` key one machine uses.
    let focused = state
        .endpoints
        .iter()
        .enumerate()
        .find_map(|(endpoint, machine)| {
            (&machine.endpoint_id == state.active_endpoint_id)
                .then(|| {
                    machine
                        .snapshot
                        .as_deref()?
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.focused)
                        .map(|index| (endpoint, index))
                })
                .flatten()
        });
    let rows =
        super::federated_tree::federated_rows(state.endpoints, state.collapsed_groups, focused)
            .into_iter()
            .map(|row| match row {
                super::federated_tree::FederatedRow::Workspace {
                    endpoint,
                    index,
                    indented,
                } => Row::Workspace {
                    endpoint,
                    entry: WorkspaceEntry {
                        index,
                        indented,
                        last_child: false,
                    },
                },
                super::federated_tree::FederatedRow::Heading { key, label } => {
                    Row::Heading { key, label }
                }
            })
            .collect::<Vec<_>>();
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    let row_heights = rows
        .iter()
        .map(|row| match row {
            Row::Heading { .. } => 1,
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                let collapsed_groups = collapsed_groups_for_endpoint(state, &endpoint.endpoint_id)
                    .unwrap_or(&empty_collapsed_groups);
                endpoint
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| {
                        let workspace = snapshot.workspaces.get(entry.index)?;
                        Some(
                            super::sidebar::workspace_rows(
                                workspace,
                                super::sidebar::displayed_workspace_status(
                                    snapshot,
                                    workspace,
                                    collapsed_groups,
                                ),
                                entry.indented,
                                &config.spaces,
                            )
                            .len()
                            .max(1)
                            .min(u16::MAX as usize) as u16,
                        )
                    })
                    .unwrap_or(1)
            }
        })
        .collect::<Vec<_>>();
    // One tree, so the gap rules are the single-machine ones: a gap before
    // every top-level row and every heading, none before an indented child.
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| match rows.get(index + 1) {
            Some(Row::Workspace { entry, .. }) => {
                u16::from(!entry.indented) * config.spaces.row_gap
            }
            Some(Row::Heading { .. }) => config.spaces.row_gap,
            None => 0,
        })
        .collect::<Vec<_>>();
    let reveal_navigation = !body.is_empty() && std::mem::take(state.reveal_navigation_workspace);
    let reveal_focus = !body.is_empty() && std::mem::take(state.reveal_focused_workspace);
    if reveal_navigation || reveal_focus {
        let selected_row = rows.iter().position(|row| match row {
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                endpoint
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.workspaces.get(entry.index))
                    .is_some_and(|workspace| {
                        if reveal_navigation {
                            state.selected_workspace_id.is_some_and(|target| {
                                target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
                            })
                        } else {
                            &endpoint.endpoint_id == state.active_endpoint_id
                                && active_snapshot.is_some_and(|snapshot| {
                                    snapshot.focused_workspace_id.as_deref()
                                        == Some(workspace.workspace_id.as_str())
                                })
                        }
                    })
            }
            Row::Heading { .. } => false,
        });
        if let Some(selected_row) = selected_row {
            *state.workspace_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
                selected_row,
            );
        }
    }
    let metrics = super::scroll::list_scroll_metrics(
        &row_heights,
        &gaps,
        body.height,
        *state.workspace_scroll,
    );
    hits.workspace_max_scroll = metrics.max_offset_from_bottom;
    hits.workspace_scroll_metrics = Some(metrics);
    *state.workspace_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (row_index, row) in rows.iter().enumerate().skip(*state.workspace_scroll) {
        match row {
            Row::Heading { key, label } => {
                if y >= body.bottom() {
                    break;
                }
                let rect = Rect::new(body.x, y, content_width, 1);
                render_tier_heading(
                    buffer,
                    rect,
                    state.endpoints,
                    key,
                    label,
                    state.collapsed_groups,
                    config.status_indicators,
                    palette,
                );
                hits.space_headings
                    .push((rect, ClientEndpointId::Local, key.clone()));
                y = y
                    .saturating_add(1)
                    .saturating_add(gaps.get(row_index).copied().unwrap_or(0));
            }
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                let Some(snapshot) = endpoint.snapshot.as_deref() else {
                    continue;
                };
                let Some(workspace) = snapshot.workspaces.get(entry.index) else {
                    continue;
                };
                let collapsed_groups = collapsed_groups_for_endpoint(state, &endpoint.endpoint_id)
                    .unwrap_or(&empty_collapsed_groups);
                let status = super::sidebar::displayed_workspace_status(
                    snapshot,
                    workspace,
                    collapsed_groups,
                );
                let tokens = super::sidebar::workspace_rows(
                    workspace,
                    status,
                    entry.indented,
                    &config.spaces,
                );
                let height = (tokens.len().max(1).min(u16::MAX as usize) as u16).min(body.height);
                if y.saturating_add(height) > body.bottom() {
                    break;
                }
                let rect = Rect::new(body.x, y, content_width, height);
                let nested = rect;
                let endpoint_active = &endpoint.endpoint_id == state.active_endpoint_id;
                let selected = state.selected_workspace_id.is_some_and(|target| {
                    target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
                });
                super::sidebar::render_workspace_rows(
                    buffer,
                    nested,
                    workspace,
                    status,
                    config.status_indicators,
                    entry,
                    tokens,
                    endpoint_active,
                    selected,
                    false,
                    palette,
                );
                if selected && palette.selection_bg == ratatui::style::Color::Reset {
                    buffer.set_style(nested, Style::default().bg(palette.active_row_bg));
                }
                if endpoint.status != ClientEndpointStatus::Online {
                    buffer.set_style(
                        rect,
                        Style::default()
                            .fg(palette.overlay0)
                            .add_modifier(Modifier::DIM),
                    );
                }
                // In the merged tree only a tier heading collapses: a space
                // never heads a group here, so it carries no toggle of its own.
                let group_toggle = None;
                hits.workspaces.push(WorkspaceHit {
                    rect,
                    endpoint_id: endpoint.endpoint_id.clone(),
                    workspace_id: workspace.workspace_id.clone(),
                    indented: entry.indented,
                    group_toggle,
                });
                y = y
                    .saturating_add(height)
                    .saturating_add(gaps.get(row_index).copied().unwrap_or(0));
            }
        }
    }
    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        let label = format!(" new · {}", active_endpoint_label(state));
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            display_width(&label).min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            &label,
            Style::default().fg(palette.overlay0),
        );
        let attention = active_snapshot.is_some_and(super::global_menu::global_menu_attention);
        let width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(width),
            footer_y,
            width,
            1,
        );
        put_right_text(
            buffer,
            workspace_area,
            footer_y,
            if attention { "● menu" } else { "menu" },
            Style::default().fg(if attention {
                palette.accent
            } else {
                palette.overlay0
            }),
        );
    }
    super::endpoint_agents::render_expanded(
        buffer,
        detail_area,
        active_snapshot.and_then(|snapshot| snapshot.agent_view_label.as_deref()),
        state.endpoints,
        state.active_endpoint_id,
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

fn active_endpoint_label<'a>(state: &'a ShellRenderState<'_>) -> &'a str {
    state
        .endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == state.active_endpoint_id)
        .map_or("Local", |endpoint| endpoint.label.as_str())
}

/// A tier heading shared across machines: the most urgent status among every
/// machine's spaces in the tier, the tier name, and the collapse toggle.
#[allow(clippy::too_many_arguments)] // mirrors render_space_heading's inputs plus the machine list
fn render_tier_heading(
    buffer: &mut Buffer,
    rect: Rect,
    endpoints: &[ClientShellEndpoint],
    key: &str,
    label: &str,
    collapsed_groups: &HashSet<String>,
    indicators: crate::config::StatusIndicatorStyle,
    palette: &Palette,
) {
    if rect.is_empty() {
        return;
    }
    let status = endpoints
        .iter()
        .filter_map(|endpoint| endpoint.snapshot.as_deref())
        .flat_map(|snapshot| snapshot.workspaces.iter())
        .filter(|workspace| {
            workspace
                .label
                .split_once('/')
                .map_or(workspace.label.as_str(), |(tier, _)| tier)
                == label
        })
        .map(|workspace| workspace.agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(crate::api::schema::AgentStatus::Unknown);
    let heading = format!("{} {label}", status_icon(status, indicators));
    put_text(
        buffer,
        rect.x.saturating_add(1),
        rect.y,
        rect.width.saturating_sub(2),
        &heading,
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        buffer,
        rect.x.saturating_add(1),
        rect.y,
        1,
        status_icon(status, indicators),
        Style::default().fg(status_color(status, palette)),
    );
    put_text(
        buffer,
        rect.right().saturating_sub(1),
        rect.y,
        1,
        if collapsed_groups.contains(key) {
            "▸"
        } else {
            "▾"
        },
        Style::default().fg(palette.accent),
    );
}
