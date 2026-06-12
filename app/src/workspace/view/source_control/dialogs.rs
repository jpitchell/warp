//! Dialogs for the Source Control panel: create-branch, stash-message,
//! add-worktree, and the destructive-action confirmations (discard /
//! discard-all / remove-worktree). All are rendered as a centered overlay
//! inside the panel using the shared [`Dialog`] component.

use std::path::PathBuf;

use warpui::elements::{ChildView, Container, CrossAxisAlignment, Element, Flex, ParentElement};
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, ViewContext, ViewHandle};

use super::item::Section;
use super::view::{SourceControlView, SourceControlViewAction};
use crate::appearance::Appearance;
use crate::menu::{MenuItem, MenuItemFields};
use crate::source_control::FileChange;
use crate::ui_components::dialog::{dialog_styles, Dialog};
use crate::util::git::BranchEntry;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, DangerPrimaryTheme, NakedTheme, PrimaryTheme,
};
use crate::view_components::{DropdownAction, FilterableDropdown, SubmittableTextInput};

const DIALOG_WIDTH: f32 = 360.;

/// Which dialog is currently open.
#[derive(Clone, Debug)]
pub enum ActiveDialog {
    CreateBranch,
    StashPush,
    AddWorktree,
    ConfirmDiscardFile {
        section: Section,
        change: FileChange,
    },
    ConfirmDiscardAll,
    ConfirmRemoveWorktree {
        path: PathBuf,
    },
}

/// View-state for the panel's dialogs. The inputs / buttons are persistent
/// child views re-configured each time a dialog opens.
pub struct DialogState {
    pub(super) active: Option<ActiveDialog>,
    /// Branch name (create-branch / add-worktree) or stash message.
    pub(super) name_input: ViewHandle<SubmittableTextInput>,
    /// Worktree path (add-worktree only).
    pub(super) path_input: ViewHandle<SubmittableTextInput>,
    /// Existing-branch picker for add-worktree.
    pub(super) branch_dropdown: ViewHandle<FilterableDropdown<SourceControlViewAction>>,
    /// The branch picked in `branch_dropdown` (used when `name_input` is empty).
    pub(super) selected_worktree_branch: Option<String>,
    cancel_button: ViewHandle<ActionButton>,
    confirm_button: ViewHandle<ActionButton>,
    alt_button: ViewHandle<ActionButton>,
    danger_button: ViewHandle<ActionButton>,
}

