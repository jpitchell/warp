//! Repo/branch header for the Source Control panel: the branch picker
//! (worktree-aware), ahead/behind counters, and the sync + refresh buttons.

use warp_core::ui::Icon;
use warpui::elements::{
    ChildView, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, MainAxisSize,
    MouseStateHandle, ParentElement, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, ViewContext, ViewHandle};

use super::view::{SourceControlView, SourceControlViewAction};
use crate::appearance::Appearance;
use crate::menu::{MenuItem, MenuItemFields};
use crate::source_control::{BranchStatus, WorktreeEntry};
use crate::ui_components::buttons::icon_button_with_color;
use crate::util::git::BranchEntry;
use crate::view_components::{DropdownAction, FilterableDropdown};

const BRANCH_MENU_WIDTH: f32 = 260.;

/// What pressing the sync button would do given the current branch state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncIntent {
    /// No upstream — push with `--set-upstream`.
    Publish,
    /// Behind the upstream — pull (then push if also ahead).
    PullThenPush,
    /// Ahead only — push.
    Push,
    /// Nothing to sync.
    UpToDate,
}

impl SyncIntent {
    pub fn from_branch(branch: &BranchStatus) -> Self {
        if branch.upstream.is_none() {
            Self::Publish
        } else if branch.behind > 0 {
            Self::PullThenPush
        } else if branch.ahead > 0 {
            Self::Push
        } else {
            Self::UpToDate
        }
    }

    fn tooltip(&self) -> &'static str {
        match self {
            Self::Publish => "Publish branch",
            Self::PullThenPush => "Pull, then push",
            Self::Push => "Push changes",
            Self::UpToDate => "Up to date",
        }
    }
}

/// View-state for the header row.
pub struct HeaderState {
    pub(super) branch_dropdown: ViewHandle<FilterableDropdown<SourceControlViewAction>>,
    sync_button_state: MouseStateHandle,
    refresh_button_state: MouseStateHandle,
}

impl HeaderState {
    pub fn new(ctx: &mut ViewContext<SourceControlView>) -> Self {
        let branch_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_menu_width(BRANCH_MENU_WIDTH, ctx);
            dropdown.set_menu_header_to_static("Switch branch");
            dropdown.set_use_overlay_layer(true, ctx);
            dropdown
        });
        Self {
            branch_dropdown,
            sync_button_state: MouseStateHandle::default(),
            refresh_button_state: MouseStateHandle::default(),
        }
    }

    /// Rebuilds the branch picker items from the latest model data. Branches
    /// checked out in another worktree get a badge and, when selected, cd the
    /// current tab to that worktree instead of switching in place.
    pub fn refresh_branch_items(
        &self,
        branches: &[BranchEntry],
        worktrees: &[WorktreeEntry],
        current_branch: Option<&str>,
        ctx: &mut ViewContext<SourceControlView>,
    ) {
        let mut items: Vec<MenuItem<DropdownAction>> = Vec::new();

        for branch in branches {
            let is_current = current_branch == Some(branch.name.as_str());
            let other_worktree = worktrees
                .iter()
                .find(|w| !w.is_current && w.branch.as_deref() == Some(branch.name.as_str()));

            let mut fields = MenuItemFields::new(branch.name.clone());
            if is_current {
                fields = fields
                    .with_icon(Icon::Check)
                    // Selecting the current branch is a no-op (clears the filter).
                    .with_on_select_action(DropdownAction::select_action_and_close(
                        SourceControlViewAction::ClearSelectedIndex,
                    ));
            } else if let Some(worktree) = other_worktree {
                fields = fields
                    .with_icon(Icon::Dataflow02)
                    .with_right_side_label("worktree", warpui::fonts::Properties::default())
                    .with_tooltip("Checked out in another worktree — switches this tab there")
                    .with_on_select_action(DropdownAction::select_action_and_close(
                        SourceControlViewAction::OpenWorktreeForBranch(worktree.path.clone()),
                    ));
            } else {
                fields = fields.with_icon(Icon::GitBranch).with_on_select_action(
                    DropdownAction::select_action_and_close(SourceControlViewAction::SwitchBranch(
                        branch.name.clone(),
                    )),
                );
            }
            items.push(MenuItem::Item(fields));
        }

        items.push(MenuItem::Separator);
        items.push(MenuItem::Item(
            MenuItemFields::new("Create new branch\u{2026}")
                .with_icon(Icon::Plus)
                .with_on_select_action(DropdownAction::select_action_and_close(
                    SourceControlViewAction::OpenCreateBranchDialog,
                )),
        ));

        self.branch_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_rich_items(items, ctx);
            if let Some(current) = current_branch {
                dropdown.set_selected_by_name(current, ctx);
            }
        });
    }

    /// Renders the header row: branch picker, ahead/behind, sync + refresh.
    pub fn render(
        &self,
        branch: Option<&BranchStatus>,
        busy: bool,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let font = appearance.ui_font_family();
        let ui_builder = appearance.ui_builder().clone();

        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(
                Shrinkable::new(1.0, ChildView::new(&self.branch_dropdown).finish()).finish(),
            );

        if let Some(branch) = branch {
            if branch.ahead > 0 || branch.behind > 0 {
                let mut counters = Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_spacing(4.);
                if branch.ahead > 0 {
                    counters.add_child(
                        Text::new_inline(
                            format!("\u{2191}{}", branch.ahead),
                            font,
                            appearance.ui_font_size() - 1.,
                        )
                        .with_color(theme.ansi_fg_green().into())
                        .finish(),
                    );
                }
                if branch.behind > 0 {
                    counters.add_child(
                        Text::new_inline(
                            format!("\u{2193}{}", branch.behind),
                            font,
                            appearance.ui_font_size() - 1.,
                        )
                        .with_color(theme.ansi_fg_yellow().into())
                        .finish(),
                    );
                }
                row.add_child(counters.finish());
            }

            let intent = SyncIntent::from_branch(branch);
            let sync_enabled = !busy && intent != SyncIntent::UpToDate;
            let icon_color = theme.sub_text_color(theme.background());
            let tooltip = ui_builder.tool_tip(intent.tooltip().to_string()).build();
            let mut sync_button = icon_button_with_color(
                appearance,
                Icon::RefreshCcw,
                false,
                self.sync_button_state.clone(),
                icon_color,
            )
            .with_tooltip(move || tooltip.finish())
            .build();
            if sync_enabled {
                sync_button = sync_button
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(SourceControlViewAction::Sync);
                    })
                    .with_cursor(Cursor::PointingHand);
            }
            row.add_child(
                ConstrainedBox::new(sync_button.finish())
                    .with_width(22.)
                    .with_height(22.)
                    .finish(),
            );
        }

        let icon_color = theme.sub_text_color(theme.background());
        let refresh_tooltip = ui_builder.tool_tip("Refresh".to_string()).build();
        let refresh_button = icon_button_with_color(
            appearance,
            Icon::Refresh,
            false,
            self.refresh_button_state.clone(),
            icon_color,
        )
        .with_tooltip(move || refresh_tooltip.finish())
        .build()
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(SourceControlViewAction::Refresh);
        })
        .with_cursor(Cursor::PointingHand)
        .finish();
        row.add_child(
            ConstrainedBox::new(refresh_button)
                .with_width(22.)
                .with_height(22.)
                .finish(),
        );

        let _ = app;
        Container::new(row.finish())
            .with_padding_left(12.)
            .with_padding_right(8.)
            .with_vertical_padding(6.)
            .finish()
    }
}
