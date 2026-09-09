#!/usr/bin/env python3
"""The browser's half of the UI acceptance: every step a user would take in
the page, made as the page makes it.

Driven by `docs/ui-acceptance.sh`, which owns the servers and the engine.
This half speaks the surface's own HTTP and WebSocket: the launch token
exchanged for the cookie, the guarded /api calls with their custom header,
and the attach WebSocket with its Origin and single-use ticket. Nothing here
reaches around the surface — if the page could not do it, this does not do
it either.

The `Browser` class goes one step further and drives the REAL page in a
real browser: Firefox, headless, over WebDriver BiDi (a WebSocket JSON
protocol), with nothing but the `websockets` module this file already needs.
That is how claims about what the page SHOWS — a form gone after login, a
panel closed on shell exit, one action panel at a time — are made against
the DOM the user sees, not against the API underneath it.
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


class Browser:
    """Headless Firefox driven over WebDriver BiDi.

    Firefox (the snap on Ubuntu, or any 129+) prints "WebDriver BiDi
    listening on ws://…" when started with --remote-debugging-port; the
    session endpoint is /session. The profile lives where the snap can
    reach it ($HOME/snap/firefox/common, or $HOME/.cache for a non-snap
    Firefox) — a profile under /tmp is invisible to the snap, and Firefox
    then falls back to the user's own profile and refuses because it is
    in use.
    """

    def __init__(self, keep_profile_under=None):
        home = os.path.expanduser("~")
        base = keep_profile_under or (
            os.path.join(home, "snap", "firefox", "common")
            if os.path.isdir(os.path.join(home, "snap", "firefox"))
            else os.path.join(home, ".cache")
        )
        self.profile = os.path.join(base, f"nemr-ui-acceptance-{os.getpid()}")
        os.makedirs(self.profile, exist_ok=True)
        self.proc = None
        self.ws = None
        self.ctx = None
        self._n = 0
        self.log = []

    async def __aenter__(self):
        try:
            return await self._start()
        except BaseException:
            # A browser that never announced itself, or a session that could
            # not be opened, must not outlive the failure — nor leave its
            # profile behind for the next run to trip on.
            await self.__aexit__(None, None, None)
            raise

    async def _start(self):
        import subprocess, shutil, threading, queue
        exe = shutil.which("firefox")
        if not exe:
            raise SystemExit("no firefox on PATH: the page-driving half of the acceptance needs a browser")
        env = dict(os.environ, MOZ_HEADLESS="1")
        self.proc = subprocess.Popen(
            [exe, "--headless", "--no-remote", "--profile", self.profile,
             "--remote-debugging-port", "0", "about:blank"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        # The browser's output is read on a thread into a queue, so the
        # deadline below is a real deadline: a readline that never returns
        # cannot hold it.
        lines = queue.Queue()
        def pump():
            # readline, not iteration: iterating a pipe read-buffers whole
            # chunks and can hold the one line we wait for past the deadline.
            for l in iter(self.proc.stdout.readline, ""):
                lines.put(l.rstrip())
            lines.put(None)
        threading.Thread(target=pump, daemon=True).start()
        url = None
        # Generous: a cold snap start on a small VM with its page cache just
        # evicted by a build was measured at 93 s. The deadline is there so a
        # browser that never comes up fails instead of hanging, not to be tight.
        deadline = time.time() + 180
        while url is None:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise SystemExit("firefox did not announce a BiDi endpoint within 180 s:\n" + "\n".join(self.log[-10:]))
            try:
                line = lines.get(timeout=remaining)
            except queue.Empty:
                continue
            if line is None:
                raise SystemExit("firefox exited before announcing a BiDi endpoint:\n" + "\n".join(self.log[-10:]))
            self.log.append(line)
            m = re.search(r"WebDriver BiDi listening on (ws://\S+)", line)
            if m:
                url = m.group(1)
        self.ws = await asyncio.wait_for(websockets.connect(url.rstrip("/") + "/session", max_size=None), 30)
        await self.cmd("session.new", {"capabilities": {}})
        tree = await self.cmd("browsingContext.getTree", {})
        self.ctx = tree["contexts"][0]["context"]
        return self

    async def __aexit__(self, *exc):
        try:
            if self.ws and self.ctx:
                try:
                    await asyncio.wait_for(self.cmd("session.end", {}), 5)
                except Exception:
                    pass
                await self.ws.close()
        finally:
            if self.proc:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=10)
                except Exception:
                    self.proc.kill()
            import shutil
            shutil.rmtree(self.profile, ignore_errors=True)

    async def cmd(self, method, params):
        self._n += 1
        n = self._n
        await self.ws.send(json.dumps({"id": n, "method": method, "params": params}))
        while True:
            r = json.loads(await self.ws.recv())
            if r.get("id") == n:
                if r.get("type") == "error":
                    raise RuntimeError(f"{method}: {r.get('error')}: {r.get('message')}")
                return r["result"]

    async def goto(self, url):
        await self.cmd("browsingContext.navigate", {"context": self.ctx, "url": url, "wait": "complete"})

    async def eval(self, expression):
        """Evaluate JS in the page (promises awaited) and return its value."""
        r = await self.cmd("script.evaluate", {
            "expression": expression, "target": {"context": self.ctx},
            "awaitPromise": True, "resultOwnership": "none",
        })
        if r.get("type") == "exception":
            raise RuntimeError(f"page threw: {r.get('exceptionDetails', {}).get('text')}")
        return _bidi_value(r["result"])

    async def screenshot(self, path):
        """Capture the page as a PNG (F-13: one shot from the headless run for
        the PR, so the design that shipped can be seen before it is run)."""
        r = await self.cmd("browsingContext.captureScreenshot", {"context": self.ctx})
        import base64
        with open(path, "wb") as f:
            f.write(base64.b64decode(r["data"]))
        return path

    async def wait_for(self, expression, timeout=30, what=None):
        """Poll a JS expression until it is truthy, the way a user waits."""
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            last = await self.eval(expression)
            if last:
                return last
            await asyncio.sleep(0.2)
        raise SystemExit(f"timed out waiting for {what or expression!r} (last value {last!r})")


def _bidi_value(v):
    """Flatten BiDi's typed remote values into plain Python."""
    t = v.get("type")
    if t in ("string", "number", "boolean"):
        return v.get("value")
    if t in ("null", "undefined"):
        return None
    if t == "array":
        return [_bidi_value(x) for x in v.get("value", [])]
    if t == "object":
        return {(_bidi_value(k) if isinstance(k, dict) else k): _bidi_value(val) for k, val in v.get("value", [])}
    return v.get("value")


