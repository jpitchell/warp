//! `warp --wait` external-editor mode.
//!
//! Lets Warp be used as a blocking editor (`EDITOR='warp --wait'`) by git,
//! Claude Code, etc.: open the given file in Warp's code editor, block the
//! short-lived `warp --wait` process until the user closes that file's editor
//! tab, then exit 0 so the caller re-reads the file.
//!
//! Split into:
//! * [`cli`] — runs in the short-lived `warp --wait` process: builds the URI,
//!   creates the back-channel socket, forwards/spawns, and blocks.
//! * [`registry`] — runs in the GUI app: maps canonical path -> waiters and
//!   notifies them when the file's editor tab closes.
//!
//! The wait address rides inside the `warplocal://action/open_file_editor`
//! URL query string (`&wait=<addr>`), so no platform-specific IPC message type
//! has to change — every platform already forwards URLs to the running app.

use std::path::{Path, PathBuf};

/// A back-channel address the GUI connects to when the watched tab closes.
/// Unix: a Unix-domain-socket path. Windows: a named-pipe path. Opaque string
/// either way; it is passed verbatim in the URL query and to `interprocess`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WaitAddr(pub String);

/// Parse a `<path>[:line[:column]]` argument into `(path, line, column)`.
///
/// Only a trailing numeric `:N` / `:N:M` is treated as line/column; a `:`
/// followed by a non-numeric segment is left as part of the path. The numeric
/// suffix is only stripped from the file-name portion (after the last path
/// separator), so a Windows `C:\…` drive colon is never mistaken for a line.
pub fn split_line_col(arg: &str) -> (String, Option<usize>, Option<usize>) {
    let sep = arg.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let (dir, name) = arg.split_at(sep);
    let parts: Vec<&str> = name.split(':').collect();
    match parts.as_slice() {
        [file, line, col] => {
            if let (Ok(l), Ok(c)) = (line.parse(), col.parse()) {
                return (format!("{dir}{file}"), Some(l), Some(c));
            }
        }
        [file, line] => {
            if let Ok(l) = line.parse() {
                return (format!("{dir}{file}"), Some(l), None);
            }
        }
        _ => {}
    }
    (arg.to_string(), None, None)
}

/// Build the `warplocal://action/open_file_editor?...` URL for a wait request.
/// `scheme` is the channel URL scheme (e.g. `warplocal`).
pub fn build_wait_url(
    scheme: &str,
    canonical_path: &Path,
    line: Option<usize>,
    column: Option<usize>,
    wait: &WaitAddr,
) -> String {
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let mut url = format!(
        "{scheme}://action/open_file_editor?path={}&wait={}",
        enc(&canonical_path.to_string_lossy()),
        enc(&wait.0),
    );
    if let Some(l) = line {
        url.push_str(&format!("&line={l}"));
        if let Some(c) = column {
            url.push_str(&format!("&column={c}"));
        }
    }
    url
}

/// True if `addr` matches the exact shape this build's [`cli::fresh_addr`] would
/// produce: on Unix a `warp-edit-wait-<digits>.sock` file directly inside the
/// system temp dir; on Windows a `\\.\pipe\warp-edit-wait-<digits>` pipe.
///
/// The `wait` back-channel address arrives from an untrusted source — a
/// `warplocal://action/open_file_editor?...&wait=<addr>` URL can be triggered by
/// any party that can hand Warp a custom-scheme link. Without this check the app
/// would `connect()` to an attacker-chosen local socket/pipe and write to it (a
/// local SSRF primitive). Restricting the target to our own naming pattern in the
/// temp dir removes the arbitrary-path capability; legitimate `warp --wait`
/// callers always produce a matching address.
pub fn is_trusted_wait_addr(addr: &str) -> bool {
    fn is_wait_token(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }

    #[cfg(unix)]
    {
        let path = std::path::Path::new(addr);
        let in_temp_dir = path.parent() == Some(std::env::temp_dir().as_path());
        let name_ok = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("warp-edit-wait-"))
            .and_then(|n| n.strip_suffix(".sock"))
            .map(is_wait_token)
            .unwrap_or(false);
        in_temp_dir && name_ok
    }
    #[cfg(windows)]
    {
        addr.strip_prefix(r"\\.\pipe\warp-edit-wait-")
            .map(is_wait_token)
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = addr;
        false
    }
}

