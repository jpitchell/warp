//! End-to-end tests for the Source Control left-panel tab: open the panel in
//! a seeded repo, stage a change (via keyboard navigation), commit, and
//! verify the working tree ends up clean.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use command::blocking::Command;
use warp::features::FeatureFlag;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warp::integration_testing::view_getters::{single_terminal_view_for_tab, workspace_view};
use warp::workspace::view::source_control::item::{Section, SourceControlListItem};
use warp::workspace::view::source_control::view::{SourceControlView, SourceControlViewAction};
use warp::workspace::WorkspaceAction;
use warpui_core::integration::TestStep;
use warpui_core::{async_assert, App, ViewHandle, WindowId};

use super::new_builder;
use crate::util::write_all_rc_files_for_test;
use crate::Builder;

const TEST_FILE_NAME: &str = "tracked.txt";
const COMMIT_MESSAGE: &str = "integration test commit";
/// Step-data key: whether `refresh_in_flight` was observed true synchronously
/// right after dispatching `Refresh`.
const REFRESH_WENT_IN_FLIGHT_KEY: &str = "refresh_went_in_flight";

fn run_git(repo_dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .status()
        .expect("git command should run");
    assert!(status.success(), "git {args:?} should succeed");
}

/// The singleton SourceControlView in the window's left panel.
fn source_control_view(app: &App, window_id: WindowId) -> ViewHandle<SourceControlView> {
    let views = app
        .views_of_type::<SourceControlView>(window_id)
        .expect("expected a SourceControlView in the view tree");
    assert_eq!(
        views.len(),
        1,
        "expected exactly one SourceControlView, found {}",
        views.len()
    );
    views.first().unwrap().clone()
}

fn dispatch_source_control_action(
    app: &mut App,
    window_id: WindowId,
    action: SourceControlViewAction,
) {
    let view = source_control_view(app, window_id);
    app.update(|ctx| {
        ctx.dispatch_typed_action_for_view(window_id, view.id(), &action);
    });
}

/// Whether the panel's flat list currently shows `path` as a file row in
/// `section`.
fn panel_shows_file(app: &App, window_id: WindowId, section: Section, path: &str) -> bool {
    let view = source_control_view(app, window_id);
    view.read(app, |view, _ctx| {
        view.integration_list_items().iter().any(|item| match item {
            SourceControlListItem::File {
                section: item_section,
                change,
            } => *item_section == section && change.path == path,
            _ => false,
        })
    })
}

/// The repo directory the active terminal is in (the test seeds the repo as
/// the shell's cwd).
fn active_repo_dir(app: &App, window_id: WindowId) -> PathBuf {
    let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
    let pwd = terminal_view
        .read(app, |terminal_view, _ctx| terminal_view.pwd())
        .expect("terminal should expose a working directory");
    PathBuf::from(pwd)
}

