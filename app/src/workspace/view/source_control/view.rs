use warpui::elements::{
    CrossAxisAlignment, Element, Flex, MainAxisAlignment, MainAxisSize, ParentElement, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::appearance::Appearance;

/// Actions handled by the source control view. Empty until the panel gains
/// interactive functionality (M1+).
#[derive(Clone, Debug)]
pub enum SourceControlViewAction {}

/// The Source Control left-panel tab. Currently a static placeholder; the
/// interactive panel (status sections, commit box, stashes, worktrees,
/// history) lands in later milestones.
pub struct SourceControlView {}

impl SourceControlView {
    pub fn init(_app: &mut AppContext) {
        // No fixed bindings yet. This mirrors the other left-panel views'
        // registration entry points so future keyboard-navigation bindings
        // (scoped to `id!(SourceControlView::ui_name())`) have a home.
    }

    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {}
    }

    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
    }
}

impl Entity for SourceControlView {
    type Event = ();
}

impl TypedActionView for SourceControlView {
    type Action = SourceControlViewAction;

    fn handle_action(&mut self, action: &Self::Action, _ctx: &mut ViewContext<Self>) {
        match *action {}
    }
}

impl View for SourceControlView {
    fn ui_name() -> &'static str {
        "SourceControlView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let title_and_subtitle = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(
                Text::new("Source Control", appearance.ui_font_family(), 14.)
                    .with_color(theme.main_text_color(theme.background()).into_solid())
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .finish(),
            )
            .with_child(
                Text::new("Coming soon", appearance.ui_font_family(), 14.)
                    .with_color(theme.disabled_ui_text_color().into_solid())
                    .finish(),
            )
            .finish();

        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_child(
                Flex::column()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::Center)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(title_and_subtitle)
                    .finish(),
            )
            .finish()
    }
}