/// Canonicalize the raw `--wait` argument (after stripping any `:line:col`).
///
/// Warp's editor only opens files that already exist, so a non-existent path is
/// an error here. Returns `(canonical_path, line, column)`.
pub fn canonicalize_arg(raw: &str) -> std::io::Result<(PathBuf, Option<usize>, Option<usize>)> {
    let (path_str, line, col) = split_line_col(raw);
    let canon = std::fs::canonicalize(&path_str)?;
    Ok((canon, line, col))
}

/// Entry point for `warp --wait <path>`. Opens the file in Warp's code editor
/// and blocks until the user closes that file's tab, then returns so the process
/// exits 0. Returns `Ok(())` (exit 0) in all non-fatal cases — a non-zero exit
/// would make git abort the commit. Exits 2 only if the path argument is invalid.
#[cfg(not(target_family = "wasm"))]
pub fn run_wait_mode(raw_arg: &str) -> anyhow::Result<()> {
    use warp_core::channel::ChannelState;

    let (path, line, col) = match canonicalize_arg(raw_arg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warp --wait: cannot open {raw_arg:?}: {e}");
            std::process::exit(2);
        }
    };

    let addr = cli::fresh_addr();
    let listener = match cli::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("warp --wait: could not create back-channel: {e}; exiting");
            return Ok(()); // exit 0, degrade gracefully
        }
    };

    let scheme = ChannelState::url_scheme();
    let url = build_wait_url(scheme, &path, line, col, &addr);

    if let Err(e) = forward_or_spawn(&url) {
        log::warn!("warp --wait: forward/spawn failed: {e}");
        // Still block; the timeout will free us if nothing ever opens the file.
    }

    cli::block_until_closed(listener, &addr, cli::timeout_from_env());
    Ok(())
}

/// Deliver `url` to a running Warp instance, or cause one to be launched, so the
/// file opens in the GUI. The short-lived `--wait` process never becomes the GUI
/// itself; it only forwards and then blocks on the back-channel.
#[cfg(not(target_family = "wasm"))]
fn forward_or_spawn(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // LaunchServices routes the custom scheme to the running instance or
        // launches a new one. `-g` keeps focus in the terminal, not Warp.
        std::process::Command::new("/usr/bin/open")
            .arg("-g")
            .arg(url)
            .spawn()?
            .wait()?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Spawn a detached `warp <url>` child. If an instance is already running,
        // that child's normal single-instance path forwards the URL and exits;
        // otherwise it becomes the GUI. Either way the file opens and our
        // back-channel is signalled when its tab closes.
        spawn_detached_gui(url)
    }
}

/// Spawn a detached Warp process that opens `url`, mirroring the setsid pattern
/// used elsewhere for detached children. The child parses the URL via `args.urls`.
#[cfg(all(not(target_os = "macos"), not(target_family = "wasm")))]
fn spawn_detached_gui(url: &str) -> anyhow::Result<()> {
    use std::process::Stdio;

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Safety: `setsid` is async-signal-safe and detaches the child into its
        // own session so it survives this process exiting.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    cmd.spawn()?;
    Ok(())
}

/// CLI-side (`warp --wait` process): create the back-channel and block on it.
#[cfg(not(target_family = "wasm"))]
pub mod cli {
    use std::io::Read as _;
    use std::time::Duration;

    use interprocess::local_socket::LocalSocketListener;

    use super::WaitAddr;

    /// Generate a fresh, single-use back-channel address.
    pub fn fresh_addr() -> WaitAddr {
        let id = rand::random::<u64>();
        #[cfg(unix)]
        {
            let p = std::env::temp_dir().join(format!("warp-edit-wait-{id}.sock"));
            WaitAddr(p.to_string_lossy().into_owned())
        }
        #[cfg(windows)]
        {
            WaitAddr(format!(r"\\.\pipe\warp-edit-wait-{id}"))
        }
    }

