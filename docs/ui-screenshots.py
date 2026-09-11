#!/usr/bin/env python3
"""Headless screenshots of the real page, for a PR to be looked at before it is run.

Not a test: it asserts nothing. It drives the same Firefox-over-BiDi browser the
acceptance uses, against a real `nemr ui` talking to a real daemon and a real
throwaway sync server, and writes four PNGs — the logged-out card, the session
list, the create panel with the quota slider, and an attached terminal.

    python3 docs/ui-screenshots.py <launch-url> <email> <password> <server-url> <outdir>
"""
import asyncio, importlib.util, json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("uiacc", os.path.join(HERE, "ui-acceptance.py"))
uiacc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(uiacc)
Browser, VISIBLE = uiacc.Browser, uiacc.VISIBLE


async def shots(launch_url, email, password, server, outdir, session):
    os.makedirs(outdir, exist_ok=True)
    async with Browser() as b:
        await b.goto(launch_url)
        await b.wait_for(VISIBLE + "('login') || " + VISIBLE + "('list')", 30, "the page")
        if await b.eval(VISIBLE + "('list')"):
            # A state directory from an earlier run already holds a session.
            # Log out so every shot comes from THIS build, then register the
            # account this run was given.
            await b.eval("document.getElementById('logout').click()")
            await b.wait_for(VISIBLE + "('login')", 60, "the login card after logging out")
        await b.screenshot(os.path.join(outdir, "1-logged-out.png"))
        print("1-logged-out.png")

        # Register, which lands on the recovery card, then confirm through it.
        await b.eval("document.getElementById('to-register').click()")
        await b.wait_for(VISIBLE + "('register')", 15, "the register card")
        await b.eval("(f => { f.server.value = %s; f.email.value = %s; f.password.value = %s; f.again.value = %s; f.requestSubmit(); })(document.getElementById('register'))"
                     % (json.dumps(server), json.dumps(email), json.dumps(password), json.dumps(password)))
        await b.wait_for(VISIBLE + "('recovery')", 180, "the recovery card")
        code = await b.eval("document.getElementById('code').textContent")
        await b.screenshot(os.path.join(outdir, "2-recovery.png"))
        print("2-recovery.png")
        await b.eval("(f => { f.code.value = %s; f.requestSubmit(); })(document.getElementById('confirm'))" % json.dumps(code.strip()))
        await b.wait_for(VISIBLE + "('list')", 120, "the session list")
        try:
            await b.wait_for("document.querySelectorAll('.card').length > 0", 60, "at least one card")
        except Exception as e:
            print("no cards:", e)
            print("payload:", await b.eval("(async () => JSON.stringify(await (await fetch('/api/sessions', {headers: {'X-Nemr-Request': '1'}})).json()).slice(0,900))()"))
        await b.screenshot(os.path.join(outdir, "3-list.png"))
        print("3-list.png")
        await panels(b, outdir, session)


async def panels(b, outdir, session):
        await b.eval("document.getElementById('create').click()")
        await b.wait_for(VISIBLE + "('createform')", 15, "the create panel")
        if not await b.eval("!!document.getElementById('createsizerange')"):
            print("picker did not build:", await b.eval("document.getElementById('createsizepick').innerHTML.slice(0,400)"))
            print("probe:", await b.eval("JSON.stringify({pick: !!document.getElementById('createsizepick'), hidden: !!document.getElementById('createsize'), forms: [...document.querySelectorAll('[id^=createsize]')].map(e => e.id)})"))
            raw = await b.eval("(async () => (await fetch('/api/sessions', {headers:{'X-Nemr-Request':'1'}})).text())()")
            print("options:", (raw or "")[:600])
            return
        await b.eval("(() => { const f = document.getElementById('createform'); f.name.value = 'orbit-refactor';"
                     " const r = document.getElementById('createsizerange');"
                     " r.value = String(Math.round(Number(r.max) * 0.62)); r.dispatchEvent(new Event('input')); })()")
        await b.screenshot(os.path.join(outdir, "4-create.png"))
        print("4-create.png")
        await b.eval("document.getElementById('createcancel').click()")

        # An attached terminal, on a session that is already running.
        await b.wait_for(f"!!document.querySelector('button[data-attach={json.dumps(session)}]')", 60, "an attachable session")
        await b.eval(f"document.querySelector('button[data-attach={json.dumps(session)}]').click()")
        await b.wait_for(VISIBLE + "('attach')", 60, "the terminal")
        await b.wait_for("!!document.querySelector('#term textarea')", 60, "xterm")
        await asyncio.sleep(3)
        await b.eval("(() => { const t = document.querySelector('#term textarea'); t.focus(); })()")
        await uiacc.type_keys(b, "ls -la /workspace\r")
        await asyncio.sleep(3)
        # The terminal sits under the list, so put it on screen before the shot.
        await b.eval("document.getElementById('attach').scrollIntoView({block:'start'})")
        await asyncio.sleep(1)
        await b.screenshot(os.path.join(outdir, "5-terminal.png"))
        print("5-terminal.png")


if __name__ == "__main__":
    asyncio.run(shots(sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6]))
