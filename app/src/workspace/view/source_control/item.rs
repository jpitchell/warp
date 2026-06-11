//! The flat list-item model for the Source Control panel plus the per-item
//! renderers. The list is a single `Vec<SourceControlListItem>` consumed by a
//! `UniformList` (every item renders at [`ITEM_HEIGHT`]); collapsed sections
//! contribute their header only.

use std::collections::HashSet;

use pathfinder_color::ColorU;
use warp_core::ui::Icon;
use warpui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, DispatchEventResult, Element, EventHandler,
    Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseInBehavior, MouseStateHandle,
    ParentElement, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::AppContext;

use super::view::SourceControlViewAction;
use crate::appearance::Appearance;
use crate::code_review::diff_state::GitFileStatus;
use crate::source_control::{CommitEntry, FileChange, RepoStatus, StashEntry, WorktreeEntry};
use crate::ui_components::buttons::icon_button_with_color;

#[cfg(test)]
#[path = "item_tests.rs"]
mod tests;

/// Fixed height for every list item — `UniformList` sizes all rows from the
/// first one, so headers and rows must match.
pub const ITEM_HEIGHT: f32 = 36.;

const STATUS_LETTER_WIDTH: f32 = 14.;
const ACTION_BUTTON_SIZE: f32 = 22.;
const ROW_LEFT_PADDING: f32 = 24.;
const HEADER_LEFT_PADDING: f32 = 8.;
const ROW_RIGHT_PADDING: f32 = 8.;

/// A section of the panel's flat list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Conflicts,
    Staged,
    Changes,
    Untracked,
    Stashes,
    Worktrees,
    Commits,
}

impl Section {
    pub fn title(&self) -> &'static str {
        match self {
            Section::Conflicts => "Merge Conflicts",
            Section::Staged => "Staged Changes",
            Section::Changes => "Changes",
            Section::Untracked => "Untracked",
            Section::Stashes => "Stashes",
            Section::Worktrees => "Worktrees",
            Section::Commits => "Commits",
        }
    }
}

/// One row of the panel's uniform list.
#[derive(Clone, Debug)]
pub enum SourceControlListItem {
    SectionHeader { section: Section, count: usize },
    File { section: Section, change: FileChange },
    Stash(StashEntry),
    Worktree(WorktreeEntry),
    Commit(CommitEntry),
    EmptyHint { section: Section, text: &'static str },
}

impl SourceControlListItem {
    /// Stable identity for hover-state bookkeeping across rebuilds.
    pub fn state_key(&self) -> String {
        match self {
            Self::SectionHeader { section, .. } => format!("header:{section:?}"),
            Self::File { section, change } => format!("file:{section:?}:{}", change.path),
            Self::Stash(stash) => format!("stash:{}", stash.index),
            Self::Worktree(worktree) => format!("worktree:{}", worktree.path.display()),
            Self::Commit(commit) => format!("commit:{}", commit.sha),
            Self::EmptyHint { section, .. } => format!("hint:{section:?}"),
        }
    }