# --- the page, driven: the claims about what the page SHOWS -----------------

VISIBLE = "(id => { const e = document.getElementById(id); return !!e && e.checkVisibility(); })"
# The terminal's screen as text (xterm's buffer, blank rows dropped) — for evidence when a wait fails.
SCREEN_TEXT = ("(() => { const term = window.nemrTerm; if (!term) return ''; const b = term.buffer.active; const o = []; "
               "for (let i = 0; i < b.length; i++) { const l = b.getLine(i); if (l) { const t = l.translateToString(true); if (t) o.push(t); } } "
               "return o.join('\\n'); })()")
ENTER = ""


async def type_keys(b, text):
    """Real keyboard input to whatever the page has focused — what a user does."""
    actions = []
    for ch in text:
        v = ENTER if ch == "\n" else ch
        actions.append({"type": "keyDown", "value": v})
        actions.append({"type": "keyUp", "value": v})
    await b.cmd("input.performActions", {"context": b.ctx, "actions": [{"type": "key", "id": "kb", "actions": actions}]})


async def page_flow(launch_url, email, password, server, remote_name, local_name, add_dir="/tmp"):
    """F-1, F-3, F-2 against the real DOM. Returns a list of (name, ok, detail).

    Preconditions the shell arranges: `remote_name` exists only on the server
    with a bundle (a pull button), `local_name` exists locally and is stopped
    (start and push buttons), and this machine is logged in.
    """
    out = []
    def check(name, ok, detail=""):
        out.append({"name": name, "ok": bool(ok), "detail": detail})
        return ok
    try:
        await _page_flow(launch_url, email, password, server, remote_name, local_name, check, add_dir)
    except SystemExit as e:
        # A wait that never came true is a failed claim, not a crash: record
        # it so the run reads as red with the reason, and stop there.
        check("the page never reached the expected state", False, str(e))
    return out


