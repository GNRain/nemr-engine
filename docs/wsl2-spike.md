# WSL2 spike — does the engine run at all? (E-10 Half 1)

The first half of the E-10 Windows pass: prove — or disprove — that the
existing engine runs on WSL2 **with no changes**. Nothing gets fixed during
this spike. Every divergence from the Ubuntu-VM baseline gets recorded, and
Half 2 fixes only what this spike proved broken.

Same standard as the cross-VM run: this is a test of the docs as much as the
engine. A step that needed knowledge not written down is a finding, even if it
succeeded. And one more thing this run is, that the cross-VM run was not:
**the first true clean-machine execution of `setup_host.sh`'s privileged
half** — the script's own header says everything from "Packages" onward has
never run end to end on a fresh machine. So a failure below the reboot gate
may be a script bug rather than a WSL2 problem; classify before blaming WSL2.

**Record everything with `tee`.** Logs are the evidence:

```bash
./scripts/setup_host.sh 2>&1 | tee ~/spike-setup-$(date +%F-%H%M).log
```

---

## Part 0 — Windows side, once

1. Update WSL itself, then install a **fresh** Ubuntu (the spike is a
   clean-host test; don't reuse a distro that has history):

   ```powershell
   wsl --update
   wsl --version          # record this verbatim, plus winver
   wsl --install -d Ubuntu-24.04
   ```

2. Record: Windows build (`winver`), `wsl --version` output, and the
   networking mode (default is NAT; check `%UserProfile%\.wslconfig` for
   `networkingMode=mirrored` — if present, say so in the report, it changes
   the port-forwarding and 10.99/16 answers below).

## Part 1 — systemd on, before anything else

Rootless containerd, lingering, and the user manager all hang off systemd.
Newer WSL images enable it by default — **verify, don't assume**:

```bash
ps -p 1 -o comm=          # want: systemd
ls -d /run/systemd/system # want: exists
```

If PID 1 is `init`, enable it and restart the VM:

```bash
printf '[boot]\nsystemd=true\n' | sudo tee -a /etc/wsl.conf
```

then from PowerShell: `wsl --shutdown`, reopen the terminal, re-verify.

**The WSL2 spelling of "reboot" is `wsl --shutdown` from Windows.** Use it
anywhere the sequence says reboot, including `setup_host.sh`'s reboot gate.

**Recommended in the same file — keep the Windows PATH out of the VM:**

```bash
printf '[interop]\nappendWindowsPath=false\n' | sudo tee -a /etc/wsl.conf
```

then `wsl --shutdown` and reopen. By default WSL2 appends the entire Windows
`PATH` — ~30 `/mnt/c/...` entries — to every Linux process's `PATH`, and each of
those is a 9p-mounted Windows drive that is traversed on every command lookup.
Two reasons to turn it off, one proven and one a hypothesis:

- **Proven:** it puts Docker Desktop's `bin` directory on the Linux `PATH`,
  which is not an NFR-01 violation (nothing we build or request depends on it)
  but is exactly what a Docker-freeness checker will trip over. Better absent
  than explained.
- **Hypothesis, deliberately not coded against (f109):** in the Half-2 run,
  `f109_the_cli_does_not_panic_when_its_reader_closes_the_pipe` was intermittent
  under suite load — some of its 40 `bash -c` spawns exited **127** (command
  not found) — yet passed in isolation. The suspected cause is command lookup
  crawling ~30 9p path entries under load. This setting removes them; if f109
  stops flaking with it on, the hypothesis is confirmed and the fix is this
  line. If it keeps flaking, the cause is elsewhere and it becomes its own
  investigation. Either way the test is left honest rather than papered over.

## Part 2 — the probe battery (before running anything of ours)

This is the divergence baseline. Run each line and record all output —
a probe that errors is data, not a problem to fix:

```bash
uname -r                                             # baseline: 6.8 on the reference host
stat -fc %T /sys/fs/cgroup                           # want cgroup2fs; tmpfs means v1/hybrid — see prediction 1
cat /sys/fs/cgroup/cgroup.controllers 2>&1
sysctl kernel.unprivileged_userns_clone 2>&1         # Ubuntu-patch knob; likely ABSENT on the MS kernel (benign)
sysctl kernel.apparmor_restrict_unprivileged_userns 2>&1   # same — likely absent (benign)
unshare --user --map-root-user echo "USERNS OK"
unshare -rmn true && echo "NS OK"                    # the functional check that actually matters
grep -H "$(id -un)" /etc/subuid /etc/subgid
ls -l /dev/loop-control 2>&1; sudo losetup -f 2>&1   # loop support — prediction 3
grep -E 'ext4|overlay' /proc/filesystems
zcat /proc/config.gz | grep -E 'CONFIG_BLK_DEV_LOOP=|CONFIG_EXT4_FS=|CONFIG_USER_NS=|CONFIG_VETH=|CONFIG_NF_NAT=|CONFIG_IP_NF_IPTABLES=|CONFIG_OVERLAY_FS='
loginctl show-user "$(id -un)" 2>&1 | head -5
systemctl --user is-system-running 2>&1              # is there a user manager at all
echo "$XDG_RUNTIME_DIR"; ls -ld "/run/user/$(id -u)" 2>&1
ip route                                             # any route overlapping 10.99.0.0/16? — prediction 6
cat /etc/resolv.conf                                 # WSL generates this; record what resolver it points at
df -h / ~; free -h                                   # the VM's disk and RAM allowance
```

## Part 3 — the sequence, exactly as the cross-VM run

1. **Clone onto the WSL2 ext4 root — never under `/mnt/c`.** Anything under
   `/mnt/c` is a 9p-mounted Windows drive: slow, permission-strange, and no
   place for a repo, a build, or `~/.local/share/nemr`. Everything lives in
   the Linux home (`~/src/nemr-engine`); the warning is explicit and Half 2
   will likely teach `setup_host.sh` to refuse it.
2. Rust toolchain via rustup (PREREQUISITES.md Step 3).
3. `./scripts/setup_host.sh` (with `tee`, as above). At the reboot gate:
   `wsl --shutdown` from PowerShell, reopen, re-run the script.
4. `claude` — log in (per-device credential, D-02; nothing travels).
5. `./scripts/verify_wp_a.sh` standalone, even though setup runs it at the
   end — the standalone rerun is part of the baseline.

That is the spike deliverable: both scripts, plus every divergence. The
Half-2 acceptance (bundle round-trip Linux↔WSL2 with history intact, both
directions; a session's dev server reachable from a Windows browser) comes
**after** the spike verdict, not during.

---

## Predictions — what we expect to break, and why

Ranked. "Fires" means the run stops or diverges visibly; "benign" means a
recorded difference that shouldn't block.

1. **cgroup hybrid/v1 boot → preflight refusal. Most likely real blocker,
   with a known fix.** WSL2 kernels have historically mounted cgroup v1 (or
   hybrid), and `setup_host.sh` refuses without the v2 unified hierarchy.
   Newer WSL + systemd may already boot pure v2 — the Part 2 `stat` probe
   decides. If it fires, the fix is Windows-side, in
   `%UserProfile%\.wslconfig`:

   ```ini
   [wsl2]
   kernelCommandLine = cgroup_no_v1=all
   ```

   then `wsl --shutdown`. Record whether it was needed. Even with v2, the
   **delegation** check (Step 2a's drop-in, cpu in the user slice) is the
   next thing to watch after the shutdown-instead-of-reboot cycle.

2. **The reboot gate reads wrong on WSL2 — certain, cosmetic.** The script
   says `sudo reboot`; the WSL2 spelling is `wsl --shutdown` from Windows.
   Divergence to record for Half 2's docs, not a failure.

3. **Loop devices — should work; if it doesn't, it's the biggest finding.**
   `LOOP_CONFIGURE` needs ≥ 5.8 and WSL2 kernels are well past it, but they
   are also trimmed builds. The config probe tells us up front whether
   `CONFIG_BLK_DEV_LOOP` and ext4 are in (both expected: `=y`, WSL's own
   root is ext4). The real test is `nemr create` driving the helper:
   `/dev/loop-control` open, `LOOP_CONFIGURE`, then `mount(2)` on the loop
   device. WSL2's `/dev` is devtmpfs without a full udev; if node creation
   or the mount misbehaves, that's a genuine engine-level divergence.

4. **Step 0 sysctls absent — near-certain, benign.** Both
   `kernel.unprivileged_userns_clone` and
   `kernel.apparmor_restrict_unprivileged_userns` are Ubuntu kernel-patch
   knobs. The Microsoft kernel likely has neither; `setup_host.sh` treats an
   absent apparmor knob as a pass, and the functional `unshare` probes are
   the truth. Expect user namespaces to just work.

5. **subuid/subgid — probably present, self-healing if not.** WSL's
   first-user creation normally writes the ranges; if not, the script adds
   them and says so.

6. **NET-02 egress and DNS — should work; this is the thing to actually
   prove from inside a session.** Two NAT layers stack (engine veth+iptables
   inside rootlesskit's namespace, then WSL2's own NAT), but slirp4netns
   terminates TCP in userspace so the inner layer doesn't care what's
   outside. Two watch-items: WSL's **generated resolv.conf** (record which
   resolver the session ends up using, and whether it answers from inside
   the session netns — `getent hosts example.com` and one real `curl` from
   an attached session are the proof); and the **10.99.0.0/16 refusal** —
   NAT mode uses 172.x plumbing and shouldn't collide, but **mirrored mode
   imports the Windows LAN's addressing into WSL2**, and a 10.x LAN would
   trip `nemr create`'s overlap refusal exactly as designed. The Part 2
   `ip route` output settles it before anything runs.

7. **Port forward to a Windows browser — expected to work in NAT mode via
   WSL's localhost relay; human-verified only.** `nemr port add` binds the
   listener in the distro's network namespace; WSL2's localhost forwarding
   is what carries `http://localhost:PORT` from Windows into the VM. It has
   known flaky edges (fails after long sleep/resume cycles; historically
   sensitive to bind address) and mirrored mode replaces the relay entirely.
   If the browser can't reach it, record `ss -tlnp` inside WSL2 (is the
   listener there?) before blaming the relay — that splits engine-forward
   failure from WSL-relay failure.

8. **WSL2 VM lifecycle semantics — not a bug, but record them.** The VM
   stops when idle (last window closed, no services pinning it), which stops
   rootless containerd and every running session — the moral equivalent of a
   surprise host reboot. Lingering + systemd should restart the daemons on
   the next VM boot, and `nemr list`/`nemr reconcile` exist for exactly this
   crash shape. Worth one deliberate test: close every terminal, wait,
   reopen, `nemr list`.

9. **Resources — watch, don't pre-tune.** WSL2 defaults to a fraction of the
   machine's RAM and a growable VHD. The base-image build is the heaviest
   step; if it OOMs or fills the VHD, `.wslconfig` `memory=` and disk notes
   become a PREREQUISITES item for Half 2.

**Not in scope, on purpose:** GPU/CUDA (separate future pass, dependent on
this one), macOS (deferred), any code changes, any Windows-native anything.

---

## Recording divergences

One entry per divergence, in order encountered:

```
### <n>. <one-line title>
step:      <which part/step, which script line or message>
expected:  <the Ubuntu-VM baseline behaviour>
observed:  <verbatim output — paste, don't paraphrase>
class:     WSL2 semantics | script bug | docs gap | blocker (best guess)
```

A step that passed but needed unwritten knowledge gets an entry too, class
`docs gap`. The spike report is the full list plus the two `tee` logs, and
Half 2's scope is derived from it — nothing else.

---

## Verdict — written after Half 2 closed (2026-09-03)

All eight predictions now have empirical answers, and the two that mattered
went the **other way** from expectation:

- **#1, cgroup hybrid — predicted the most likely blocker — never fired.** The
  host booted pure `cgroup2fs` with every controller present.
- **Mount propagation — not predicted at all — was the entire cause of all 12
  failures.** WSL2's `/init` leaves `/` a private mount, so the helper's volume
  mount never reached rootlesskit's rslave namespace and every project-starting
  test died at the same `start_task` step (F-128; fixed by a boot-ordered
  systemd unit, since a live `make-rshared` did not survive `wsl --shutdown`).
- **#8, VM idle-stop — recovers cleanly.** After the VM idle-stopped with a
  running project, `nemr list`/`status` reported *stopped, volume unmounted,
  loop device none* — the truth, not a stale "running" — and start/attach/
  stop/delete left no orphans and needed no reconcile. That is VOL-06 absorbing
  a crash shape nobody designed it for.

The lesson worth keeping is not that the predictions were right; the ones that
mattered were wrong. It is that the **divergence template above was ready when
the unpredicted thing appeared**, so the 12 failures were recorded as observables
first and diagnosed second — two independent investigations that converged on
one cause, rather than a guess that happened to fit. Keep the template; hold the
predictions loosely.