    /// Whether keyboard navigation can land on this item.
    pub fn is_selectable(&self) -> bool {
        match self {
            Self::File { .. } | Self::Stash(_) | Self::Worktree(_) | Self::Commit(_) => true,
            Self::SectionHeader { .. } | Self::EmptyHint { .. } => false,
        }
    }
}

/// Builds the flat item list from the model's current data. Pure so the
/// section ordering / collapse behavior is unit-testable.
pub fn build_list_items(
    status: Option<&RepoStatus>,
    stashes: &[StashEntry],
    worktrees: &[WorktreeEntry],
    history: &[CommitEntry],
    collapsed: &HashSet<Section>,
) -> Vec<SourceControlListItem> {
    let mut items = Vec::new();

    let empty: Vec<FileChange> = Vec::new();
    let (conflicted, staged, unstaged, untracked) = match status {
        Some(status) => (
            &status.conflicted,
            &status.staged,
            &status.unstaged,
            &status.untracked,
        ),
        None => (&empty, &empty, &empty, &empty),
    };

    let mut push_file_section = |items: &mut Vec<SourceControlListItem>,
                                 section: Section,
                                 changes: &[FileChange]| {
        items.push(SourceControlListItem::SectionHeader {
            section,
            count: changes.len(),
        });
        if !collapsed.contains(&section) {
            items.extend(changes.iter().map(|change| SourceControlListItem::File {
                section,
                change: change.clone(),
            }));
        }
    };

    // Merge Conflicts only appears when there are conflicted files.
    if !conflicted.is_empty() {
        push_file_section(&mut items, Section::Conflicts, conflicted);
    }
    push_file_section(&mut items, Section::Staged, staged);
    push_file_section(&mut items, Section::Changes, unstaged);
    push_file_section(&mut items, Section::Untracked, untracked);

    items.push(SourceControlListItem::SectionHeader {
        section: Section::Stashes,
        count: stashes.len(),
    });
    if !collapsed.contains(&Section::Stashes) {
        if stashes.is_empty() {
            items.push(SourceControlListItem::EmptyHint {
                section: Section::Stashes,
                text: "No stashes",
            });
        } else {
            items.extend(stashes.iter().cloned().map(SourceControlListItem::Stash));
        }
    }

    items.push(SourceControlListItem::SectionHeader {
        section: Section::Worktrees,
        count: worktrees.len(),
    });
    if !collapsed.contains(&Section::Worktrees) {
        items.extend(
            worktrees
                .iter()
                .cloned()
                .map(SourceControlListItem::Worktree),
        );
    }

    items.push(SourceControlListItem::SectionHeader {
        section: Section::Commits,
        count: history.len(),
    });
    if !collapsed.contains(&Section::Commits) {
        if history.is_empty() {
            items.push(SourceControlListItem::EmptyHint {
                section: Section::Commits,
                text: "Loading commits\u{2026}",
            });
        } else {
            items.extend(history.iter().cloned().map(SourceControlListItem::Commit));
        }
    }

    items
}

/// Hover / per-row mouse state. `actions` covers up to four hover action
/// buttons per row.
#[derive(Clone, Default)]
pub struct ItemState {
    pub row: MouseStateHandle,
    pub actions: [MouseStateHandle; 4],
}

/// A right-aligned hover action button on a row or section header.
pub struct RowAction {
    pub icon: Icon,
    pub tooltip: &'static str,
    pub action: SourceControlViewAction,
}

/// The single-character status indicator for a file change.
pub fn status_letter(status: &GitFileStatus) -> &'static str {
    match status {
        GitFileStatus::New => "A",
        GitFileStatus::Modified => "M",
        GitFileStatus::Deleted => "D",
        GitFileStatus::Renamed { .. } => "R",
        GitFileStatus::Copied { .. } => "C",
        GitFileStatus::Untracked => "U",
        GitFileStatus::Conflicted => "!",
    }
}

/// Status-letter color, all via theme accessors (no hard-coded colors).
pub fn status_color(status: &GitFileStatus, appearance: &Appearance) -> ColorU {
    let theme = appearance.theme();
    match status {
        GitFileStatus::New | GitFileStatus::Copied { .. } => theme.ansi_fg_green(),
        GitFileStatus::Modified | GitFileStatus::Renamed { .. } => theme.ansi_fg_yellow(),
        GitFileStatus::Deleted | GitFileStatus::Conflicted => theme.ansi_fg_red(),
        GitFileStatus::Untracked => theme.sub_text_color(theme.background()).into_solid(),
    }
}

fn render_row_actions(
    actions: Vec<RowAction>,
    state: &ItemState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let ui_builder = appearance.ui_builder().clone();
    let icon_color = theme.sub_text_color(theme.background());
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min)
        .with_spacing(2.);
    let _ = app;
    for (ix, action) in actions.into_iter().enumerate().take(4) {
        let tooltip = ui_builder.tool_tip(action.tooltip.to_string()).build();
        let dispatched = action.action;
        row.add_child(
            ConstrainedBox::new(
                icon_button_with_color(
                    appearance,
                    action.icon,
                    false,
                    state.actions[ix].clone(),
                    icon_color,
                )
                .with_tooltip(move || tooltip.finish())
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(dispatched.clone());
                })
                .with_cursor(Cursor::PointingHand)
                .finish(),
            )
            .with_width(ACTION_BUTTON_SIZE)
            .with_height(ACTION_BUTTON_SIZE)
            .finish(),
        );
    }
    row.finish()
}

