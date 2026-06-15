#!/usr/bin/env bash
# warp-emit-cwd.sh — installed by Warp into ~/.claude/hooks/.
#
# Wired to Claude Code's `CwdChanged` hook. When Claude Code changes its working
# directory (e.g. via `cd`), this emits an OSC 7 escape sequence to the terminal
# so Warp's working-directory / git-branch cards update mid-session, even though
# the outer shell is blocked running `claude`.
#
# Claude Code's Bash tool and hooks run detached from the controlling terminal,
# so we cannot rely on /dev/tty. Instead we discover the pty by walking up the
# process tree to the `claude`/shell process that owns one, then write there.

input=$(cat)

# Extract the new working directory from the hook's JSON payload. Prefer jq, then
# python3, then a best-effort sed fallback so the hook has no hard dependency.
if command -v jq >/dev/null 2>&1; then
  cwd=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
elif command -v python3 >/dev/null 2>&1; then
  cwd=$(printf '%s' "$input" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("cwd",""))' 2>/dev/null)
else
  cwd=$(printf '%s' "$input" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
fi
[ -z "$cwd" ] && exit 0

# Warp percent-decodes the OSC 7 path, but only transforms `%XX` escapes and passes
# every other byte through unchanged. So escaping a literal `%` as `%25` is enough to
# round-trip any path (e.g. a directory named "100%done" or "a%2Fb"); other bytes such
# as spaces or UTF-8 need no encoding here.
cwd=${cwd//%/%25}

# Resolve the terminal device: the controlling tty if we have one, otherwise the
# tty of the nearest ancestor process that owns one.
dev=/dev/tty
if ! { : > "$dev"; } 2>/dev/null; then
  dev=""
  pid=$PPID
  for _ in 1 2 3 4 5 6 7 8; do
    t=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d ' ')
    case "$t" in
      ttys*|pts*|tty*) dev="/dev/$t"; break ;;
    esac
    pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
    { [ -z "$pid" ] || [ "$pid" = "0" ] || [ "$pid" = "1" ]; } && break
  done
fi
[ -z "$dev" ] && exit 0

# OSC 7: file://<hostname>/<path>. Warp requires the hostname to match the local
# machine's hostname (gethostname()), which `hostname` reports.
printf '\033]7;file://%s%s\007' "$(hostname)" "$cwd" > "$dev" 2>/dev/null || true
exit 0