    /// Bind the listener BEFORE forwarding the URL, so the app can never connect
    /// before we are listening.
    pub fn bind(addr: &WaitAddr) -> std::io::Result<LocalSocketListener> {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&addr.0); // clear any stale socket file
        LocalSocketListener::bind(addr.0.as_str())
    }

    /// Block (synchronously) until the GUI connects and signals tab-close, or
    /// until `timeout` elapses. Always returns; never panics. Uses blocking
    /// `interprocess` sockets since the `--wait` process has no async runtime.
    pub fn block_until_closed(listener: LocalSocketListener, addr: &WaitAddr, timeout: Duration) {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        // `accept()` blocks; run it on a helper thread so we can time out.
        std::thread::spawn(move || {
            let result = listener.accept().and_then(|mut conn| {
                let mut buf = Vec::new();
                // One status byte expected, but EOF alone is treated as success.
                conn.read_to_end(&mut buf)?;
                Ok(buf)
            });
            let _ = tx.send(result);
        });

        match rx.recv_timeout(timeout) {
            Ok(Ok(_bytes)) => { /* normal close — success */ }
            Ok(Err(e)) => log::warn!("edit-wait: back-channel error: {e}"),
            Err(_) => log::warn!("edit-wait: timed out waiting for tab close"),
        }

        #[cfg(unix)]
        let _ = std::fs::remove_file(&addr.0);
        #[cfg(not(unix))]
        let _ = addr;
    }

    /// Overall timeout, overridable via `WARP_EDIT_WAIT_TIMEOUT_SECS`.
    pub fn timeout_from_env() -> Duration {
        std::env::var("WARP_EDIT_WAIT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(24 * 60 * 60))
    }
}

/// App-side registry: maps canonical file path -> waiting `warp --wait`
/// processes, and signals them when the file's editor tab closes.
#[cfg(not(target_family = "wasm"))]
pub mod registry {
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use interprocess::local_socket::LocalSocketStream;
    use warpui::{Entity, SingletonEntity};

    use super::WaitAddr;

    /// Status bytes written back to the waiting CLI.
    pub const STATUS_CLOSED_OK: u8 = 0x00;
    pub const STATUS_NEVER_OPENED: u8 = 0x01;

    /// Singleton mapping canonical file path -> the CLI processes waiting on it.
    #[derive(Default)]
    pub struct EditWaitRegistry {
        waiters: HashMap<PathBuf, Vec<WaitAddr>>,
    }

    impl EditWaitRegistry {
        fn canon(path: &Path) -> PathBuf {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }

        /// Record a new waiter for `path`, keyed by canonical path.
        pub fn register(&mut self, path: &Path, addr: WaitAddr) {
            self.waiters.entry(Self::canon(path)).or_default().push(addr);
        }

        /// A tab for `path` closed: notify and drop ALL waiters on it.
        pub fn notify_closed(&mut self, path: &Path) {
            let key = Self::canon(path);
            if let Some(addrs) = self.waiters.remove(&key) {
                for addr in addrs {
                    Self::signal(&addr, STATUS_CLOSED_OK);
                }
            }
        }

        /// Signal a single addr immediately (e.g. the file could never open).
        pub fn signal_addr(addr: &WaitAddr, status: u8) {
            Self::signal(addr, status);
        }

        fn signal(addr: &WaitAddr, status: u8) {
            // Blocking connect+write of a single byte over a local socket. If the
            // CLI already exited, this errors harmlessly.
            match LocalSocketStream::connect(addr.0.as_str()) {
                Ok(mut stream) => {
                    let _ = stream.write_all(&[status]);
                    let _ = stream.flush();
                    // drop => EOF on the CLI side
                }
                Err(e) => log::debug!("edit-wait: could not signal {}: {e}", addr.0),
            }
        }
    }

    impl Entity for EditWaitRegistry {
        type Event = ();
    }
    impl SingletonEntity for EditWaitRegistry {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_plain_path() {
        assert_eq!(split_line_col("/a/b.txt"), ("/a/b.txt".into(), None, None));
    }

    #[test]
    fn split_line_only() {
        assert_eq!(
            split_line_col("/a/b.txt:42"),
            ("/a/b.txt".into(), Some(42), None)
        );
    }

    #[test]
    fn split_line_and_col() {
        assert_eq!(
            split_line_col("/a/b.txt:42:7"),
            ("/a/b.txt".into(), Some(42), Some(7))
        );
    }

    #[test]
    fn split_does_not_eat_non_numeric() {
        assert_eq!(
            split_line_col("/a/b:c.txt"),
            ("/a/b:c.txt".into(), None, None)
        );
    }