/// Shared row scaffolding: fixed height, hover background, selection hover
/// actions, mouse-in selection tracking, and an optional row click action.
#[allow(clippy::too_many_arguments)]
fn render_row_base(
    content: Box<dyn Element>,
    actions: Vec<RowAction>,
    left_padding: f32,
    index: usize,
    is_selected: bool,
    state: &ItemState,
    on_click: Option<SourceControlViewAction>,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let show_actions = is_selected && !actions.is_empty();
    let actions_element = show_actions.then(|| render_row_actions(actions, state, appearance, app));

    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(Shrinkable::new(1.0, content).finish());
    if let Some(actions_element) = actions_element {
        row.add_child(actions_element);
    }
    let row = row.finish();

    let hoverable = Hoverable::new(state.row.clone(), move |_| {
        let mut container = Container::new(row)
            .with_padding_left(left_padding)
            .with_padding_right(ROW_RIGHT_PADDING);
        if is_selected {
            container = container.with_background(theme.surface_overlay_1());
        }
        container.finish()
    })
    .with_defer_events_to_children();

    let hoverable = if let Some(action) = on_click {
        hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(action.clone());
            })
            .finish()
    } else {
        hoverable.finish()
    };

    EventHandler::new(
        ConstrainedBox::new(hoverable)
            .with_height(ITEM_HEIGHT)
            .finish(),
    )
    .on_mouse_in(
        move |ctx, _, _| {
            ctx.dispatch_typed_action(SourceControlViewAction::SetSelectedIndex(index));
            DispatchEventResult::PropagateToParent
        },
        Some(MouseInBehavior {
            fire_on_synthetic_events: false,
            fire_when_covered: true,
        }),
    )
    .finish()
}

/// Renders a collapsible section header with a count badge and hover actions.
#[allow(clippy::too_many_arguments)]
pub fn render_section_header(
    section: Section,
    count: usize,
    is_collapsed: bool,
    actions: Vec<RowAction>,
    state: &ItemState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();

    let chevron_icon = if is_collapsed {
        Icon::ChevronRight
    } else {
        Icon::ChevronDown
    };
    let header_color = theme.sub_text_color(theme.background());

    let mut content = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.)
        .with_child(
            ConstrainedBox::new(chevron_icon.to_warpui_icon(header_color).finish())
                .with_width(12.)
                .with_height(12.)
                .finish(),
        )
        .with_child(
            Text::new_inline(section.title(), font, appearance.ui_font_size() - 1.)
                .with_color(header_color.into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        );
    if count > 0 {
        content.add_child(
            Container::new(
                Text::new_inline(count.to_string(), font, appearance.ui_font_size() - 3.)
                    .with_color(header_color.into())
                    .finish(),
            )
            .with_horizontal_padding(6.)
            .with_background(theme.surface_overlay_1())
            .with_corner_radius(warpui::elements::CornerRadius::with_all(
                warpui::elements::Radius::Pixels(7.),
            ))
            .finish(),
        );
    }

    let state_clone = state.clone();
    let content = content.finish();
    let row = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_child(content);

    let actions_for_render = actions;
    let appearance_for_render = appearance.clone();
    let hoverable = Hoverable::new(state.row.clone(), {
        let app_ptr = app as *const AppContext;
        move |mouse_state| {
            // SAFETY-free: the Hoverable render closure runs synchronously
            // during this render pass, so reborrowing `app` is sound — but to
            // stay in safe Rust we instead capture clones above.
            let _ = app_ptr;
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween);
            let _ = &row;
            let _ = mouse_state;
            unreachable!()
        }
    });
    // The closure-based Hoverable above can't borrow `app`; build the header
    // without hover-dependent action visibility instead. Actions are always
    // rendered (they are subtle icon buttons), matching the panel's narrow
    // width budget.
    drop(hoverable);
    let _ = state_clone;

    let mut full_row = row;
    if !actions_for_render.is_empty() {
        full_row.add_child(render_row_actions(
            actions_for_render,
            state,
            &appearance_for_render,
            app,
        ));
    }
    let full_row = full_row.finish();

    let hoverable = Hoverable::new(state.row.clone(), move |mouse_state| {
        let mut container = Container::new(full_row)
            .with_padding_left(HEADER_LEFT_PADDING)
            .with_padding_right(ROW_RIGHT_PADDING);
        if mouse_state.is_hovered() {
            container = container.with_background(theme.surface_overlay_1());
        }
        container.finish()
    })
    .with_defer_events_to_children()
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(SourceControlViewAction::ToggleSection(section));
    });

    EventHandler::new(
        ConstrainedBox::new(hoverable.finish())
            .with_height(ITEM_HEIGHT)
            .finish(),
    )
    .on_mouse_in(
        move |ctx, _, _| {
            ctx.dispatch_typed_action(SourceControlViewAction::ClearSelectedIndex);
            DispatchEventResult::PropagateToParent
        },
        Some(MouseInBehavior {
            fire_on_synthetic_events: false,
            fire_when_covered: true,
        }),
    )
    .finish()
}

