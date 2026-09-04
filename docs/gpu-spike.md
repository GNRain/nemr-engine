# GPU spike — can a rootless container use the GPU? (local-LLM, Phase 1)

The eventual goal is a session that runs a local LLM on the host's GPU and
points a coding agent at it — fully local inference, no API calls out. The
hardware is an **RTX 3070, 8 GB**, in the Windows machine, reachable only
through WSL2 (E-10). That is enough for a 7B–8B model quantised, not enough for
anything that codes well — and that is fine, because this phase is about
whether the **plumbing** works at all, not whether the model is good.

This is Phase 1 of four, and it asks exactly one question: **can a rootless
containerd container, created the way *we* create them, use the GPU?** Same
discipline as the ports spike and NET-02: establish feasibility before any
code, and be willing to come back with **"not feasible on this stack."** That
is an acceptable verdict; a smoothed-over "mostly works" is not.

**Build nothing.** No `setup_host.sh` changes, no engine code, no base-image
variant, no containerd config. Everything below is done by hand on the WSL2
host and recorded. Phase 2 (Ollama in a container, by hand) and Phase 3 (an
agent talking to it) also touch nothing in Nemr. Only Phase 4 is engine work,
and only once the stack is proven by hand.

**Not in scope now:** which agent, model choice, Nemr code, bundle format,
weights storage, multi-session GPU sharing. All downstream of Phase 1's answer.

**Two costs, stated up front rather than discovered later:**

- **Nothing in this line of work will ever have CI coverage.** GitHub's runners
  have no GPU. The WSL2 box is the only machine with one (the VirtualBox VM
  cannot have one), so this is manual verification, permanently — the same
  cost E-10 accepted for WSL2, extended to the GPU.
- **`nvidia-container-toolkit` comes from NVIDIA's apt repository** — a
  third-party repo, exactly what D-13 just ruled `setup_host.sh` does not add.
  For the spike, install it **by hand**, record that as a decision to be made
  later, and do not touch `setup_host.sh`. `nerdctl`, if used in Part 4, is a
  GitHub release binary — same category, same recording.

Nothing here uses Docker Engine (NFR-01). The CUDA images are pulled by `ctr`
from a registry; a registry is not a daemon.

**Record everything with `tee`.** Logs are the evidence:

```bash
mkdir -p ~/gpu-spike && cd ~/gpu-spike
# prefix every command block below with:  2>&1 | tee -a ~/gpu-spike/step-<n>.log
```

---

## The one fact that shapes Part 4 — read before running anything

Nemr creates containers over containerd's **direct gRPC API** (the
`nemr-containerd` crate), not through `nerdctl`, `podman` or the CRI plugin.
Its `ContainerSpec` can express **bind mounts and environment variables** —
and nothing else that a GPU needs: no `linux.devices` entries, no OCI hooks,
no CDI. And containerd's core does **not** apply CDI specs itself; CDI is
applied by the *client* that builds the OCI spec (nerdctl, podman, CRI). On the
reference host even `ctr` on containerd 2.2.1 has no CDI flag — verify the same
on WSL2, don't assume.

So the sharp form of the question is not "does CDI work on WSL2" but:

> **Can the GPU be reached inside a rootless container using *only* bind
> mounts and env — what our spec can already say — or does it need device
> entries, hooks, or a CDI-aware client?**

If bind mounts + env are enough, Phase 4 is small. If it needs `linux.devices`,
the engine grows a device field. If it needs hooks or works only through
`nerdctl`, say so — that is a finding, and it tells us what the engine would
have to do differently. Part 4 is built to separate those cases.

---

## Part 0 — Windows side, once

The **only** NVIDIA driver in this picture is the **Windows** one. WSL2 GPU
support is paravirtualised: the Windows driver is projected into the distro
through `/dev/dxg` (the `dxgkrnl` bridge) and the driver's user-space
libraries appear under `/usr/lib/wsl/lib`. **Never install a Linux NVIDIA
driver inside the distro** — it does not work and can break the projected
path. If a distro package tries to pull one in as a dependency, that is a
divergence to record and refuse.

Record, verbatim:

```powershell
# from Windows
nvidia-smi                       # Windows driver version, CUDA version it reports
winver                           # Windows build
wsl --version                    # WSL, kernel, WSLg versions
type %UserProfile%\.wslconfig    # confirm nothing disables the GPU; note networkingMode
```