    #[test]
    fn split_does_not_eat_windows_drive() {
        assert_eq!(
            split_line_col(r"C:\code\b.txt"),
            (r"C:\code\b.txt".to_string(), None, None)
        );
    }

    #[test]
    fn build_url_basic() {
        let u = build_wait_url(
            "warplocal",
            Path::new("/a/b.txt"),
            Some(3),
            Some(9),
            &WaitAddr("/tmp/x.sock".into()),
        );
        assert!(u.starts_with("warplocal://action/open_file_editor?path="));
        assert!(u.contains("%2Fa%2Fb.txt"));
        assert!(u.contains("wait="));
        assert!(u.ends_with("&line=3&column=9"));
    }

    #[test]
    fn build_url_no_line_col() {
        let u = build_wait_url(
            "warplocal",
            Path::new("/a/b.txt"),
            None,
            None,
            &WaitAddr("/tmp/x.sock".into()),
        );
        assert!(!u.contains("&line="));
        assert!(!u.contains("&column="));
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn fresh_addr_is_trusted_but_arbitrary_paths_are_not() {
        // Anything our own CLI generates must pass the allowlist...
        let addr = cli::fresh_addr();
        assert!(
            is_trusted_wait_addr(&addr.0),
            "fresh_addr produced an address the allowlist rejects: {}",
            addr.0
        );

        // ...but arbitrary attacker-chosen targets must be rejected.
        assert!(!is_trusted_wait_addr("/run/evil.sock"));
        assert!(!is_trusted_wait_addr("/etc/passwd"));
        assert!(!is_trusted_wait_addr(""));
        #[cfg(unix)]
        {
            let outside = "/tmp/warp-edit-wait-1.sock"; // right name, wrong dir on macOS
            let in_tmp = std::env::temp_dir().join("warp-edit-wait-not-numeric.sock");
            // Non-numeric token is rejected even inside the temp dir.
            assert!(!is_trusted_wait_addr(&in_tmp.to_string_lossy()));
            // On platforms where temp_dir() isn't /tmp, a /tmp path is rejected.
            if std::env::temp_dir() != std::path::Path::new("/tmp") {
                assert!(!is_trusted_wait_addr(outside));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_block_until_closed_releases_on_signal() {
        use std::io::Write as _;
        use std::time::{Duration, Instant};

        let addr = cli::fresh_addr();
        let listener = cli::bind(&addr).unwrap();

        let addr2 = addr.clone();
        let connector = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut s =
                interprocess::local_socket::LocalSocketStream::connect(addr2.0.as_str()).unwrap();
            let _ = s.write_all(&[super::registry::STATUS_CLOSED_OK]);
        });

        let start = Instant::now();
        cli::block_until_closed(listener, &addr, Duration::from_secs(30));
        let elapsed = start.elapsed();
        connector.join().unwrap();

        assert!(
            elapsed < Duration::from_secs(5),
            "should release promptly after the app signals, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_block_until_closed_times_out() {
        use std::time::{Duration, Instant};

        let addr = cli::fresh_addr();
        let listener = cli::bind(&addr).unwrap();

        let start = Instant::now();
        cli::block_until_closed(listener, &addr, Duration::from_millis(800));
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(700),
            "should block until the timeout elapses, took {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn registry_notify_writes_status_byte() {
        use std::io::Read as _;

        use super::registry::{EditWaitRegistry, STATUS_CLOSED_OK};

        let dir = std::env::temp_dir();
        let id = rand::random::<u64>();
        let sock = dir.join(format!("warp-test-{id}.sock"));
        let addr = WaitAddr(sock.to_string_lossy().into_owned());
        let _ = std::fs::remove_file(&sock);
        let listener =
            interprocess::local_socket::LocalSocketListener::bind(addr.0.as_str()).unwrap();

        let file = dir.join(format!("warp-test-{id}.txt"));
        std::fs::write(&file, b"x").unwrap();

        let mut reg = EditWaitRegistry::default();
        reg.register(&file, addr.clone());

        let handle = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            let mut buf = Vec::new();
            conn.read_to_end(&mut buf).unwrap();
            buf
        });

        reg.notify_closed(&file);
        let buf = handle.join().unwrap();
        assert_eq!(buf, vec![STATUS_CLOSED_OK]);

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_file(&file);
    }
}