/// Renders a changed-file row: colored status letter, dimmed directory prefix,
/// filename, hover actions.
#[allow(clippy::too_many_arguments)]
pub fn render_file_row(
    change: &FileChange,
    actions: Vec<RowAction>,
    index: usize,
    is_selected: bool,
    state: &ItemState,
    on_click: SourceControlViewAction,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    let letter = Text::new_inline(status_letter(&change.status), font, font_size - 1.)
        .with_color(status_color(&change.status, appearance).into())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish();

    let (dir, file_name) = match change.path.rsplit_once('/') {
        Some((dir, name)) => (Some(format!("{dir}/")), name.to_string()),
        None => (None, change.path.clone()),
    };

    let mut name_row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min);
    if let Some(dir) = dir {
        name_row.add_child(
            Shrinkable::new(
                1.0,
                Text::new_inline(dir, font, font_size)
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
            )
            .finish(),
        );
    }
    name_row.add_child(
        Text::new_inline(file_name, font, font_size)
            .with_color(theme.main_text_color(theme.background()).into())
            .finish(),
    );

    let content = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            ConstrainedBox::new(letter)
                .with_width(STATUS_LETTER_WIDTH)
                .finish(),
        )
        .with_child(Shrinkable::new(1.0, name_row.finish()).finish())
        .finish();

    render_row_base(
        content,
        actions,
        ROW_LEFT_PADDING,
        index,
        is_selected,
        state,
        Some(on_click),
        appearance,
        app,
    )
}

fn render_two_line_content(
    top: String,
    sub: String,
    leading: Option<Box<dyn Element>>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    let lines = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_child(
            Text::new_inline(top, font, font_size)
                .with_color(theme.main_text_color(theme.background()).into())
                .finish(),
        )
        .with_child(
            Text::new_inline(sub, font, font_size - 2.)
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
        )
        .finish();

    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.);
    if let Some(leading) = leading {
        row.add_child(leading);
    }
    row.add_child(Shrinkable::new(1.0, lines).finish());
    row.finish()
}

/// Renders a stash row: message on top, `on <branch> · <age>` subline.
#[allow(clippy::too_many_arguments)]
pub fn render_stash_row(
    stash: &StashEntry,
    actions: Vec<RowAction>,
    index: usize,
    is_selected: bool,
    state: &ItemState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut sub_parts = Vec::new();
    if let Some(branch) = &stash.branch {
        sub_parts.push(format!("on {branch}"));
    }
    if let Some(age) = &stash.age {
        sub_parts.push(age.clone());
    }
    let leading = ConstrainedBox::new(
        Icon::LayersThree01
            .to_warpui_icon(theme.sub_text_color(theme.background()))
            .finish(),
    )
    .with_width(13.)
    .with_height(13.)
    .finish();
    let content = render_two_line_content(
        stash.message.clone(),
        sub_parts.join(" \u{b7} "),
        Some(leading),
        appearance,
    );
    render_row_base(
        content,
        actions,
        ROW_LEFT_PADDING,
        index,
        is_selected,
        state,
        None,
        appearance,
        app,
    )
}