async def _page_flow(launch_url, email, password, server, remote_name, local_name, check, add_dir="/tmp"):
    async with Browser() as b:
        await b.goto(launch_url)
        await b.wait_for("document.getElementById('status').textContent !== 'connecting…'", 30, "the page's own handshake")
        # Logged in already (the account is on this machine), so the list is up.
        await b.wait_for(VISIBLE + "('list')", 30, "the list after the handshake")

        # ---- F-1: log out → the forms are back; log in → they go.
        await b.eval("document.getElementById('logout').click()")
        await b.wait_for(VISIBLE + "('login')", 30, "the login form after logging out")
        check("F-1 after logout the login form is on the page", await b.eval(VISIBLE + "('login')"))
        check("F-1 after logout the header says not logged in", await b.eval("document.getElementById('who').textContent") == "not logged in")
        check("F-1 after logout the list is gone", not await b.eval(VISIBLE + "('list')"))
        await b.eval("document.getElementById('to-register').click()")
        check("F-1 the register form can be shown", await b.eval(VISIBLE + "('register')"))
        await b.eval("document.getElementById('to-login').click()")
        await b.eval(f"(f => {{ f.server.value = {json.dumps(server)}; f.email.value = {json.dumps(email)}; f.password.value = {json.dumps(password)}; f.requestSubmit(); }})(document.getElementById('login'))")
        await b.wait_for(VISIBLE + "('list')", 60, "the list after logging in through the form")
        check("F-1 after login the page has no login form", not await b.eval(VISIBLE + "('login')"))
        check("F-1 after login the page has no register form", not await b.eval(VISIBLE + "('register')"))
        check("F-1 after login the page has no recovery panel", not await b.eval(VISIBLE + "('recovery')"))
        who = await b.eval("document.getElementById('who').textContent")
        check("F-1 after login the header shows the account", email in (who or ""), who)
        check("F-1 after login the header shows a log-out control", await b.eval(VISIBLE + "('logout')"))
        await b.wait_for(f"!!document.querySelector('button[data-pull={json.dumps(remote_name)}]')", 30, "the remote row's pull button")

        # ---- F-11: create the local session through the page's own panel.
        check("F-11 the list has no row for the session yet", not await b.eval(f"!!document.querySelector('button[data-remove={json.dumps(local_name)}]')"))
        await b.eval("document.getElementById('create').click()")
        await b.wait_for(VISIBLE + "('createform')", 10, "the create panel")
        agents = await b.eval("[...document.getElementById('createagent').options].map(o => o.value)")
        sizes = await b.eval("[...document.getElementById('createsize').options].map(o => o.value)")
        chosen = await b.eval("document.getElementById('createsize').value")
        check("F-11 the panel offers the agents and a quota picker, Claude Code and 2GB by default",
              agents and agents[0] == "claude-code" and len(sizes) >= 3 and chosen == "2GB", f"agents={agents} sizes={sizes} default={chosen}")
        await b.eval(f"(f => {{ f.name.value = {json.dumps(local_name)}; f.size.value = '500MB'; f.agent.value = 'claude-code'; f.requestSubmit(); }})(document.getElementById('createform'))")
        await b.wait_for(f"!!document.querySelector('button[data-start={json.dumps(local_name)}]')", 120, "the new row, stopped, after create")
        check("F-11 on success the row appears (stopped) and the panel is closed", not await b.eval(VISIBLE + "('job')"))
        st = await b.eval("document.getElementById('status').textContent")
        check("F-11 the outcome is on the status line", st.startswith(f"create {local_name}: created"), st)
        # A refusal goes to the status line, as F-3 defined: the same name again.
        await b.eval("document.getElementById('create').click()")
        await b.wait_for(VISIBLE + "('createform')", 10, "the create panel again")
        await b.eval(f"(f => {{ f.name.value = {json.dumps(local_name)}; f.requestSubmit(); }})(document.getElementById('createform'))")
        await b.wait_for("document.getElementById('status').textContent.includes('failed')", 60, "the refusal on the status line")
        st = await b.eval("document.getElementById('status').textContent")
        check("F-11 a refusal (the name in use) goes to the status line and closes the panel",
              st.startswith(f"create {local_name} failed:") and not await b.eval(VISIBLE + "('job')"), st)
        await b.wait_for(f"!!document.querySelector('button[data-push={json.dumps(local_name)}]')", 30, "the local row's push button")

        # ---- F-3: open pull, then push — only the push form is in the page.
        panels = "['pullform','pushform','attach'].filter(id => document.getElementById(id).checkVisibility())"
        await b.eval(f"document.querySelector('button[data-pull={json.dumps(remote_name)}]').click()")
        await b.wait_for(VISIBLE + "('pullform')", 10, "the pull form")
        check("F-3 the pull form opens", await b.eval(VISIBLE + "('pullform')"))
        await b.eval(f"document.querySelector('button[data-push={json.dumps(local_name)}]').click()")
        await b.wait_for(VISIBLE + "('pushform')", 10, "the push form")
        open_now = await b.eval(panels)
        check("F-3 open pull then push: only the push form is in the page", open_now == ["pushform"], str(open_now))
        title = await b.eval("document.getElementById('jobtitle').textContent")
        check("F-3 the heading names the open action", title == f"push {local_name}", title)
        await b.eval("document.getElementById('pushcancel').click()")
        check("F-3 cancel closes it", await b.eval(panels) == [] and not await b.eval(VISIBLE + "('job')"), str(await b.eval(panels)))

        # ---- F-2: start the local session through the page, attach, type exit.
        await b.eval(f"document.querySelector('button[data-start={json.dumps(local_name)}]').click()")
        await b.wait_for(f"!!document.querySelector('button[data-attach={json.dumps(local_name)}]')", 120, "the row to say running after start")
        check("F-2 a completed job closed its panel", not await b.eval(VISIBLE + "('job')"))
        await b.eval(f"document.querySelector('button[data-attach={json.dumps(local_name)}]').click()")
        await b.wait_for("document.getElementById('attachnote').textContent.startsWith('attached')", 30, "the terminal to attach")
        check("F-2 the terminal panel opens on attach", await b.eval(VISIBLE + "('attach')"))
        check("F-2 attach clears a previous status", (await b.eval("document.getElementById('status').textContent")) == "")
        await b.eval("document.querySelector('#term textarea').focus()")
        await asyncio.sleep(0.5)
        await type_keys(b, "exit\n")
        await b.wait_for("!document.getElementById('attach').checkVisibility()", 30, "the terminal panel to close on shell exit")
        check("F-2 after exit the terminal panel is gone", not await b.eval(VISIBLE + "('attach')"))
        # The page refreshes the list after an exit and then writes the
        # outcome; reading the status the instant the panel closes is a race
        # by construction, so wait for it the way a user's eye does.
        try:
            await b.wait_for("document.getElementById('status').textContent === 'the shell exited (0)'", 20, "the exit status line")
            st = "the shell exited (0)"
        except SystemExit:
            st = await b.eval("document.getElementById('status').textContent")
        check("F-2 the exit code is one line of status above the table", st == "the shell exited (0)", st)
        row = await b.wait_for(f"(() => {{ const r = document.querySelector('tr.srow[data-name={json.dumps(local_name)}]'); return r ? r.children[3].textContent : ''; }})()", 30, "the row after the exit")
        check("F-2 the row still says running: shell exit is not stop", row == "running", row)
        await b.eval(f"document.querySelector('button[data-attach={json.dumps(local_name)}]').click()")
        await b.wait_for("document.getElementById('attachnote').textContent.startsWith('attached')", 30, "a second attach")
        check("F-2 the next attach clears the exit status", (await b.eval("document.getElementById('status').textContent")) == "")
        await b.eval("document.getElementById('detach').click()")

        # F-13: one screenshot of the list from this headless run, for the PR.
        # Not an assertion (so it never shifts the count) — a capture, gated by
        # an env var, used only when preparing the PR.
        shot = os.environ.get("NEMR_UI_SCREENSHOT")
        if shot:
            try:
                await b.screenshot(shot)
            except Exception as e:
                sys.stderr.write(f"screenshot failed: {e}\n")

        # ---- F-12, the only copy: the name typed exactly enables the button; the
        # running session is stopped first; the row is gone afterwards.
        await b.eval(f"document.querySelector('button[data-remove={json.dumps(local_name)}]').click()")
        await b.wait_for(VISIBLE + "('removeform')", 10, "the remove panel")
        case = await b.eval("document.getElementById('removecase').textContent")
        check("F-12 the only copy: the panel says so, says it will be stopped first, and the button is disabled until the name is typed",
              "only copy" in case and "stopped first" in case and await b.eval(VISIBLE + "('removetypeit')") and await b.eval("document.getElementById('removeconfirm').disabled"), case)
        await b.eval("document.querySelector('#removeform input[name=typed]').focus()")
        await type_keys(b, local_name[:-1])
        check("F-12 a name that does not match keeps the button disabled", await b.eval("document.getElementById('removeconfirm').disabled"))
        await type_keys(b, local_name[-1])
        check("F-12 the exact name enables it", not await b.eval("document.getElementById('removeconfirm').disabled"))
        await b.eval("document.getElementById('removeform').requestSubmit()")
        await b.wait_for("document.getElementById('status').textContent.startsWith('remove ')", 120, "the remove outcome")
        st = await b.eval("document.getElementById('status').textContent")
        gone = not await b.eval(f"!!document.querySelector('button[data-remove={json.dumps(local_name)}]')")
        check("F-12 removed: the row is gone and the outcome names this machine", gone and "removed" in st and "this machine" in st, st)

        # ---- F-12, a copy in the cloud: create, push through the real push form,
        # then remove with one click and no typing; the row stays, as remote.
        third = local_name + "-b"
        await b.eval("document.getElementById('create').click()")
        await b.wait_for(VISIBLE + "('createform')", 10, "the create panel for the third session")
        await b.eval(f"(f => {{ f.name.value = {json.dumps(third)}; f.size.value = '500MB'; f.requestSubmit(); }})(document.getElementById('createform'))")
        await b.wait_for(f"!!document.querySelector('button[data-push={json.dumps(third)}]')", 120, "the third row")
        await b.eval(f"document.querySelector('button[data-push={json.dumps(third)}]').click()")
        await b.wait_for(VISIBLE + "('pushform')", 10, "the push form for the third session")
        await b.eval(f"(f => {{ f.password.value = {json.dumps(password)}; f.release.checked = true; f.requestSubmit(); }})(document.getElementById('pushform'))")
        await b.wait_for(f"(() => {{ const b = document.querySelector('button[data-remove={json.dumps(third)}]'); return !!b && b.dataset.bundle === '1'; }})()", 300, "the third row to carry a bundle after the push")
        await b.eval(f"document.querySelector('button[data-remove={json.dumps(third)}]').click()")
        await b.wait_for(VISIBLE + "('removeform')", 10, "the remove panel for the third session")
        case = await b.eval("document.getElementById('removecase').textContent")
        check("F-12 a copy in the cloud: the panel says the cloud copy stays, no typing, one click",
              "cloud stays" in case and not await b.eval(VISIBLE + "('removetypeit')") and not await b.eval("document.getElementById('removeconfirm').disabled"), case)
        await b.eval("document.getElementById('removeform').requestSubmit()")
        await b.wait_for(f"(() => {{ const r = (window.nemrRows || []).find(x => x.name === {json.dumps(third)}); return !!r && r.where === 'remote'; }})()", 120, "the third row to read remote after the remove")
        check("F-12 removed here: the row stays and reads remote", not await b.eval(f"!!document.querySelector('button[data-remove={json.dumps(third)}]')"))

        # ---- F-21: add an existing folder from the page. The panel IS the
        # confirmation (adding is not destructive), and it must show the plan —
        # what travels, what is excluded, how many transcripts — with the
        # confirm disabled until a folder has actually been measured.
        await b.eval("document.getElementById('addfolder').click()")
        await b.wait_for(VISIBLE + "('addform')", 10, "the add-folder panel")
        check("F-21 the add panel opens with its confirm disabled until a folder is measured",
              await b.eval("document.getElementById('addconfirm').disabled"))
        await b.eval("document.querySelector('#addform input[name=folder]').focus()")
        await type_keys(b, add_dir)
        await b.wait_for("!document.getElementById('addconfirm').disabled", 60, "the plan to come back and enable the confirm")
        plan = await b.eval("document.getElementById('addplan').textContent")
        check("F-21 the panel shows the plan before the confirm: what travels, what is excluded, how many transcripts",
              ("would travel" in plan and "Excluded" in plan and "transcript" in plan and "copied, not moved" in plan), plan[:200])
        check("F-21 there is no typed-name gate — adding is not destructive",
              not await b.eval("!!document.getElementById('addtypeit')"))
        await b.eval("document.getElementById('addcancel').click()")
        check("F-21 cancel closes the add panel", not await b.eval(VISIBLE + "('addform')"))

        # ---- E-22, the only-copy case on the real page: the cloud copy of a
        # remote-only session is the only copy, so the typed name is required
        # (F-12's rule), and deleting it drops the row entirely.
        await b.eval(f"document.querySelector('button[data-cloud={json.dumps(third)}]').click()")
        await b.wait_for(VISIBLE + "('cloudform')", 10, "the cloud-delete panel")
        case = await b.eval("document.getElementById('cloudcase').textContent")
        check("E-22 the only copy: the panel says so and the button is disabled until the name is typed",
              "only copy" in case and await b.eval(VISIBLE + "('cloudtypeit')") and await b.eval("document.getElementById('cloudconfirm').disabled"), case)
        await b.eval("document.querySelector('#cloudform input[name=typed]').focus()")
        await type_keys(b, third[:-1])
        check("E-22 a name that does not match keeps the button disabled", await b.eval("document.getElementById('cloudconfirm').disabled"))
        await type_keys(b, third[-1])
        check("E-22 the exact name enables it", not await b.eval("document.getElementById('cloudconfirm').disabled"))
        await b.eval("document.getElementById('cloudform').requestSubmit()")
        await b.wait_for(f"(() => {{ const r = (window.nemrRows || []).find(x => x.name === {json.dumps(third)}); return !r; }})()", 120, "the third row to vanish after the cloud copy is deleted")
        check("E-22 deleting the only copy drops the row entirely", not await b.eval(f"!!document.querySelector('button[data-cloud={json.dumps(third)}]')"))

        # ---- F-1, the other direction, once more at the end.
        await b.eval("document.getElementById('logout').click()")
        await b.wait_for(VISIBLE + "('login')", 30, "the login form after the final logout")
        check("F-1 after logout the forms are back", await b.eval(VISIBLE + "('login')") and not await b.eval(VISIBLE + "('logout')"))
        # Logging out deleted the account this machine shares with the rest
        # of the acceptance; leave the machine as it was found — logged in,
        # through the form, one more time.
        # E-19: the form is pre-filled with the server remembered from the
        # last login, so an EMPTY field logs in — the page's only way to
        # reach a server here is that memory (NEMR_SERVER_URL is not set).
        prefilled = await b.eval("document.getElementById('login').server.value")
        check("E-19 after logout the form is pre-filled with the remembered server", prefilled == server, prefilled)
        await b.eval(f"(f => {{ f.server.value = ''; f.email.value = {json.dumps(email)}; f.password.value = {json.dumps(password)}; f.requestSubmit(); }})(document.getElementById('login'))")
        await b.wait_for(VISIBLE + "('list')", 60, "the list after logging back in with an empty server field")
        check("E-19 an empty server field logs in through the remembered server", not await b.eval(VISIBLE + "('login')") and await b.eval(VISIBLE + "('logout')"))
        who = await b.eval("document.getElementById('who').textContent")
        check("the page block leaves the machine logged in, as it found it", server in (who or ""), who)


