//! Telemetry for the Source Control panel. One event with an `action`
//! payload, per the panel plan. Payloads never include file paths, branch
//! names, or commit messages (UGC redaction).

use serde_json::{json, Value};
use strum_macros::{EnumDiscriminants, EnumIter};
use warp_core::features::FeatureFlag;
use warp_core::telemetry::{EnablementState, TelemetryEvent, TelemetryEventDesc};

/// Which panel interaction occurred. Serialized as a snake_case string in the
/// event payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceControlPanelAction {
    Opened,
    Stage,
    Unstage,
    Discard,
    CommitOnly,
    CommitAndPush,
    Amend,
    Pull,
    Sync,
    BranchSwitch,
    BranchCreate,
    BranchDelete,
    StashPush,
    StashApply,
    StashPop,
    StashDrop,
    WorktreeAdd,
    WorktreeRemove,
    WorktreeOpen,
    OpenDiff,
    AiMessage,
}

impl SourceControlPanelAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Stage => "stage",
            Self::Unstage => "unstage",
            Self::Discard => "discard",
            Self::CommitOnly => "commit_only",
            Self::CommitAndPush => "commit_and_push",
            Self::Amend => "amend",
            Self::Pull => "pull",
            Self::Sync => "sync",
            Self::BranchSwitch => "branch_switch",
            Self::BranchCreate => "branch_create",
            Self::BranchDelete => "branch_delete",
            Self::StashPush => "stash_push",
            Self::StashApply => "stash_apply",
            Self::StashPop => "stash_pop",
            Self::StashDrop => "stash_drop",
            Self::WorktreeAdd => "worktree_add",
            Self::WorktreeRemove => "worktree_remove",
            Self::WorktreeOpen => "worktree_open",
            Self::OpenDiff => "open_diff",
            Self::AiMessage => "ai_message",
        }
    }
}

#[derive(Debug, EnumDiscriminants)]
#[strum_discriminants(derive(EnumIter))]
pub enum SourceControlTelemetryEvent {
    PanelAction { action: SourceControlPanelAction },
}

impl TelemetryEvent for SourceControlTelemetryEvent {
    fn name(&self) -> &'static str {
        SourceControlTelemetryEventDiscriminants::from(self).name()
    }

    fn payload(&self) -> Option<Value> {
        match self {
            Self::PanelAction { action } => Some(json!({
                "action": action.as_str(),
            })),
        }
    }

    fn description(&self) -> &'static str {
        SourceControlTelemetryEventDiscriminants::from(self).description()
    }

    fn enablement_state(&self) -> EnablementState {
        SourceControlTelemetryEventDiscriminants::from(self).enablement_state()
    }

    fn contains_ugc(&self) -> bool {
        match self {
            Self::PanelAction { .. } => false,
        }
    }

    fn event_descs() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>> {
        warp_core::telemetry::enum_events::<Self>()
    }
}

impl TelemetryEventDesc for SourceControlTelemetryEventDiscriminants {
    fn name(&self) -> &'static str {
        match self {
            Self::PanelAction => "SourceControl.Panel.Action",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::PanelAction => "User performed an action in the Source Control panel",
        }
    }

    fn enablement_state(&self) -> EnablementState {
        match self {
            Self::PanelAction => EnablementState::Flag(FeatureFlag::SourceControlPanel),
        }
    }
}

warp_core::register_telemetry_event!(SourceControlTelemetryEvent);