impl DialogState {
    pub fn new(ctx: &mut ViewContext<SourceControlView>) -> Self {
        let name_input = ctx.add_typed_action_view(SubmittableTextInput::new);
        let path_input = ctx.add_typed_action_view(SubmittableTextInput::new);
        let branch_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_menu_header_to_static("Existing branch");
            dropdown.set_use_overlay_layer(true, ctx);
            dropdown
        });

        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(SourceControlViewAction::DialogCancel);
                })
        });
        let confirm_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Confirm", PrimaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(SourceControlViewAction::DialogConfirm);
                })
        });
        let alt_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Stash Staged", PrimaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(SourceControlViewAction::DialogConfirmAlt);
                })
        });
        let danger_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Discard", DangerPrimaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(SourceControlViewAction::DialogConfirm);
                })
        });

        Self {
            active: None,
            name_input,
            path_input,
            branch_dropdown,
            selected_worktree_branch: None,
            cancel_button,
            confirm_button,
            alt_button,
            danger_button,
        }
    }

    /// Opens `dialog`, resetting and configuring the inputs / buttons.
    pub fn open(
        &mut self,
        dialog: ActiveDialog,
        branches: &[BranchEntry],
        ctx: &mut ViewContext<SourceControlView>,
    ) {
        self.selected_worktree_branch = None;
        let clear = |input: &ViewHandle<SubmittableTextInput>,
                     placeholder: &str,
                     ctx: &mut ViewContext<SourceControlView>| {
            input.update(ctx, |input, ctx| {
                input.set_placeholder_text(placeholder, ctx);
            });
            let editor = input.as_ref(ctx).editor().clone();
            editor.update(ctx, |editor, ctx| {
                editor.system_reset_buffer_text("", ctx);
            });
        };

        match &dialog {
            ActiveDialog::CreateBranch => {
                clear(&self.name_input, "Branch name", ctx);
                self.confirm_button
                    .update(ctx, |b, ctx| b.set_label("Create", ctx));
            }
            ActiveDialog::StashPush => {
                clear(&self.name_input, "Stash message (optional)", ctx);
                self.confirm_button
                    .update(ctx, |b, ctx| b.set_label("Stash All", ctx));
            }
            ActiveDialog::AddWorktree => {
                clear(&self.name_input, "New branch name (optional)", ctx);
                clear(&self.path_input, "Worktree path (default suggested)", ctx);
                self.confirm_button
                    .update(ctx, |b, ctx| b.set_label("Add", ctx));
                let items: Vec<MenuItem<DropdownAction>> = branches
                    .iter()
                    .map(|branch| {
                        MenuItem::Item(
                            MenuItemFields::new(branch.name.clone()).with_on_select_action(
                                DropdownAction::select_action_and_close(
                                    SourceControlViewAction::SelectWorktreeBranch(
                                        branch.name.clone(),
                                    ),
                                ),
                            ),
                        )
                    })
                    .collect();
                self.branch_dropdown.update(ctx, |dropdown, ctx| {
                    dropdown.set_rich_items(items, ctx);
                });
            }
            ActiveDialog::ConfirmDiscardFile { .. } | ActiveDialog::ConfirmDiscardAll => {
                self.danger_button
                    .update(ctx, |b, ctx| b.set_label("Discard", ctx));
            }
            ActiveDialog::ConfirmRemoveWorktree { .. } => {
                self.danger_button
                    .update(ctx, |b, ctx| b.set_label("Remove", ctx));
            }
        }

        let focus_name_input = matches!(
            dialog,
            ActiveDialog::CreateBranch | ActiveDialog::StashPush | ActiveDialog::AddWorktree
        );
        self.active = Some(dialog);
        if focus_name_input {
            ctx.focus(&self.name_input);
        } else {
            ctx.focus_self();
        }
        ctx.notify();
    }

    pub fn close(&mut self, ctx: &mut ViewContext<SourceControlView>) {
        self.active = None;
        self.selected_worktree_branch = None;
        ctx.notify();
    }

    /// The trimmed contents of `name_input`, or `None` when empty.
    pub fn name_text(&self, app: &AppContext) -> Option<String> {
        let text = self
            .name_input
            .as_ref(app)
            .editor()
            .as_ref(app)
            .buffer_text(app);
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// The trimmed contents of `path_input`, or `None` when empty.
    pub fn path_text(&self, app: &AppContext) -> Option<String> {
        let text = self
            .path_input
            .as_ref(app)
            .editor()
            .as_ref(app)
            .buffer_text(app);
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// Renders the active dialog, if any.
    pub fn render(&self, appearance: &Appearance, app: &AppContext) -> Option<Box<dyn Element>> {
        let active = self.active.as_ref()?;
        let _ = app;

        let cancel = Container::new(ChildView::new(&self.cancel_button).finish())
            .with_margin_right(8.)
            .finish();

        let dialog = match active {
            ActiveDialog::CreateBranch => Dialog::new(
                "Create new branch".to_string(),
                Some("The new branch is created from the current HEAD and checked out.".into()),
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_child(ChildView::new(&self.name_input).finish())
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(ChildView::new(&self.confirm_button).finish()),
            ActiveDialog::StashPush => Dialog::new(
                "Stash changes".to_string(),
                None,
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_child(ChildView::new(&self.name_input).finish())
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(
                Container::new(ChildView::new(&self.alt_button).finish())
                    .with_margin_right(8.)
                    .finish(),
            )
            .with_bottom_row_child(ChildView::new(&self.confirm_button).finish()),
            ActiveDialog::AddWorktree => Dialog::new(
                "Add worktree".to_string(),
                Some("Pick an existing branch or type a new branch name.".into()),
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_child(
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(ChildView::new(&self.branch_dropdown).finish())
                    .with_child(ChildView::new(&self.name_input).finish())
                    .with_child(ChildView::new(&self.path_input).finish())
                    .finish(),
            )
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(ChildView::new(&self.confirm_button).finish()),
            ActiveDialog::ConfirmDiscardFile { change, .. } => Dialog::new(
                "Discard changes?".to_string(),
                Some(format!(
                    "Changes to '{}' will be permanently lost. This cannot be undone.",
                    change.path
                )),
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(ChildView::new(&self.danger_button).finish()),
            ActiveDialog::ConfirmDiscardAll => Dialog::new(
                "Discard all changes?".to_string(),
                Some(
                    "All unstaged changes will be permanently lost. This cannot be undone."
                        .to_string(),
                ),
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(ChildView::new(&self.danger_button).finish()),
            ActiveDialog::ConfirmRemoveWorktree { path } => Dialog::new(
                "Remove worktree?".to_string(),
                Some(format!(
                    "The worktree at '{}' will be removed.",
                    path.display()
                )),
                UiComponentStyles {
                    width: Some(DIALOG_WIDTH),
                    ..dialog_styles(appearance)
                },
            )
            .with_bottom_row_child(cancel)
            .with_bottom_row_child(ChildView::new(&self.danger_button).finish()),
        };

        Some(dialog.build().finish())
    }
}