# --- the bucket, read independently of the server -----------------------------
#
# E-20's proof needs a second opinion on what the server stored: the object
# fetched from the bucket by something that is not the server. AWS Signature
# V4 with the standard library only — no SDK, no third-party tool; the
# credential is read from the environment, used for the signature, and never
# printed, placed in a URL, or written anywhere.
import datetime, hashlib, hmac, urllib.parse, urllib.request, urllib.error



def _sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


def s3_request(method, key, body=b"", query=None):
    """One signed request against the bucket named by NEMR_S3_*; returns (status, bytes).

    The credential is read from the environment and used for the signature
    only; it is never printed, never placed in a URL, never written.
    """
    endpoint = os.environ["NEMR_S3_ENDPOINT"].rstrip("/")
    bucket = os.environ["NEMR_S3_BUCKET"]
    access = os.environ["NEMR_S3_ACCESS_KEY_ID"]
    secret = os.environ["NEMR_S3_SECRET_ACCESS_KEY"]
    region = os.environ.get("NEMR_S3_REGION") or ("auto" if os.environ.get("NEMR_S3_PROVIDER") == "r2" else "us-east-1")
    host = urllib.parse.urlparse(endpoint).netloc
    path = "/" + bucket + ("/" + urllib.parse.quote(key, safe="/-_.~") if key else "")
    qs = "&".join(f"{urllib.parse.quote(k, safe='-_.~')}={urllib.parse.quote(str(v), safe='-_.~')}" for k, v in sorted((query or {}).items()))
    now = datetime.datetime.now(datetime.timezone.utc)
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    scope_date = now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(body).hexdigest()
    headers = {"host": host, "x-amz-content-sha256": payload_hash, "x-amz-date": amz_date}
    signed = ";".join(sorted(headers))
    canonical = "\n".join([method, path, qs, "".join(f"{k}:{headers[k]}\n" for k in sorted(headers)), signed, payload_hash])
    scope = f"{scope_date}/{region}/s3/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    k = _sign(_sign(_sign(_sign(("AWS4" + secret).encode(), scope_date), region), "s3"), "aws4_request")
    signature = hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest()
    auth = f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={signature}"
    url = endpoint + path + (("?" + qs) if qs else "")
    req = urllib.request.Request(url, data=body if method in ("PUT", "POST") else None, method=method)
    for h, v in headers.items():
        if h != "host":
            req.add_header(h, v)
    req.add_header("Authorization", auth)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def s3_list(prefix):
    """Keys under a prefix, from a ListObjectsV2 page (enough for an acceptance run's handful)."""
    import re
    status, body = s3_request("GET", "", query={"list-type": "2", "prefix": prefix, "max-keys": "1000"})
    if status != 200:
        raise SystemExit(f"bucket list refused: HTTP {status}")
    return re.findall(r"<Key>([^<]+)</Key>", body.decode(errors="replace"))


