//! Commit box for the Source Control panel: a multiline message editor with
//! an AI-generate button, plus a Commit split button whose menu offers
//! Commit / Commit & Push / Amend Last Commit.

use warp_core::ui::Icon;
use warpui::elements::{
    ChildAnchor, Container, CornerRadius, CrossAxisAlignment, Element, Flex, MainAxisSize,
    MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
    Stack,
};
use warpui::platform::Cursor;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, ViewContext, ViewHandle};

use super::view::{SourceControlView, SourceControlViewAction};
use crate::appearance::Appearance;
use crate::editor::{EditorOptions, EditorView, PropagateAndNoOpNavigationKeys, TextOptions};
use crate::menu::{Menu, MenuItemFields};
use crate::view_components::action_button::ButtonSize;
use crate::view_components::compactible_action_button::RenderCompactibleActionButton;
use crate::view_components::compactible_split_action_button::CompactibleSplitActionButton;

const EDITOR_FONT_SIZE: f32 = 12.;
const EDITOR_MIN_HEIGHT: f32 = 64.;
const COMMIT_MENU_WIDTH: f32 = 180.;
pub(super) const PLACEHOLDER_TEXT: &str = "Type a commit message";
const GENERATING_PLACEHOLDER_TEXT: &str = "Generating commit message\u{2026}";

/// View-state for the commit box.
pub struct CommitBoxState {
    pub message_editor: ViewHandle<EditorView>,
    pub(super) split_button: CompactibleSplitActionButton,
    pub(super) commit_menu: ViewHandle<Menu<SourceControlViewAction>>,
    pub(super) menu_open: bool,
    /// True while a commit / push / amend chain spawned by the view runs.
    pub(super) committing: bool,
    /// True while an AI commit-message generation request is in flight.
    pub(super) generating: bool,
    generate_button_state: MouseStateHandle,
    pub(super) save_position_id: String,
}

impl CommitBoxState {
    pub fn new(ctx: &mut ViewContext<SourceControlView>) -> Self {
        // Editor configured like the code-review commit dialog's message input.
        let message_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                text: TextOptions {
                    font_size_override: Some(EDITOR_FONT_SIZE),
                    font_family_override: Some(appearance.ui_font_family()),
                    ..Default::default()
                },
                soft_wrap: true,
                autogrow: true,
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                supports_vim_mode: false,
                single_line: false,
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text(PLACEHOLDER_TEXT, ctx);
            editor
        });

        let save_position_id = format!("source_control_commit_button_{}", ctx.view_id());
        let split_button = CompactibleSplitActionButton::new(
            "Commit".to_string(),
            None,
            ButtonSize::Small,
            SourceControlViewAction::Commit,
            SourceControlViewAction::ToggleCommitMenu,
            Icon::GitCommit,
            /* use_primary_theme */ true,
            Some(save_position_id.clone()),
            ctx,
        );

        let commit_menu = ctx.add_typed_action_view(|ctx| {
            let mut menu = Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(COMMIT_MENU_WIDTH);
            menu.set_items(
                vec![
                    MenuItemFields::new("Commit")
                        .with_on_select_action(SourceControlViewAction::Commit)
                        .into_item(),
                    MenuItemFields::new("Commit & Push")
                        .with_on_select_action(SourceControlViewAction::CommitAndPush)
                        .into_item(),
                    MenuItemFields::new("Amend Last Commit")
                        .with_on_select_action(SourceControlViewAction::AmendLastCommit)
                        .into_item(),
                ],
                ctx,
            );
            menu
        });

        Self {
            message_editor,
            split_button,
            commit_menu,
            menu_open: false,
            committing: false,
            generating: false,
            generate_button_state: MouseStateHandle::default(),
            save_position_id,
        }
    }

    /// The trimmed commit message, or `None` when empty.
    pub fn message(&self, app: &AppContext) -> Option<String> {
        let text = self.message_editor.as_ref(app).buffer_text(app);
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    pub(super) fn set_generating(&mut self, generating: bool, ctx: &mut ViewContext<SourceControlView>) {
        self.generating = generating;
        let placeholder = if generating {
            GENERATING_PLACEHOLDER_TEXT
        } else {
            PLACEHOLDER_TEXT
        };
        self.message_editor.update(ctx, |editor, ctx| {
            editor.set_placeholder_text(placeholder, ctx);
        });
        ctx.notify();
    }

    /// Renders the commit box (message editor + ✨ + split button).
    pub fn render(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let theme = appearance.theme();
        let ui_builder = appearance.ui_builder().clone();

        let line_height = self
            .message_editor
            .as_ref(app)
            .line_height(app.font_cache(), appearance);
        let editor_element = ui_builder
            .text_input(self.message_editor.clone())
            .with_style(UiComponentStyles {
                border_color: Some(theme.surface_3().into()),
                border_radius: Some(CornerRadius::with_all(Radius::Pixels(6.))),
                height: Some(EDITOR_MIN_HEIGHT.max(line_height * 3.)),
                ..Default::default()
            })
            .build()
            .finish();

        let generate_tooltip = ui_builder
            .tool_tip(
                if self.generating {
                    "Generating commit message\u{2026}"
                } else {
                    "Generate commit message with AI"
                }
                .to_string(),
            )
            .build();
        let icon_color = theme.sub_text_color(theme.background());
        let mut generate_button = crate::ui_components::buttons::icon_button_with_color(
            appearance,
            Icon::Stars,
            false,
            self.generate_button_state.clone(),
            icon_color,
        )
        .with_tooltip(move || generate_tooltip.finish())
        .build();
        if !self.generating && !self.committing {
            generate_button = generate_button
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(SourceControlViewAction::GenerateCommitMessage);
                })
                .with_cursor(Cursor::PointingHand);
        }

        let mut editor_stack = Stack::new().with_child(editor_element);
        editor_stack.add_positioned_child(
            generate_button.finish(),
            OffsetPositioning::offset_from_parent(
                pathfinder_geometry::vector::vec2f(-4., 4.),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopRight,
                ChildAnchor::TopRight,
            ),
        );

        let button_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(self.split_button.render_expanded_button())
            .finish();

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(editor_stack.finish())
                .with_child(Container::new(button_row).with_margin_top(8.).finish())
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(8.)
        .finish()
    }
}