/// Opens the Source Control panel, stages the seeded change with the
/// keyboard (down + space), commits it via the commit box, and asserts the
/// working tree is clean afterwards.
pub fn test_source_control_panel_stage_and_commit() -> Builder {
    FeatureFlag::SourceControlPanel.set_enabled(true);

    new_builder()
        .use_tmp_filesystem_for_test_root_directory()
        .with_setup(|utils| {
            let test_dir = utils.test_dir();
            let repo_dir = test_dir.join("repo");
            fs::create_dir_all(&repo_dir).expect("should create repo subdirectory");
            let repo_dir_string = repo_dir
                .to_str()
                .expect("repo directory should be valid utf-8");

            write_all_rc_files_for_test(&test_dir, format!("cd {repo_dir_string}"));

            fs::write(repo_dir.join(TEST_FILE_NAME), "initial contents\n")
                .expect("should write the tracked file");
            run_git(&repo_dir, &["init", "-b", "main"]);
            run_git(&repo_dir, &["config", "user.email", "test@example.com"]);
            run_git(&repo_dir, &["config", "user.name", "Warp Integration Test"]);
            run_git(&repo_dir, &["config", "commit.gpgsign", "false"]);
            run_git(&repo_dir, &["add", TEST_FILE_NAME]);
            run_git(&repo_dir, &["commit", "-m", "Initial commit"]);

            // Leave an unstaged modification for the panel to show.
            fs::write(repo_dir.join(TEST_FILE_NAME), "modified contents\n")
                .expect("should modify the tracked file");
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(
            TestStep::new("Wait for the terminal to detect the git repository")
                .set_timeout(Duration::from_secs(20))
                .add_named_assertion("repo detected", |app, window_id| {
                    let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
                    terminal_view.read(app, |terminal_view, _ctx| {
                        async_assert!(
                            terminal_view.current_repo_path().is_some(),
                            "expected the active terminal to detect a git repository"
                        )
                    })
                }),
        )
        .with_step(
            TestStep::new("Open the Source Control panel")
                .with_action(|app, window_id, _| {
                    let workspace = workspace_view(app, window_id);
                    app.update(|ctx| {
                        ctx.dispatch_typed_action_for_view(
                            window_id,
                            workspace.id(),
                            &WorkspaceAction::ToggleSourceControlPanel,
                        );
                    });
                })
                .set_timeout(Duration::from_secs(20))
                .add_named_assertion("modified file listed under Changes", |app, window_id| {
                    async_assert!(
                        panel_shows_file(app, window_id, Section::Changes, TEST_FILE_NAME),
                        "expected '{TEST_FILE_NAME}' as an unstaged change in the panel"
                    )
                }),
        )
        .with_step(
            // Dispatching Refresh sets `refresh_in_flight` synchronously and
            // spawns the status reload; the spawned future doesn't run inline,
            // so the flag is observably true right after dispatch and clears
            // once the reload emits `StatusChanged`. This exercises the Refresh
            // button's loading indicator (icon swap + disabled click).
            TestStep::new("Refresh shows an in-flight indicator, then settles")
                .with_action(|app, window_id, step_data| {
                    let view = source_control_view(app, window_id);
                    app.update(|ctx| {
                        ctx.dispatch_typed_action_for_view(
                            window_id,
                            view.id(),
                            &SourceControlViewAction::Refresh,
                        );
                    });
                    let in_flight =
                        view.read(app, |view, _ctx| view.integration_refresh_in_flight());
                    step_data.insert(REFRESH_WENT_IN_FLIGHT_KEY, in_flight);
                })
                .set_timeout(Duration::from_secs(20))
                .add_named_assertion_with_data_from_prior_step(
                    "refresh entered the in-flight state",
                    |_app, _window_id, step_data| {
                        let went_in_flight = step_data
                            .get::<_, bool>(REFRESH_WENT_IN_FLIGHT_KEY)
                            .copied()
                            .unwrap_or(false);
                        async_assert!(
                            went_in_flight,
                            "expected refresh_in_flight to be true immediately after dispatching Refresh"
                        )
                    },
                )
                .add_named_assertion("refresh settled back to idle", |app, window_id| {
                    let view = source_control_view(app, window_id);
                    let in_flight =
                        view.read(app, |view, _ctx| view.integration_refresh_in_flight());
                    async_assert!(
                        !in_flight,
                        "expected refresh_in_flight to clear once the status reload completes"
                    )
                })
                .add_named_assertion("modified file still listed after refresh", |app, window_id| {
                    async_assert!(
                        panel_shows_file(app, window_id, Section::Changes, TEST_FILE_NAME),
                        "expected '{TEST_FILE_NAME}' to remain an unstaged change after refresh"
                    )
                }),
        )
        .with_step(
            TestStep::new("Stage the file with keyboard navigation (down + space)")
                .with_keystrokes(&["down", "space"])
                .set_timeout(Duration::from_secs(20))
                .add_named_assertion("file moved to Staged Changes", |app, window_id| {
                    async_assert!(
                        panel_shows_file(app, window_id, Section::Staged, TEST_FILE_NAME),
                        "expected '{TEST_FILE_NAME}' to be staged after pressing space"
                    )
                }),
        )
        .with_step(
            TestStep::new("Commit the staged change from the commit box")
                .with_action(|app, window_id, _| {
                    let view = source_control_view(app, window_id);
                    app.update(|ctx| {
                        view.update(ctx, |view, ctx| {
                            view.integration_set_commit_message(COMMIT_MESSAGE, ctx);
                        });
                    });
                    dispatch_source_control_action(
                        app,
                        window_id,
                        SourceControlViewAction::Commit,
                    );
                })
                .set_timeout(Duration::from_secs(30))
                .add_named_assertion("panel shows no remaining changes", |app, window_id| {
                    let view = source_control_view(app, window_id);
                    let any_file_rows = view.read(app, |view, _ctx| {
                        view.integration_list_items()
                            .iter()
                            .any(|item| matches!(item, SourceControlListItem::File { .. }))
                    });
                    async_assert!(
                        !any_file_rows,
                        "expected no file rows in the panel after committing"
                    )
                })
                .add_named_assertion("working tree is clean", |app, window_id| {
                    let repo_dir = active_repo_dir(app, window_id);
                    let output = Command::new("git")
                        .args(["status", "--porcelain"])
                        .current_dir(&repo_dir)
                        .output()
                        .expect("git status should run");
                    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    async_assert!(
                        output.status.success() && status.is_empty(),
                        "expected a clean working tree, got: {status}"
                    )
                })
                .add_named_assertion("commit landed with the typed message", |app, window_id| {
                    let repo_dir = active_repo_dir(app, window_id);
                    let output = Command::new("git")
                        .args(["log", "-1", "--format=%s"])
                        .current_dir(&repo_dir)
                        .output()
                        .expect("git log should run");
                    let subject = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    async_assert!(
                        subject == COMMIT_MESSAGE,
                        "expected HEAD subject '{COMMIT_MESSAGE}', got '{subject}'"
                    )
                }),
        )
}
