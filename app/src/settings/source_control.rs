use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

define_settings_group!(SourceControlSettings, settings: [
    // Controls whether the source control panel appears in the tools panel.
    show_source_control: ShowSourceControl {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "source_control.show_source_control",
        description: "Whether the source control panel is shown in the tools panel.",
    },
    history_commit_limit: HistoryCommitLimit {
        type: usize,
        default: 50,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "source_control.history_commit_limit",
        description: "The maximum number of commits shown in the source control panel's history section.",
    },
]);