/// Renders a worktree row: name on top, `branch · path` subline, plus a
/// "current" tag on the active worktree.
#[allow(clippy::too_many_arguments)]
pub fn render_worktree_row(
    worktree: &WorktreeEntry,
    actions: Vec<RowAction>,
    index: usize,
    is_selected: bool,
    state: &ItemState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();

    let name = worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| worktree.path.display().to_string());
    let home_dir = dirs::home_dir().and_then(|p| p.to_str().map(String::from));
    let display_path = warp_util::path::user_friendly_path(&worktree.path, home_dir.as_deref());
    let mut sub_parts = Vec::new();
    if let Some(branch) = &worktree.branch {
        sub_parts.push(branch.clone());
    } else {
        sub_parts.push(format!("detached @ {}", short_head(&worktree.head)));
    }
    sub_parts.push(display_path.into_owned());

    let leading = ConstrainedBox::new(
        Icon::Dataflow02
            .to_warpui_icon(theme.sub_text_color(theme.background()))
            .finish(),
    )
    .with_width(13.)
    .with_height(13.)
    .finish();

    let mut content_row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(Shrinkable::new(
            1.0,
            render_two_line_content(
                name,
                sub_parts.join(" \u{b7} "),
                Some(leading),
                appearance,
            ),
        )
        .finish());

    if worktree.is_current {
        content_row.add_child(
            Container::new(
                Text::new_inline("current", font, appearance.ui_font_size() - 3.)
                    .with_color(theme.accent().into_solid())
                    .finish(),
            )
            .with_horizontal_padding(6.)
            .with_background(warp_core::ui::theme::color::internal_colors::accent_overlay_1(
                theme,
            ))
            .with_corner_radius(warpui::elements::CornerRadius::with_all(
                warpui::elements::Radius::Pixels(4.),
            ))
            .finish(),
        );
    }

    render_row_base(
        content_row.finish(),
        actions,
        ROW_LEFT_PADDING,
        index,
        is_selected,
        state,
        None,
        appearance,
        app,
    )
}

fn short_head(head: &str) -> &str {
    if head.len() > 7 {
        &head[..7]
    } else {
        head
    }
}

/// Renders a commit row: unpushed marker, subject + `author · time` sublabel,
/// right-aligned short sha.
#[allow(clippy::too_many_arguments)]
pub fn render_commit_row(
    commit: &CommitEntry,
    index: usize,
    is_selected: bool,
    state: &ItemState,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    let marker: Box<dyn Element> = if commit.is_unpushed {
        Text::new_inline("\u{2191}", font, font_size - 1.)
            .with_color(theme.ansi_fg_green().into())
            .finish()
    } else {
        warpui::elements::Empty::new().finish()
    };

    let lines = render_two_line_content(
        commit.subject.clone(),
        format!("{} \u{b7} {}", commit.author, commit.relative_time),
        None,
        appearance,
    );

    let content = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(ConstrainedBox::new(marker).with_width(10.).finish())
        .with_child(Shrinkable::new(1.0, lines).finish())
        .with_child(
            Text::new_inline(commit.short_sha.clone(), font, font_size - 2.)
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
        )
        .finish();

    render_row_base(
        content,
        Vec::new(),
        ROW_LEFT_PADDING,
        index,
        is_selected,
        state,
        None,
        appearance,
        app,
    )
}

/// Renders a dimmed informational row (e.g. "No stashes").
pub fn render_empty_hint(text: &'static str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    ConstrainedBox::new(
        Container::new(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(
                    Text::new_inline(
                        text,
                        appearance.ui_font_family(),
                        appearance.ui_font_size() - 1.,
                    )
                    .with_color(theme.disabled_ui_text_color().into_solid())
                    .finish(),
                )
                .finish(),
        )
        .with_padding_left(ROW_LEFT_PADDING)
        .finish(),
    )
    .with_height(ITEM_HEIGHT)
    .finish()
}
