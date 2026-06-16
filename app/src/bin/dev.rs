use anyhow::Result;
use warp_core::channel::{
    AutoupdateConfig, Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig,
};
use warp_core::{features, AppId};

// Simple wrapper around warp::run() for dev channel builds.
//
// Unlike the first-party channels, this fork constructs its `ChannelConfig` inline rather than
// relying on the private `warp-channel-config` generator. Autoupdate is pointed at this fork's
// GitHub Releases (see `autoupdate::release_assets_directory_url` and
// `autoupdate::channel_versions` for how these URLs are used).
fn main() -> Result<()> {
    let config = ChannelConfig {
        app_id: AppId::new("dev", "warp", "WarpDev"),
        logfile_name: "warp-dev.log".into(),
        server_config: WarpServerConfig::production(),
        oz_config: OzConfig::production(),
        telemetry_config: None,
        autoupdate_config: Some(AutoupdateConfig {
            releases_base_url: "https://github.com/jpitchell/warp/releases".into(),
            show_autoupdate_menu_items: true,
        }),
        crash_reporting_config: None,
        mcp_static_config: None,
    };

    ChannelState::set(
        ChannelState::new(Channel::Dev, config)
            .with_additional_features(features::DEBUG_FLAGS)
            .with_additional_features(features::DOGFOOD_FLAGS)
            .with_additional_features(features::PREVIEW_FLAGS),
    );

    warp::run()
}
