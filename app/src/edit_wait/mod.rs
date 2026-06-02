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

/// Canonicalize the raw `--wait` argument (after stripping any `:line:col`).
///
/// Warp's editor only opens files that already exist, so a non-existent path is
/// an error here. Returns `(canonical_path, line, column)`.
pub fn canonicalize_arg(raw: &str) -> std::io::Result<(PathBuf, Option<usize>, Option<usize>)> {
    let (path_str, line, col) = split_line_col(raw);
    let canon = std::fs::canonicalize(&path_str)?;
    Ok((canon, line, col))
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
}