The Windows driver must be one with WSL support (any current GeForce driver
is). Its version is a **precondition**: `/usr/lib/wsl/lib` is a projection of
*this* driver, so a driver update changes what every later step saw. See
"What to record".

---

## Step 1 — does WSL2 see the GPU?

```bash
which nvidia-smi                 # expect /usr/lib/wsl/lib/nvidia-smi; absent from PATH is a divergence, not a failure
/usr/lib/wsl/lib/nvidia-smi      # run by full path regardless
nvidia-smi --query-gpu=name,driver_version,memory.total,compute_cap --format=csv
```

Record the driver version, the **CUDA version** the header reports (that is
the *maximum* CUDA the driver supports, not an installed toolkit — Step 5's
image must not exceed it), the GPU name, and VRAM. **Gate:** if `nvidia-smi`
fails here, stop — nothing downstream can work, and the fix is Windows-side
(driver, or WSL/`.wslconfig`), not ours.

---

## Step 2 — what device nodes actually exist?

This is where the first surprise is expected. WSL2 does **not** expose
`/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm` the way bare metal does.
Establish what is really there rather than assuming either shape, and —
because everything we do is rootless — establish **who can open it**.

```bash
# nodes
ls -l /dev/dxg /dev/nvidia* 2>&1                 # expect: dxg present; nvidia* absent
stat -c '%A %a %U:%G %t:%T %n' /dev/dxg          # mode, owner, major:minor
ls -l /proc/driver/nvidia 2>&1                   # expect absent (that is a bare-metal artifact)
lsmod | grep -iE 'dxg|nvidia'; zcat /proc/config.gz | grep -E 'DXGKRNL|CONFIG_DRM'

# the projected libraries
ls -la /usr/lib/wsl/lib/ | head -40
findmnt -no SOURCE,FSTYPE,OPTIONS /usr/lib/wsl/lib /usr/lib/wsl/drivers 2>&1   # what KIND of mount; look for noexec
cat /etc/ld.so.conf.d/*wsl* 2>&1; ldconfig -p | grep -E 'libcuda|libnvidia-ml|libdxcore'

# can the UNPRIVILEGED user open the device?  (open only — no ioctl, no compute)
id -un; id -u
( exec 3<>/dev/dxg && echo "open /dev/dxg: OK as $(id -un)" ) 2>&1

# and is all of it visible INSIDE rootlesskit's mount namespace — where our containerd,
# and therefore runc, actually live?  (F-128 is why this question exists)
CHILD=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")
nsenter -U --preserve-credentials -m -t "$CHILD" ls -l /dev/dxg 2>&1
nsenter -U --preserve-credentials -m -t "$CHILD" ls /usr/lib/wsl/lib 2>&1 | head -5
nsenter -U --preserve-credentials -m -t "$CHILD" sh -c '( exec 3<>/dev/dxg && echo "open /dev/dxg: OK inside rootlesskit" )' 2>&1
```

**Gate:** the unprivileged open must succeed, on the host *and* inside
rootlesskit's namespace. A root-only `/dev/dxg` is a hard blocker for a
rootless stack — record the mode and owner verbatim and stop; that verdict is
"not feasible without a udev/ACL change", which is a different (root, host)
decision. Record the mount type of `/usr/lib/wsl/lib` too: a `noexec` mount of
the driver libraries would fire later, at Step 5, as a loader error.

---

## Step 3 — does the NVIDIA container toolkit work here?

Install **by hand**, from NVIDIA's repository, and **record every line** — this
is the third-party repo D-13 keeps out of `setup_host.sh`, and it stays a
decision for later:

```bash
# NVIDIA's documented apt setup (keyring + list), then:
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
nvidia-ctk --version
dpkg -l | grep -E 'nvidia-container|libnvidia-container'   # record versions
```

**Do NOT run `nvidia-ctk runtime configure`.** That edits containerd's config
to add a `nvidia` runtime — a config change (build nothing) and the *legacy*
hook path. This spike tests **CDI**, which needs no runtime registration.

Generate the CDI spec twice — auto-detected, then with the WSL mode forced —
and **read it**:

```bash
sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml            # note which mode it auto-detected
sudo nvidia-ctk cdi generate --mode=wsl --output=/etc/cdi/nvidia-wsl.yaml
nvidia-ctk cdi list
diff /etc/cdi/nvidia.yaml /etc/cdi/nvidia-wsl.yaml && echo "auto == wsl"

# the two questions, answered from the file, not the exit status:
grep -nE 'path:|hostPath:|containerPath:' /etc/cdi/nvidia-wsl.yaml     # which DEVICE NODES and MOUNTS it names
grep -nE 'hooks:|hookName|nvidia-cdi-hook|nvidia-ctk|args:' /etc/cdi/nvidia-wsl.yaml   # which HOOKS it wants run
grep -nE '^\s*env:|NVIDIA_|LD_LIBRARY' -A3 /etc/cdi/nvidia-wsl.yaml   # which ENV it sets
```

**Gate:** (a) a spec is produced at all; (b) it names the devices Step 2
actually found. A spec naming `/dev/nvidia0` on a host that has only
`/dev/dxg` is a spec that will fail at container start — record it as a
divergence and use `--mode=wsl`. Copy the final spec into the report; Part 4
hand-applies exactly what it describes, so its contents *are* the Phase-4
input. Note especially whether it lists **hooks** (`update-ldcache`,
`create-symlinks`, ...): those run a host binary at container start and are
the part our spec cannot express.

---

## Step 4 — can a rootless containerd container see the GPU? (the one that matters)

Three arms, ordered from "the documented path" to "our path". Each arm runs
against **our** rootless containerd (the unit `setup_host.sh` installed, at
`$CONTAINERD_ADDRESS`), never a second daemon. Pull one image first and record
its digest — pick a `-base` tag whose CUDA version is **≤ the driver's
reported CUDA version** from Step 1 (e.g. `docker.io/nvidia/cuda:12.4.1-base-ubuntu22.04`
is the shape; the version is chosen from Step 1, not copied from here):

```bash
export CONTAINERD_ADDRESS="${XDG_RUNTIME_DIR}/containerd/containerd.sock"
IMG=docker.io/nvidia/cuda:<ver>-base-ubuntu22.04
ctr -n default images pull --platform linux/amd64 "$IMG"
ctr -n default images ls | grep cuda        # record the digest
ctr version; runc --version                 # record; and: does THIS ctr know CDI?
ctr run --help | grep -i cdi || echo "ctr has no CDI flag (expected: CDI is the client's job)"
```

