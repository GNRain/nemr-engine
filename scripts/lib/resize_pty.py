#!/usr/bin/env python3
"""TEST-ONLY. Run a command on a real pty and resize the pty while it runs.

    ./scripts/lib/resize_pty.py --out FILE --cols N --rows N \
        --at SECONDS:COLSxROWS [--at ...] -- command [args...]

`script(1)` gives a pty but never changes its size, so nothing in this
repository could reproduce what a person does every day: drag the window while
something is running. This does. It opens a pty, makes it the child's
controlling terminal (so SIGWINCH is actually delivered), runs the command,
and calls TIOCSWINSZ at the times given.

It also TRACKS THE SCREEN as it goes, through the same renderer the assertions
use, so the cursor-position query (ESC[6n) is answered with the row the cursor
is really on. A canned answer would let a program that re-anchors itself with
that query pass a test it should fail — which is exactly what happened here
before this was added.

It writes the transcript to --out and, beside it, `<out>.resizes` — one
`BYTEOFFSET COLS ROWS` line per resize, so a renderer can replay the session
and apply each resize at the exact point in the byte stream where it happened.

Exit status is the command's own.
"""
import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import importlib.util

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "render_pty", os.path.join(_HERE, "render_pty.py"))
render_pty = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(render_pty)


def set_size(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--cols", type=int, default=100)
    ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--at", action="append", default=[],
                    help="SECONDS:COLSxROWS — resize at that many seconds in")
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--reflow", default="wrap",
                    help="how this pty rewraps what is on screen when it is resized; "
                         "it decides where the cursor ends up, and so what ESC[6n is told")
    ap.add_argument("--dsr-row", type=int, default=0,
                    help="answer the cursor-position query (ESC[6n) with this row; "
                         "0 (the default) answers with the row the cursor is really on; "
                         "-1 never answers, which is the terminal that will not say")
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd
    if not cmd:
        sys.exit("no command given")

    events = []
    for spec in a.at:
        when, size = spec.split(":", 1)
        cols, rows = size.lower().split("x", 1)
        events.append((float(when), int(cols), int(rows)))
    events.sort()

    master, slave = pty.openpty()
    set_size(master, a.cols, a.rows)

    pid = os.fork()
    if pid == 0:                                  # child
        os.close(master)
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)  # the pty IS our terminal
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        os.dup2(slave, 2)
        if slave > 2:
            os.close(slave)
        os.execvp(cmd[0], cmd)
        os._exit(127)

    os.close(slave)
    out = bytearray()
    applied = []
    cur_rows = [a.rows]
    answered_before = [-1]
    screen = render_pty.Screen(a.rows, a.cols)
    fed = [0]

    def catch_up():
        """Feed the screen everything the child has produced since last time."""
        if len(out) > fed[0]:
            screen.feed(bytes(out[fed[0]:]).decode("utf-8", "replace"))
            fed[0] = len(out)
    started = time.time()
    status = None
    while True:
        now = time.time() - started
        while events and events[0][0] <= now:
            _, cols, rows = events.pop(0)
            catch_up()
            set_size(master, cols, rows)
            screen.resize(cols, rows, a.reflow)
            cur_rows[0] = rows
            # Recorded at the byte offset the child had produced by now: a
            # renderer replaying this stream resizes at exactly this point.
            applied.append((len(out), cols, rows))
        timeout = 0.05 if events else 0.25
        try:
            r, _, _ = select.select([master], [], [], timeout)
        except InterruptedError:
            continue
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                break
            out += chunk
            # Answer the cursor-position query the way a terminal does. Without
            # this the region cannot learn where it is, takes the
            # draw-in-place path, and never records an anchor — so the resize
            # handling under test would never run.
            while a.dsr_row >= 0 and b"\x1b[6n" in out[-len(chunk) - 8:]:
                idx = out.rfind(b"\x1b[6n")
                if idx <= answered_before[0]:
                    break
                answered_before[0] = idx
                catch_up()
                row = a.dsr_row if a.dsr_row > 0 else screen.r + 1
                os.write(master, b"\x1b[%d;%dR" % (row, screen.c + 1))
        done, st = os.waitpid(pid, os.WNOHANG)
        if done:
            status = st
            # Drain whatever is still buffered.
            while True:
                r, _, _ = select.select([master], [], [], 0.2)
                if not r:
                    break
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    chunk = b""
                if not chunk:
                    break
                out += chunk
            break
        if time.time() - started > a.timeout:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            status = 1 << 8
            break

    os.close(master)
    with open(a.out, "wb") as f:
        f.write(bytes(out))
    with open(a.out + ".resizes", "w") as f:
        for off, cols, rows in applied:
            f.write(f"{off} {cols} {rows}\n")
    if status is None:
        _, status = os.waitpid(pid, 0)
    sys.exit(os.waitstatus_to_exitcode(status) if status is not None else 0)


if __name__ == "__main__":
    main()