def main_no_page(op):
    if op == "s3-get":
        # s3-get <launch_url> <key> <out-file>   (the launch URL is unused; the op needs no page)
        status, body = s3_request("GET", sys.argv[3])
        if status != 200:
            raise SystemExit(f"bucket GET refused: HTTP {status}")
        with open(sys.argv[4], "wb") as f:
            f.write(body)
        print(len(body))
    elif op == "s3-list":
        print("\n".join(s3_list(sys.argv[3])))
    elif op == "s3-delete-prefix":
        keys = s3_list(sys.argv[3])
        for k in keys:
            st, _ = s3_request("DELETE", k)
            if st not in (200, 204):
                raise SystemExit(f"bucket DELETE {k!r} refused: HTTP {st}")
        print(len(keys))
    else:
        raise SystemExit(f"unknown op {op!r}")


async def credential_step_flow(launch_url, local_name):
    """E-21's automated arm against the real page: on a machine with no
    login, the list carries the login line, an attach shows Claude Code's
    sign-in screen, and the page shows the URL as text and as a link.
    Returns (checks, url). Nothing here logs in."""
    out = []
    def check(name, ok, detail=""):
        out.append({"name": name, "ok": bool(ok), "detail": " ".join(str(detail).split())})
        return ok
    url = ""
    try:
        async with Browser() as b:
            await b.goto(launch_url)
            await b.wait_for("document.getElementById('status').textContent !== 'connecting…'", 30, "the page's own handshake")
            await b.wait_for(VISIBLE + "('list')", 60, "the list")
            await b.wait_for(f"!!document.querySelector('button[data-attach={json.dumps(local_name)}]')", 60, "the running row")
            _rows = await b.eval("JSON.stringify((window.nemrRows||[]).map(r=>({n:r.name,w:r.where,cp:r.credential_present})))")
            check("E-21 the page says this machine has no Claude login yet", await b.eval(VISIBLE + "('loginline')"), _rows)
            text = await b.eval("document.getElementById('loginline').textContent")
            check("E-21 the line says what to do: attach and run /login", "/login" in (text or ""), text)
            await b.eval(f"document.querySelector('button[data-attach={json.dumps(local_name)}]').click()")
            await b.wait_for("document.getElementById('attachnote').textContent.startsWith('attached')", 30, "the terminal to attach")
            await b.eval("document.querySelector('#term textarea').focus()")
            await asyncio.sleep(0.5)
            # Claude Code, seeded (F-131), goes straight to the prompt; /login is
            # the way to the sign-in screen. Unseeded it shows the theme picker
            # first — Enter accepts it and the login prompt follows.
            await type_keys(b, "claude\n")
            await asyncio.sleep(12)
            await type_keys(b, "/login\n")
            await asyncio.sleep(8)
            await type_keys(b, "\n")       # the first login method (Claude account)
            try:
                await b.wait_for("!document.getElementById('signin').hidden", 90, "Claude Code's sign-in URL to appear in the terminal")
            except SystemExit as e:
                # The screen is the evidence: what the terminal showed instead.
                screen = await b.eval(SCREEN_TEXT)
                raise SystemExit(f"{e} — the terminal showed: {(screen or '')[-1200:]}")
            check("E-21 the terminal showed Claude Code's sign-in URL and the page shows it as a link", await b.eval("document.getElementById('signinlink').href.startsWith('https://claude.com/')"))
            url = await b.eval("document.getElementById('signinurl').textContent")
            check("E-21 the page shows the URL as text, as Claude Code printed it", "oauth/authorize" in (url or ""), url[:60] + "…" if url else "")
            await type_keys(b, "\x03\x03")
            await asyncio.sleep(1)
            await b.eval("document.getElementById('detach').click()")
    except SystemExit as e:
        check("the page never reached the expected state", False, str(e))
    return out, url