**Every `ctr run` below must run inside rootlesskit's namespaces.** A bare
`ctr run` fails on the overlay mount — PREREQUISITES.md ("When nsenter IS
required") already says so, and the first version of this runbook omitted it;
that cost the Phase 1 run two cycles (a docs gap in this document, recorded in
the Verdict). Define the wrapper once, and discover the driver store once:

```bash
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")
CTR() { nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
          env CONTAINERD_ADDRESS=/run/containerd/containerd.sock ctr -n default "$@"; }

# The NVIDIA driver store is a HASHED directory whose name changes on every
# Windows driver update. Discover it — never hardcode it. (Phase 4 must do the
# same: this is a precondition Phase 1 found, not a path to bake in.)
DRV=$(ls -d /usr/lib/wsl/drivers/nv_dispi.inf_amd64_* | head -1); echo "driver store: $DRV"
# nvidia-smi is NOT on PATH inside the CUDA image; it lives in the driver store.
```

The image ships the CUDA **runtime**, deliberately **not** the driver library:
`libcuda.so` must come from the injected `/usr/lib/wsl/lib`, or the arm has
proven nothing about the host GPU.

### Arm A — the documented path, as the control (nerdctl + CDI)

`nerdctl` is a GitHub release binary (record it: hand-installed, third-party).
Point it at **our** socket; if it insists on its own rootless setup
(`containerd-rootless-setuptool.sh`), that is a divergence — record it and
**do not** reconfigure our stack to please it.

```bash
nerdctl --address "unix://$CONTAINERD_ADDRESS" -n default run --rm \
  --device nvidia.com/gpu=all "$IMG" nvidia-smi
```

This answers: **does CDI injection work at all** in rootless containerd on
WSL2? If Arm A fails, the stack is the problem and Arms B/C cannot succeed —
record the verbatim error and stop here. If it works, it is the control for
everything below: a later failure is then attributable to *our path*, not to
the GPU or the toolkit.

### Arm B — `ctr`, the closest analog to our gRPC path, with the CDI edits applied by hand

`ctr` builds the OCI spec directly and applies no CDI — exactly our situation.
So apply, by hand, what Step 3's spec describes, in two forms that separate
"device entry" from "bind mount":

**B1 — as a device entry** (`linux.devices`, the mknod path):

```bash
CTR run --rm --device /dev/dxg \
  --mount type=bind,src=/usr/lib/wsl/lib,dst=/usr/lib/wsl/lib,options=rbind:ro \
  --mount type=bind,src="$DRV",dst="$DRV",options=rbind:ro \
  --env LD_LIBRARY_PATH="/usr/lib/wsl/lib:$DRV" \
  "$IMG" gpu-b1 "$DRV/nvidia-smi"
```

**B2 — as a bind mount only** (what our `ContainerSpec` can express today):

```bash
CTR run --rm \
  --mount type=bind,src=/dev/dxg,dst=/dev/dxg,options=rbind:rw \
  --mount type=bind,src=/usr/lib/wsl/lib,dst=/usr/lib/wsl/lib,options=rbind:ro \
  --mount type=bind,src="$DRV",dst="$DRV",options=rbind:ro \
  --env LD_LIBRARY_PATH="/usr/lib/wsl/lib:$DRV" \
  "$IMG" gpu-b2 "$DRV/nvidia-smi"
```

Add any further `env:` entries Step 3's spec listed (record which). If
`nvidia-smi` cannot find its libraries, the diagnostic is the loader, not the
GPU:

```bash
CTR run --rm <same mounts/env> "$IMG" gpu-ld sh -c "ldconfig -p | grep -E 'libcuda|libnvidia-ml'; LD_DEBUG=libs $DRV/nvidia-smi 2>&1 | head -20"
```

If the device is present but **opening it fails inside the container**
(`EPERM`/`Operation not permitted` while `ls -l /dev/dxg` shows it), that is
the device cgroup, not the mount — record it verbatim; the condition is then
"the device must be allowed in the container's device cgroup", which is a
`linux.resources.devices` rule, another thing our spec does not express.

**B2 is the decisive arm.** Its outcomes map directly onto Phase 4:

| B1 | B2 | Meaning for the engine |
|---|---|---|
| ok | ok | Bind mounts + env suffice. Phase 4 is small: two mounts and an env var on the spec. |
| ok | fail | The GPU needs a `linux.devices` entry. The engine grows a device field. |
| fail | ok | Rootless mknod is refused but bind works — the good case for us; note it. |
| fail | fail (A ok) | Something the hand-applied edits omit: hooks, ldcache, or a cgroup rule. Diff against Step 3's spec; name the condition. |
| fail | fail (A fail) | The stack, not our path. Verdict from Arm A. |

### Arm C — the Phase-4 input (no code)

Only if B2 (or B1) works: write down the **exact** OCI-spec delta that was
sufficient — every mount, env var, device entry and cgroup rule, and nothing
that was not needed. That list, plus Step 3's spec, is what Phase 4 implements.
Nothing is built here.

---

## Step 5 — does CUDA actually compute?

`nvidia-smi` inside a container proves device *visibility*. It does not prove
a CUDA context can be created or a kernel run. This is the presence-versus-
continuity distinction applied to a GPU — a device node appearing is not the
same as compute working — so Step 5 runs **real work with a checkable result**.

The workload is a self-checking vector add: the device computes, the host
verifies every element, and the program prints `CUDA COMPUTE PASS` only if the
result is right. Any CUDA error is printed **verbatim** — that text *is* the
diagnostic (`no CUDA-capable device` means injection failed; `CUDA driver
version is insufficient for CUDA runtime version` means the image's CUDA
exceeds the Windows driver's — go back to Step 1's number).

```bash
cat > ~/gpu-spike/check.cu <<'EOF'
#include <cstdio>
#include <cuda_runtime.h>
__global__ void add(const float* a, const float* b, float* c, int n) {
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) c[i] = a[i] + b[i];
}
#define CK(x) do { cudaError_t e = (x); if (e != cudaSuccess) { \
    printf("CUDA ERROR at %s: %s\n", #x, cudaGetErrorString(e)); return 1; } } while (0)
int main() {
    const int n = 1 << 20;
    cudaDeviceProp p; CK(cudaGetDeviceProperties(&p, 0));
    printf("device 0: %s, %zu MB\n", p.name, p.totalGlobalMem >> 20);
    float *a, *b, *c;
    CK(cudaMallocManaged(&a, n * sizeof(float)));
    CK(cudaMallocManaged(&b, n * sizeof(float)));
    CK(cudaMallocManaged(&c, n * sizeof(float)));
    for (int i = 0; i < n; i++) { a[i] = i; b[i] = 2.0f * i; c[i] = -1; }
    add<<<(n + 255) / 256, 256>>>(a, b, c, n);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());
    for (int i = 0; i < n; i++) if (c[i] != 3.0f * i) { printf("CUDA COMPUTE FAIL at %d: %f\n", i, c[i]); return 2; }
    printf("CUDA COMPUTE PASS (n=%d, device=%s)\n", n, p.name);
    return 0;
}
EOF
```

It needs `nvcc`, so use the matching **`-devel`** image (same CUDA version as
Step 4's `-base`; record its digest). Compile and run it **inside the
container**, first through the arm that worked in Step 4:

```bash
DEV=docker.io/nvidia/cuda:<ver>-devel-ubuntu22.04
ctr -n default images pull --platform linux/amd64 "$DEV"
# Arm B2 form (substitute the arm that worked) — through the CTR wrapper, with the driver store:
CTR run --rm \
  --mount type=bind,src=/dev/dxg,dst=/dev/dxg,options=rbind:rw \
  --mount type=bind,src=/usr/lib/wsl/lib,dst=/usr/lib/wsl/lib,options=rbind:ro \
  --mount type=bind,src="$DRV",dst="$DRV",options=rbind:ro \
  --mount type=bind,src="$HOME/gpu-spike",dst=/work,options=rbind:ro \
  --env LD_LIBRARY_PATH="/usr/lib/wsl/lib:$DRV" \
  "$DEV" gpu-compute sh -c 'nvcc -o /tmp/check /work/check.cu && /tmp/check'
```

**The control, stated honestly:** a true *host-side* compute control would
need the CUDA toolkit installed in WSL2 itself — a large install from another
third-party repo — so it is deliberately not done. The control is instead the
**two-arm comparison**: run the same compute through Arm A (nerdctl, the
documented path). A computes and B2 does not → our path is missing something
(name it). Neither computes → the stack. Both compute → Phase 1 is answered.

If `cudaMallocManaged` fails where plain `cudaMalloc` would not, record it: on
WSL2, unified memory has known constraints, and Phase 2's inference engine
cares. Record the run time too — an order-of-magnitude slow result is a
finding about the paravirtualised path, not a pass.

---

## Predictions — what we expect to break, and why

Ranked. "Fires" means the step stops or diverges visibly; "benign" means a
recorded difference that shouldn't block. This habit has now inverted twice
(cgroup hybrid never fired; mount propagation was not predicted and was the
whole cause) — the value is the divergence template being ready, not these
being right. Hold them loosely.

1. **containerd's core does not apply CDI — near-certain, and the central
   finding, not a blocker.** CDI is a client-side spec transform. Our gRPC
   client is not CDI-aware, and neither is `ctr`. Expect Arm A (nerdctl) to
   work where a naive "hand the CDI name to containerd" would not. The real
   question is whether Arm B2's *hand-applied* subset is enough — that is what
   decides Phase 4's size.

2. **Device injection rootless: `linux.devices` vs bind mount — likely to fire
   once, with a known shape.** A rootless runc cannot `mknod` inside a user
   namespace; whether it falls back to bind-mounting the node, or B1 simply
   fails, is exactly what B1-vs-B2 measures. Separately, the **device cgroup**
   may refuse the open even when the node is visible — the WSL2 `dxg` major
   number is dynamic, and whether rootless cgroup v2 filters it at all is
   unknown. If it fires: `EPERM` on open with the node present. The condition
   would then be a `linux.resources.devices` allow rule — not expressible by
   our spec today.

3. **`nvidia-ctk cdi generate` needs `--mode=wsl` — medium; benign if the
   auto-detect gets it right.** The toolkit has a WSL mode that names
   `/dev/dxg` and mounts `/usr/lib/wsl/lib`. Auto-detection may or may not
   choose it; a spec naming `/dev/nvidia*` is the failure shape. Step 3's
   `diff` decides.

4. **The CDI spec lists hooks our spec cannot express — medium, and the most
   likely reason B fails while A passes.** WSL-mode specs commonly carry
   `nvidia-cdi-hook update-ldcache` / `create-symlinks`, which run a host
   binary at container start to make the injected libraries resolvable. Arm B
   replaces that with `LD_LIBRARY_PATH`. If `nvidia-smi` in B cannot find
   `libcuda` while A can, the hook is the missing piece — and "the engine must
   run an OCI hook" is a materially bigger Phase 4 than "add two mounts".

5. **`/dev/dxg` is openable unprivileged — expected, but a hard blocker if
   wrong.** WSL ships it world-read/write in practice. If Step 2's open fails
   as the normal user, nothing rootless can follow; the fix is a host-side
   udev/ACL decision, and the honest verdict is "not feasible without it".

6. **CUDA version skew at Step 5 — medium, and the diagnostic is verbatim.**
   The image's CUDA runtime must not exceed the Windows driver's supported
   CUDA. Choosing the image from Step 1's number avoids it; ignoring that
   yields `CUDA driver version is insufficient…`, which looks like a container
   problem and is not.

7. **No `/dev/nvidia*` nodes at all — certain, benign.** WSL2's shape is
   `/dev/dxg` plus `/usr/lib/wsl/lib`. This is the expected divergence from
   bare metal, not a problem — unless a tool (toolkit, an inference server)
   hard-codes the bare-metal nodes. Record any that do.

8. **The projected library mount is `noexec` or otherwise odd — low.**
   `/usr/lib/wsl/lib` is a Windows-side projection, not a normal directory.
   If Step 2's `findmnt` shows `noexec`, Step 5 fails as a loader error
   (`failed to map segment`), and the condition is a remount — another host
   decision, recorded rather than worked around.

9. **The CDI spec goes stale on every Windows driver update — certain over
   time, a precondition rather than a failure.** `/usr/lib/wsl/lib` is a
   projection of the installed driver; the generated spec pins its contents.
   After a driver update: regenerate the spec, re-run Steps 1, 2 and 5. This
   is the "must be redone after a driver update" item — write it down as the
   maintenance cost it is.

---

## What to record — the preconditions list

The verdict is only as good as its preconditions. Every one of these goes in
the report, verbatim where it is an output:

- Windows: driver version **and date**, `winver` build, `wsl --version` (all
  lines), `.wslconfig` contents, distro release (`/etc/os-release`).
- Kernel: `uname -r`; `DXGKRNL` config line; `/dev/dxg` mode/owner/major:minor.
- Our stack: `ctr version` (client + server), `runc --version`,
  `systemctl --user status containerd-rootless.service` (one line),
  `findmnt -no PROPAGATION /` (F-128 — must be `shared`).
- Third-party, **hand-installed** (each a decision still to make):
  `nvidia-container-toolkit` version + the repo line added; `nerdctl` version
  + where it came from. Nothing else outside the Ubuntu archive.
- Root-once steps taken: the toolkit install; writing `/etc/cdi/*`. Anything
  `sudo` touched, listed.
- The CDI spec itself (`/etc/cdi/nvidia-wsl.yaml`, attached), and which mode
  auto-detect chose.
- Images used, **with digests**: the `-base` and `-devel` tags and their
  CUDA versions, against Step 1's driver CUDA version.
- **What must be redone after a Windows driver update:** regenerate the CDI
  spec; re-run Steps 1, 2, 5. (Prediction 9.)
- Every command's exit status, not just its output.

---

## The verdict

One of three, with the commands and outputs behind it:

- **Feasible** — Steps 1–5 all pass, and B2 (bind mounts + env) is enough.
  Name the exact spec delta (Arm C). Phase 2 may begin.
- **Feasible with caveats** — it works, but only under a condition. **Name
  the condition** exactly: "only through nerdctl", "needs a `linux.devices`
  entry", "needs the `update-ldcache` hook", "needs a device-cgroup allow
  rule", "only with `cudaMalloc`, not managed memory", "only at driver ≥ X".
  Each caveat is a Phase-4 requirement or a reason to stop; say which.
- **Not feasible on this stack** — say at which step, with the verbatim
  error, what was tried, and what *would* have to change (host-side ACL, a
  different runtime, a Docker-shaped path we cannot take). That verdict is a
  legitimate outcome of a spike, and a better one than a wishful pass.

State plainly what could **not** be verified. Steps skipped, arms not run,
controls not available (the host-side compute control, above) — listed, not
smoothed.

---

## Recording divergences

One entry per divergence, in order encountered — the same template as the
WSL2 spike, because it is the part that earned its keep there:

```
### <n>. <one-line title>
step:      <which step / arm, which command>
expected:  <the prediction, or the bare-metal / documented behaviour>
observed:  <verbatim output — paste, don't paraphrase — including exit status>
class:     WSL2 semantics | rootless semantics | toolkit gap | our-path gap | docs gap | blocker (best guess)
```

"Our-path gap" is the class this spike exists to find: something the
documented path (Arm A) does that our spec cannot express. A step that passed
but needed unwritten knowledge gets an entry too, class `docs gap`. The spike
report is the full list plus the `tee` logs; the Phase-4 scope — if there is
one — is derived from it and nothing else.

**Not in scope, on purpose:** any change to `setup_host.sh`, the engine, the
base image or containerd's configuration; model choice; agent choice; how
weights are stored or travel in a bundle; sharing one GPU across sessions.

---

## Verdict — Phase 1: FEASIBLE (2026-09-04, Product Owner, on the WSL2 box)

**Arm B2 works — bind mounts and env only.** `CUDA COMPUTE PASS (n=1048576,
device=NVIDIA GeForce RTX 3070)`, managed memory included, in a rootless
container on our own containerd, through our own path. **The exact delta (Arm
C), and nothing else:**

```
bind  /dev/dxg                                          rw
bind  /usr/lib/wsl/lib                                  ro
bind  /usr/lib/wsl/drivers/nv_dispi.inf_amd64_<hash>    ro
env   LD_LIBRARY_PATH=/usr/lib/wsl/lib:<that driver-store path>
```

No device entries, no hooks, no cgroup rules. That is entirely within what
`ContainerSpec` expresses today — Phase 4, if it comes, is small.

**Predictions, scored.** #1 confirmed (`ctr` has no CDI flag; CDI is the
client's job). #2 moot — B2 sufficed; no device-cgroup refusal was observed.
#3 benign — auto-detect chose `wsl`, the forced-mode spec was byte-identical.
#4 fired in the spec (two `createContainer` hooks) **but did not matter** —
`LD_LIBRARY_PATH` substituted for them successfully. #5 dead — `/dev/dxg` is
mode 666 and opens inside rootlesskit. #6 did not fire. #7 confirmed benign.
#8 dead — `/usr/lib/wsl/lib` is not even a separate mount. #9 stands, and is
**sharpened** below.

**Unpredicted — two docs gaps in this document itself, each costing a cycle,
both fixed above in the same commit as this verdict:**

1. Arm B's commands ran bare `ctr run`, which fails on the overlay mount;
   PREREQUISITES.md documents the `nsenter` wrapper, and the runbook omitted
   it. Now defined once as `CTR` at the top of Step 4.
2. `nvidia-smi` is not on `PATH` inside the CUDA image — it lives in the driver
   store at the hashed path. Now invoked by full path, and the driver store is
   a third bind the first draft did not have.

**New precondition, the sharpened #9:** the driver-store directory name
carries a **hash that changes on every Windows driver update**. Phase 1 lives
with `ls -d /usr/lib/wsl/drivers/nv_dispi.inf_amd64_*`. **Phase 4 must discover
that path at start time — never hardcode it** — and the CDI-spec-goes-stale
item from the preconditions list now has a concrete mechanism.

**Not verified, stated:** a host-side CUDA compute control (needs the toolkit
inside WSL2 — deliberately not installed). The two-arm comparison served as
the control, as the doc said it would.

Phase 2 — Ollama in a container, by hand — is `docs/gpu-phase2-ollama.md`.
