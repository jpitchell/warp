#!/usr/bin/env python3
"""Alt-screen variant of read_keys.py.

First switches the terminal into the alternate screen (CSI ? 1049 h) so that
Warp's model reports `is_alt_screen_active()`, then reads raw bytes from stdin
and prints each as hex into the alt-screen grid. Used by
test_cmd_arrow_line_nav_auto_alt_screen to verify that cmd+left / cmd+right
resolve to Home/End escape sequences when a full-screen program owns the
terminal (the same code path CLI agents like Claude Code take).
"""
import sys
import termios
import tty

# Put terminal in raw mode
old_settings = termios.tcgetattr(sys.stdin)
try:
    tty.setraw(sys.stdin.fileno())

    # Enter the alternate screen so Warp's model sets is_alt_screen_active().
    sys.stdout.write('\x1b[?1049h')
    sys.stdout.flush()

    print("Ready", end='', flush=True)

    bytes_received = []
    while True:
        char = sys.stdin.read(1)
        byte_val = ord(char)
        bytes_received.append(byte_val)

        # Print each byte in hex into the alt-screen grid.
        print(f"\r\n0x{byte_val:02x}", end='', flush=True)

        # Exit on Ctrl+C
        if byte_val == 3:
            break
finally:
    termios.tcsetattr(sys.stdin, termios.TCSADRAIN, old_settings)
    # Leave the alternate screen.
    sys.stdout.write('\x1b[?1049l')
    sys.stdout.flush()
