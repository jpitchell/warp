# In-app updates from GitHub Releases (fork)

This fork's `dev` channel build pulls its in-app updates from this repository's
GitHub Releases instead of Warp's servers.

## How it is wired

- `app/src/bin/dev.rs` builds its `ChannelConfig` inline (no private
  `warp-channel-config` generator needed) and sets
  `autoupdate_config.releases_base_url` to
  `https://github.com/jpitchell/warp/releases`.
- `app/src/autoupdate/channel_versions.rs` fetches the version manifest from
  `<releases_base_url>/latest/download/channel_versions.json` (the manifest is an
  asset on the GitHub Release tagged/marked **latest**). The upstream
  `/client_version` server call is intentionally skipped — querying
  `server_root_url` would return upstream Warp's versions, not this fork's.
- `app/src/autoupdate/mod.rs` (`release_assets_directory_url`) builds artifact
  URLs as `<releases_base_url>/download/<version>/<file>`, i.e. each app version
  is published as a GitHub Release whose **tag equals the version string**.

The HTTP client follows redirects by default, so GitHub's 302 from
`releases/download/...` to `objects.githubusercontent.com` works transparently.

## Building

```sh
GIT_RELEASE_TAG=v0.2026.06.14.00.00.dev_00 \
  cargo build --bin dev --features autoupdate
# Linux additionally needs APPIMAGE_NAME baked in at compile time, e.g.:
#   APPIMAGE_NAME=WarpDev.AppImage GIT_RELEASE_TAG=... cargo build --bin dev --features autoupdate
```

- `--features autoupdate` turns on the `Autoupdate` feature flag (see
  `app/src/features.rs`).
- `GIT_RELEASE_TAG` is the running build's version; the updater compares it
  against the manifest, so it must use the version format below.
- Do **not** pass `--features release_bundle` (that path expects an embedded
  config from the private generator).

## Version string format

The parser (`crates/channel_versions`) requires:

```
v<major>.<YYYY>.<MM>.<DD>.<HH>.<mm>.<label>_<patch>
```

Example: `v0.2026.06.14.21.30.dev_01`. The date portion must be a valid
`%Y.%m.%d.%H.%M`. The GitHub Release **tag must equal this string exactly**.

## Per-release assets

Tag the Release with the version string and attach the platform artifacts using
these exact names (names are compile-time / channel-prefix derived — `WarpDev`
for the dev channel):

| Platform              | Asset name (dev channel)        |
| --------------------- | ------------------------------- |
| macOS (Apple Silicon) | `WarpDev-arm64.dmg`             |
| macOS (universal)     | `WarpDev.dmg`                   |
| Linux (AppImage)      | value of `APPIMAGE_NAME`        |
| Windows (x64)         | `WarpDevSetup.exe`              |
| Windows (arm64)       | `WarpDevSetup-arm64.exe`        |

Also publish `channel_versions.json` (below) as an asset on the Release marked
**latest**.

> Note: the Linux package-manager update paths (apt/dnf/pacman) are **not**
> redirected — they still reference Warp's repos and shell out to the system
> package manager. Only the AppImage path downloads from `releases_base_url`.

## Sample `channel_versions.json`

The `dev`, `preview`, and `stable` fields are all required by the schema even
though the dev build only reads `dev`.

```json
{
  "dev":     { "version": "v0.2026.06.14.21.30.dev_01" },
  "preview": { "version": "v0.2026.06.14.21.30.dev_01" },
  "stable":  { "version": "v0.2026.06.14.21.30.dev_01" }
}
```

## Publishing via CI

`.github/workflows/fork_release_dev_autoupdate.yml` automates the above on
GitHub-hosted macOS runners (no secrets beyond `GITHUB_TOKEN`). Run it from the
Actions tab (leave `version` blank to auto-generate a correctly-formatted one) or
push a `v0.*.dev_*` tag. It builds the `dev` channel with `--features autoupdate`
for both architectures, generates `channel_versions.json`, and publishes a
GitHub Release (marked **latest**) carrying `WarpDev.dmg`, `WarpDev-arm64.dmg`,
and the manifest. The .dmg is ad-hoc signed and not notarized.

> Linux and Windows release jobs are not included yet — only the AppImage,
> macOS, and Windows-installer download paths are supported by the updater, but
> only macOS is wired into CI here.

## Local testing without a server

Point the updater at a local manifest file to exercise the flow end-to-end:

```sh
WARP_CHANNEL_VERSIONS_PATH=~/channel_versions.json <run the dev build>
```
