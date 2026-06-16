use std::env;
use std::fs::read_to_string;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use channel_versions::ChannelVersions;

use crate::channel::ChannelState;
use crate::server::server_api::{ServerApi, FETCH_CHANNEL_VERSIONS_TIMEOUT};

// Fetches channel versions asynchronously from this fork's GitHub Releases manifest.
//
// Upstream Warp first queries its own server (`/client_version`) and falls back to JSON storage.
// This fork serves updates from GitHub Releases instead, so we go straight to the JSON manifest;
// querying `server_root_url` would return upstream Warp's versions, not this fork's. The
// `include_changelogs` and `is_daily` arguments only affected the Warp-server request and are
// therefore unused here.
pub async fn fetch_channel_versions(
    nonce: &str,
    server_api: Arc<ServerApi>,
    _include_changelogs: bool,
    _is_daily: bool,
) -> Result<ChannelVersions> {
    if let Ok(path) = env::var("WARP_CHANNEL_VERSIONS_PATH") {
        // Load channel versions from local filesystem. Used for testing both
        // autoupdate and changelog behavior.
        let path = shellexpand::tilde(&path);
        let channel_versions_string = read_to_string::<&str>(&path)?;
        return serde_json::from_str(channel_versions_string.as_str())
            .context("Failed to parse channel versions JSON");
    }

    fetch_channel_versions_from_json_storage(server_api.http_client(), nonce).await
}

// Fetches updated [`ChannelVersions`] from the releases manifest. For this fork the manifest is
// published as the `channel_versions.json` asset on the `latest` GitHub Release, which GitHub
// serves at `<releases_base_url>/latest/download/channel_versions.json`.
async fn fetch_channel_versions_from_json_storage(
    client: &http_client::Client,
    nonce: &str,
) -> Result<ChannelVersions> {
    log::info!("Fetching channel versions from releases manifest");
    let res = client
        .get(
            format!(
                "{}/latest/download/channel_versions.json?r={}",
                ChannelState::releases_base_url(),
                nonce
            )
            .as_str(),
        )
        .timeout(FETCH_CHANNEL_VERSIONS_TIMEOUT)
        .send()
        .await?;
    let versions: ChannelVersions = res.json().await?;
    log::info!("Received channel versions from GCP JSON storage: {versions}");
    Ok(versions)
}
