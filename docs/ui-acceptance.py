#!/usr/bin/env python3
"""The browser's half of the UI acceptance: every step a user would take in
the page, made as the page makes it.

Driven by `docs/ui-acceptance.sh`, which owns the servers and the engine.
This half speaks the surface's own HTTP and WebSocket: the launch token
exchanged for the cookie, the guarded /api calls with their custom header,
and the attach WebSocket with its Origin and single-use ticket. Nothing here
reaches around the surface — if the page could not do it, this does not do
it either.
"""
import asyncio, json, os, re, sys, time
import urllib.request, urllib.error

import websockets


class Surface:
    """The page, as a script. One cookie, obtained the way the page obtains
    it: the launch token from the URL fragment, exchanged once — and then
    carried, because the token is spent."""

    def __init__(self, launch_url, cookie=None):
        m = re.match(r"(http://127\.0\.0\.1:(\d+))/#token=([0-9a-f]{64})$", launch_url)
        if not m:
            raise SystemExit(f"not a launch URL: {launch_url!r}")
        self.base, self.port, self.token = m.group(1), int(m.group(2)), m.group(3)
        # The launch token is single-use: one page load, one exchange, one
        # cookie — and every later action carries that cookie, exactly as
        # the open tab does. A caller that already has it passes it back.
        self.cookie = cookie

    def _raw(self, method, path, body=None, headers=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method)
        req.add_header("X-Nemr-Request", "1")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if self.cookie:
            req.add_header("Cookie", self.cookie)
        for k, v in (headers or {}).items():
            req.add_header(k, v)
        try:
            with urllib.request.urlopen(req) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def handshake(self):
        if self.cookie:
            return self.cookie
        status, headers, _ = self._raw(
            "POST", "/auth/session", headers={"X-Nemr-Token": self.token}
        )
        if status != 204:
            raise SystemExit(f"the handshake was refused: {status}")
        set_cookie = headers.get("Set-Cookie", "")
        self.cookie = set_cookie.split(";")[0]
        return self.cookie

    def api(self, method, path, body=None):
        status, _, raw = self._raw(method, "/api" + path, body)
        try:
            return status, json.loads(raw or b"null")
        except json.JSONDecodeError:
            return status, raw.decode(errors="replace")

    def job(self, method, path, body=None, timeout=900):
        """Start a job and poll it the way the page polls, returning it done."""
        status, d = self.api(method, path, body)
        if status != 200:
            return {"ok": False, "error": (d or {}).get("error", f"HTTP {status}"), "lines": []}
        deadline = time.time() + timeout
        while time.time() < deadline:
            s, j = self.api("GET", "/jobs/" + d["job"])
            if s != 200:
                return {"ok": False, "error": f"the job vanished ({s})", "lines": []}
            if j["done"]:
                return j
            time.sleep(0.4)
        return {"ok": False, "error": "the job did not finish in time", "lines": []}