def main():
    # NEMR_UI_COOKIE carries the one session cookie between the steps of the
    # acceptance, the way an open tab carries it between clicks.
    op = sys.argv[1]
    if op.startswith("s3-"):
        # The bucket ops touch no page and spend no token.
        return main_no_page(op)
    if op == "credential-step":
        # credential-step <launch_url> <local_name>
        results, url = asyncio.run(credential_step_flow(sys.argv[2], sys.argv[3]))
        print(json.dumps({"checks": results, "url": url}))
        raise SystemExit(0 if all(r["ok"] for r in results) else 4)
    if op == "page":
        # The page itself exchanges the launch token; this process must not.
        email, password, server, remote_name, local_name = sys.argv[3:8]
        add_dir = sys.argv[8] if len(sys.argv) > 8 else "/tmp"
        results = asyncio.run(page_flow(sys.argv[2], email, password, server, remote_name, local_name, add_dir))
        print(json.dumps(results))
        raise SystemExit(0 if all(r["ok"] for r in results) else 4)
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
    elif op == "create":
        # create <name> [size] [agent] — F-11, the page's own route.
        body = {"name": sys.argv[3]}
        if len(sys.argv) > 4: body["size"] = sys.argv[4]
        if len(sys.argv) > 5: body["agent"] = sys.argv[5]
        print(json.dumps(surface.job("POST", "/sessions", body)))
    elif op == "remove":
        # remove <name> — F-12, from this machine; the cloud copy is never touched.
        print(json.dumps(surface.job("DELETE", f"/sessions/{sys.argv[3]}")))
    elif op == "cloud-delete":
        # cloud-delete <name> [take-over] — E-22, delete the cloud copy.
        take_over = len(sys.argv) > 4 and sys.argv[4] == "take-over"
        print(json.dumps(surface.job("DELETE", f"/sessions/{sys.argv[3]}/cloud", {"take_over": take_over})))
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
