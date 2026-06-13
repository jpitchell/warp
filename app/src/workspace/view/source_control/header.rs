//! Repo/branch header for the Source Control panel: the branch picker
//! (worktree-aware), ahead/behind counters, and the sync + refresh buttons.

use warp_core::ui::Icon;
use warpui::elements::{
    ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Flex,
    MainAxisSize, MouseStateHandle, ParentElement, Radius, SavePosition, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::ui_components::button::Button;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{AppContext, ViewContext, ViewHandle};

use super::view::{SourceControlView, SourceControlViewAction};
use crate::appearance::Appearance;
use crate::menu::{Menu, MenuItem, MenuItemFields};
use crate::source_control::{BranchStatus, WorktreeEntry};
use crate::ui_components::buttons::icon_button_with_color;
use crate::util::git::BranchEntry;
use crate::view_components::{DropdownAction, FilterableDropdown};

const BRANCH_MENU_WIDTH: f32 = 260.;
const SYNC_MENU_WIDTH: f32 = 160.;

/// What pressing the sync button would do given the current branch state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncIntent {
    /// No upstream — push with `--set-upstream`.
    Publish,
    /// Behind the upstream — pull only. When also ahead, "Pull, then push"
    /// is offered through the sync menu rather than being the default.
    Pull,
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
            Self::Pull
        } else if branch.ahead > 0 {
            Self::Push
        } else {
            Self::UpToDate
        }
    }

    fn tooltip(&self) -> &'static str {
        match self {
            Self::Publish => "Publish branch",
            Self::Pull => "Pull",
            Self::Push => "Push changes",
            Self::UpToDate => "Up to date",
        }
    }

    fn click_action(&self) -> SourceControlViewAction {
        match self {
            Self::Pull => SourceControlViewAction::Pull,
            _ => SourceControlViewAction::Sync,
        }
    }
}

/// View-state for the header row.
pub struct HeaderState {
    pub(super) branch_dropdown: ViewHandle<FilterableDropdown<SourceControlViewAction>>,
    /// Menu behind the sync chevron ("Pull" / "Pull, then push"), shown only
    /// when the branch is both behind and ahead.
    pub(super) sync_menu: ViewHandle<Menu<SourceControlViewAction>>,
    pub(super) sync_menu_open: bool,
    pub(super) sync_save_position_id: String,
    sync_button_state: MouseStateHandle,
    sync_menu_button_state: MouseStateHandle,
    refresh_button_state: MouseStateHandle,
}