async def attach_run(surface, name, command, timeout=600):
    """Attach through the browser's path and run one command in the session.

    Returns (output, exit_code) where `output` is ONLY what the command
    itself wrote — everything the terminal echoed of what we typed is
    excluded, structurally.

    Why that matters: the first form of this helper waited for a marker and
    returned the whole screen, and the screen begins with the shell's echo
    of the command we just typed. A prompt that names the answer it wants
    ("reply with exactly: STORED-BOTH") therefore matched its own echo, and
    the acceptance reported a live conversation for a Claude that never ran.
    An assertion that can be satisfied by its own input is not evidence.

    So the shell brackets the command with sentinels it ASSEMBLES from
    pieces: the line we type contains `${S}${T}-BEGIN`, and only the shell's
    own output ever contains the assembled `NEMRACC<tag>-BEGIN`. The END
    sentinel is printed after the command exits, which also closes the race
    where the capture stopped while the command was still writing.
    """
    status, t = surface.api("POST", f"/sessions/{name}/attach-ticket")
    if status != 200:
        raise SystemExit(f"the attach ticket was refused: {status} {t}")
    tag = "".join(f"{b:02x}" for b in os.urandom(4))
    begin = f"NEMRACC{tag}-BEGIN"
    end = f"NEMRACC{tag}-END"
    wrapped = (
        f'S=NEMR; T=ACC{tag}; echo "${{S}}${{T}}-BEGIN"; {command}; '
        f'rc=$?; echo "${{S}}${{T}}-END $rc"'
    )
    url = f"ws://127.0.0.1:{surface.port}/ws/attach/{name}?ticket={t['ticket']}"
    seen = bytearray()
    async with websockets.connect(
        url,
        additional_headers={"Cookie": surface.cookie, "Origin": surface.base},
        max_size=None,
    ) as ws:
        await ws.send(json.dumps({"type": "start", "rows": 40, "cols": 200}))
        await asyncio.sleep(1.0)
        await ws.send((wrapped + "\n").encode())
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                m = await asyncio.wait_for(ws.recv(), max(1, deadline - time.time()))
            except asyncio.TimeoutError:
                break
            if isinstance(m, bytes):
                seen += m
                # The END sentinel, in the OUTPUT — the echo of the typed
                # line carries the unexpanded `${S}${T}-END`, never this.
                if end.encode() in seen:
                    break
            else:
                seen += f"\n[control frame] {m}\n".encode()
                break
        try:
            await ws.send(b"exit\n")
            await asyncio.wait_for(ws.recv(), 5)
        except Exception:
            pass

    raw = bytes(seen)
    # Flatten for reading: drop escape sequences, and turn a bare carriage
    # return into a newline rather than deleting it — a terminal wraps by
    # emitting CR, and deleting it fuses the two halves into a word that was
    # never on the screen ("two in\rnstructions" -> "innstructions").
    flat = re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]", b"", raw)
    flat = re.sub(rb"\x1b\][^\x07]*\x07", b"", flat)
    flat = flat.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
    text = flat.decode(errors="replace")

    i = text.find(begin)
    j = text.rfind(end)
    if i < 0 or j < 0:
        return raw, text, None, f"the command's sentinels never appeared (begin={i}, end={j})"
    output = text[i + len(begin) : j]
    rc = None
    m = re.match(r"\s+(\d+)", text[j + len(end) :])
    if m:
        rc = int(m.group(1))
    return raw, output, rc, None


def main():
    # NEMR_UI_COOKIE carries the one session cookie between the steps of the
    # acceptance, the way an open tab carries it between clicks.
    op = sys.argv[1]
    surface = Surface(sys.argv[2], os.environ.get("NEMR_UI_COOKIE") or None)
    surface.handshake()
    if op == "register":
        email, password = sys.argv[3], sys.argv[4]
        server = sys.argv[5]
        s, d = surface.api("POST", "/register", {"server": server, "email": email, "password": password})
        if s != 200:
            raise SystemExit(f"register refused: {s} {d}")
        code = d["recovery_code"]
        # Typed back, as the page requires and the CLI requires.
        s, d = surface.api("POST", "/register/confirm", {"code": code})
        if s != 200:
            raise SystemExit(f"the recovery confirmation failed: {s} {d}")
        print(json.dumps({"cookie": surface.cookie, "recovery_code": code, "account": d}))
    elif op == "login":
        email, password, server = sys.argv[3], sys.argv[4], sys.argv[5]
        s, d = surface.api("POST", "/login", {"server": server, "email": email, "password": password})
        if s != 200:
            raise SystemExit(f"login refused: {s} {d}")
        print(json.dumps({"cookie": surface.cookie, "account": d}))
    elif op == "sessions":
        s, d = surface.api("GET", "/sessions")
        if s != 200:
            raise SystemExit(f"the list was refused: {s} {d}")
        print(json.dumps(d))
    elif op == "pull":
        name, password = sys.argv[3], sys.argv[4]
        print(json.dumps(surface.job("POST", f"/sessions/{name}/pull", {"password": password})))
    elif op == "push":
        name, password = sys.argv[3], sys.argv[4]
        release = len(sys.argv) > 5 and sys.argv[5] == "release"
        print(json.dumps(surface.job("POST", f"/sessions/{name}/push", {"password": password, "release": release})))
    elif op == "start":
        print(json.dumps(surface.job("POST", f"/sessions/{sys.argv[3]}/start")))
    elif op == "attach":
        # attach <name> <command> [raw-capture-path]
        name, command = sys.argv[3], sys.argv[4]
        raw, output, rc, err = asyncio.run(attach_run(surface, name, command))
        if len(sys.argv) > 5:
            with open(sys.argv[5], "wb") as f:
                f.write(raw)   # everything the terminal showed, kept (F-132)
        if err:
            sys.stderr.write(err + "\n")
            sys.stdout.write(output if output else "")
            raise SystemExit(2)
        sys.stdout.write(output)
        if rc not in (0, None):
            raise SystemExit(3 if rc else 0)
    else:
        raise SystemExit(f"unknown op {op!r}")


if __name__ == "__main__":
    main()