impl HeaderState {
    pub fn new(ctx: &mut ViewContext<SourceControlView>) -> Self {
        let branch_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_menu_width(BRANCH_MENU_WIDTH, ctx);
            // The closed trigger shows the current branch (set via
            // `refresh_branch_items`); this only covers the pre-load state.
            dropdown.set_closed_label_fallback("Loading\u{2026}");
            dropdown.set_use_overlay_layer(true, ctx);
            dropdown
        });

        let sync_menu = ctx.add_typed_action_view(|ctx| {
            let mut menu = Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(SYNC_MENU_WIDTH);
            menu.set_items(
                vec![
                    MenuItemFields::new("Pull")
                        .with_on_select_action(SourceControlViewAction::Pull)
                        .into_item(),
                    MenuItemFields::new("Pull, then push")
                        .with_on_select_action(SourceControlViewAction::Sync)
                        .into_item(),
                ],
                ctx,
            );
            menu
        });

        Self {
            branch_dropdown,
            sync_menu,
            sync_menu_open: false,
            sync_save_position_id: format!("source_control_sync_button_{}", ctx.view_id()),
            sync_button_state: MouseStateHandle::default(),
            sync_menu_button_state: MouseStateHandle::default(),
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
        detached: bool,
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
            // The closed trigger's label comes entirely from the fallback —
            // the current item is deliberately never *selected* in the menu,
            // since selection paints a persistent highlight row that clashes
            // with hover highlighting. The ✓ icon marks the current branch
            // instead.
            let label = if detached {
                "Detached HEAD".to_string()
            } else {
                current_branch.unwrap_or("Select branch").to_string()
            };
            dropdown.set_closed_label_fallback(label);
            // Clears any selection (no item has an empty name).
            dropdown.set_selected_by_name("", ctx);
        });
    }

    /// Renders the header row: branch picker, ahead/behind, sync + refresh.
    pub fn render(
        &self,
        branch: Option<&BranchStatus>,
        busy: bool,
        sync_in_flight: bool,
        refresh_in_flight: bool,
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

        // The sync button carries its own pull/push counts (`↓7 ↑2`) so the
        // numbers are visually bound to the action; it disappears entirely
        // when there's nothing to sync. Publish (no upstream) shows an
        // upload-cloud icon instead, keeping `Refresh` below as the only
        // circular-arrow icon in the header.
        if let Some(branch) = branch {
            let intent = SyncIntent::from_branch(branch);
            if intent != SyncIntent::UpToDate {
                let tooltip = ui_builder.tool_tip(intent.tooltip().to_string()).build();
                // While the sync chain runs, replace the counters/upload-cloud
                // with a loading glyph; `busy` already drops the click handler.
                let mut sync_button = if sync_in_flight {
                    icon_button_with_color(
                        appearance,
                        Icon::Loading,
                        false,
                        self.sync_button_state.clone(),
                        theme.sub_text_color(theme.background()),
                    )
                } else {
                    match intent {
                        SyncIntent::Publish => icon_button_with_color(
                            appearance,
                            Icon::UploadCloud,
                            false,
                            self.sync_button_state.clone(),
                            theme.sub_text_color(theme.background()),
                        ),
                        _ => {
                            // Pull count first to match the action order.
                            let count_font_size = appearance.ui_font_size() - 1.;
                            let mut counters = Flex::row()
                                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                                .with_main_axis_size(MainAxisSize::Min)
                                .with_spacing(4.);
                            if branch.behind > 0 {
                                counters.add_child(
                                    Text::new_inline(
                                        format!("\u{2193}{}", branch.behind),
                                        font,
                                        count_font_size,
                                    )
                                    .with_color(theme.ansi_fg_yellow())
                                    .finish(),
                                );
                            }
                            if branch.ahead > 0 {
                                counters.add_child(
                                    Text::new_inline(
                                        format!("\u{2191}{}", branch.ahead),
                                        font,
                                        count_font_size,
                                    )
                                    .with_color(theme.ansi_fg_green())
                                    .finish(),
                                );
                            }
                            let pill_styles = || {
                                UiComponentStyles::default()
                                    .set_height(22.)
                                    .set_border_width(0.)
                                    .set_padding(Coords {
                                        top: 3.,
                                        bottom: 3.,
                                        left: 6.,
                                        right: 6.,
                                    })
                                    .set_border_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                            };
                            Button::new(
                                self.sync_button_state.clone(),
                                pill_styles(),
                                Some(pill_styles().set_background(theme.surface_2().into())),
                                Some(pill_styles().set_background(theme.surface_3().into())),
                                Some(pill_styles()),
                            )
                            .with_custom_label(counters.finish())
                        }
                    }
                };
                sync_button = sync_button.with_tooltip(move || tooltip.finish());
                let mut sync_button = sync_button.build();
                if !busy {
                    let click_action = intent.click_action();
                    sync_button = sync_button
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(click_action.clone());
                        })
                        .with_cursor(Cursor::PointingHand);
                }
                let mut sync_row = Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_spacing(2.)
                    .with_child(
                        ConstrainedBox::new(sync_button.finish())
                            .with_height(22.)
                            .finish(),
                    );

                // Both behind and ahead: the default click pulls only, so a
                // chevron offers "Pull, then push" as the explicit option.
                if intent == SyncIntent::Pull && branch.ahead > 0 {
                    let menu_tooltip = ui_builder.tool_tip("Sync options".to_string()).build();
                    let mut menu_button = icon_button_with_color(
                        appearance,
                        Icon::ChevronDown,
                        false,
                        self.sync_menu_button_state.clone(),
                        theme.sub_text_color(theme.background()),
                    )
                    .with_tooltip(move || menu_tooltip.finish())
                    .build();
                    if !busy {
                        menu_button = menu_button
                            .on_click(|ctx, _, _| {
                                ctx.dispatch_typed_action(SourceControlViewAction::ToggleSyncMenu);
                            })
                            .with_cursor(Cursor::PointingHand);
                    }
                    sync_row.add_child(
                        ConstrainedBox::new(menu_button.finish())
                            .with_width(16.)
                            .with_height(22.)
                            .finish(),
                    );
                }

                row.add_child(
                    SavePosition::new(sync_row.finish(), &self.sync_save_position_id).finish(),
                );
            }
        }

        let icon_color = theme.sub_text_color(theme.background());
        let refresh_tooltip = ui_builder.tool_tip("Refresh".to_string()).build();
        let refresh_icon = if refresh_in_flight {
            Icon::Loading
        } else {
            Icon::Refresh
        };
        let mut refresh_button = icon_button_with_color(
            appearance,
            refresh_icon,
            false,
            self.refresh_button_state.clone(),
            icon_color,
        )
        .with_tooltip(move || refresh_tooltip.finish())
        .build();
        // Don't re-fire a refresh while one is running, or during any other
        // mutating operation.
        if !busy && !refresh_in_flight {
            refresh_button = refresh_button
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(SourceControlViewAction::Refresh);
                })
                .with_cursor(Cursor::PointingHand);
        }
        let refresh_button = refresh_button.finish();
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
