//! F-78 diagnosis harness: does `stop_task` ever fail to reap a SIGKILLed task
//! within its 10s window, and if so, WHY?
//!
//! SIGKILL is uncatchable, so a task surviving it is not running — it is
//! blocked, almost certainly in uninterruptible I/O (`D` state), OR it died and
//! something downstream (the shim's reaping, the Wait RPC) lost track of it. A
//! `D`-state init process with its kernel stack settles the question; a zombie
//! (`Z`) or a vanished PID with a slow Wait RPC points the other way.
//!
//! Bounded by construction (an unbounded loop on a 1-in-20 event filled the disk
//! last time): stops at NEMR_F78_RUNS iterations (default 50) or
//! NEMR_F78_SECONDS wall-clock (default 7200 = 2h), whichever comes first.
//! Every iteration's result is flushed to the results file immediately, and any
//! slow-kill capture is written to its own timestamped file, so the run that
//! matters cannot be swallowed if the harness later dies.
//!
//! Reproduces the exact `proc_06` scenario the failure came from: a PID 1 that
//! ignores SIGTERM, driven through the real `stop_task`. Disposable containers
//! only; each is deleted and its snapshot removed before the next.

use nemr_containerd::client::ContainerdClient;
use nemr_containerd::containers::ContainerSpec;
use std::fs;
use std::io::Write;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let runs: u32 = env_or("NEMR_F78_RUNS", 50);
    let max_secs: u64 = env_or("NEMR_F78_SECONDS", 7200);
    let out_dir = std::env::var("NEMR_F78_OUT").unwrap_or_else(|_| "/tmp/f78".to_string());
    fs::create_dir_all(&out_dir).expect("create out dir");
    let results_path = format!("{out_dir}/results.tsv");
    let mut results = fs::File::create(&results_path).expect("results file");
    writeln!(
        results,
        "iter\tkill_latency_ms\toutcome\tinit_final_state\tnote"
    )
    .unwrap();
    results.flush().unwrap();

    let client = ContainerdClient::connect()
        .await
        .expect("connect to containerd");
    let pid = std::process::id();
    let started = Instant::now();

    let mut slow = 0u32;
    let mut max_latency = 0u128;
    let mut latencies: Vec<u128> = Vec::new();

    // Optional CPU load: the failure appeared under a loaded host, and the
    // hypothesised mechanism (the shim's reaping / Wait RPC delayed by
    // scheduling contention) is exactly what CPU pressure aggravates. Each
    // spinner is a busy thread; they are stopped when the run ends.
    let load: usize = env_or("NEMR_F78_LOAD", 0usize);
    let load_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut spinners = Vec::new();
    for _ in 0..load {
        let s = load_stop.clone();
        spinners.push(std::thread::spawn(move || {
            let mut x: u64 = 0;
            while !s.load(std::sync::atomic::Ordering::Relaxed) {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                std::hint::black_box(x);
            }
        }));
    }

    eprintln!(
        "[f78] bounded run: {runs} iterations OR {max_secs}s, whichever first.          load={load} spinners. out={out_dir}"
    );

    for i in 0..runs {
        if started.elapsed().as_secs() >= max_secs {
            eprintln!("[f78] time budget reached at iteration {i}; stopping.");
            break;
        }

        let name = format!("f78probe-{pid}-{i}");
        let id = format!("nemr-{name}");

        let spec = ContainerSpec {
            own_network_namespace: true,
            id: id.clone(),
            image: std::env::var("NEMR_F78_IMAGE")
                .unwrap_or_else(|_| "ghcr.io/gnrain/nemr-base:0.2.0".to_string()),
            mounts: vec![],
            working_dir: None,
            extra_env: vec![],
            // PID 1 that ignores SIGTERM, exactly as proc_06 does.
            args: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "trap '' TERM; while :; do sleep 1; done".to_string(),
            ]),
            cgroup_name: Some(name.clone()),
            cgroup_prefix: std::env::var("NEMR_F78_CGROUP_PREFIX")
                .unwrap_or_else(|_| "nemr".to_string()),
            labels: Default::default(),
        };

        if let Err(e) = client.create_container(&spec).await {
            eprintln!("[f78] iter {i}: create failed: {e:#}; skipping");
            continue;
        }
        let host_pid = match client.start_task(&id).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[f78] iter {i}: start failed: {e:#}; cleaning up");
                let _ = client.delete_container(&id).await;
                let _ = client.remove_snapshot(&id).await;
                continue;
            }
        };

        // Monitor the init process's /proc state across the whole stop, in
        // parallel. It needs to know WHEN the kill window opens so it can tell a
        // startup-time D (runc execing into the payload — normal, fast) from a
        // kill-time D (the F-78-relevant one). SIGKILL lands ~grace (5s) after
        // stop_task begins; the shared `kill_at` is set the instant before the
        // call so the monitor can classify.
        let monitor_out = out_dir.clone();
        let monitor_i = i;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_m = stop.clone();
        let kill_window = std::sync::Arc::new(std::sync::Mutex::new(None::<Instant>));
        let kill_window_m = kill_window.clone();
        let monitor = std::thread::spawn(move || {
            monitor_proc(host_pid, monitor_i, &monitor_out, stop_m, kill_window_m)
        });

        *kill_window.lock().unwrap() = Some(Instant::now());
        let t0 = Instant::now();
        let result = client.stop_task(&id).await;
        let latency_ms = t0.elapsed().as_millis();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let final_state = monitor.join().unwrap_or_else(|_| "monitor-panic".into());

        // The kill latency is (total stop time - grace), i.e. the time from
        // SIGKILL to reap. Grace is 5s; stop_task waits it out because SIGTERM
        // is ignored.
        let kill_latency = latency_ms.saturating_sub(5000);
        latencies.push(kill_latency);
        max_latency = max_latency.max(kill_latency);

        // Whether the payload pid is already gone tells the two mechanisms
        // apart: gone + slow means the delay is in the shim's reaping or the
        // Wait RPC, not a payload that would not die. Present + slow means the
        // payload itself is stuck.
        let payload_gone = read_state(host_pid).is_none();

        let (outcome, note) = match &result {
            Ok(o) => (format!("{o:?}"), String::new()),
            Err(e) => {
                slow += 1;
                let msg = format!("{e:#}").replace('\t', " ").replace('\n', " ");
                // The actual F-78 failure. Capture the SHIM and the whole tree,
                // since the payload is likely already gone.
                capture_slow_kill(host_pid, i, &out_dir, payload_gone, "TIMEOUT");
                ("ERROR".to_string(), msg)
            }
        };

        // Also capture a merely-SLOW kill (over 2s but under the 10s timeout):
        // it is the same mechanism, milder, and far more frequent, so it is the
        // realistic way to characterise what drives the tail.
        if result.is_ok() && kill_latency > 2000 {
            capture_slow_kill(host_pid, i, &out_dir, payload_gone, "SLOW");
        }

        writeln!(
            results,
            "{i}\t{kill_latency}\t{outcome}\t{final_state}\t{note}"
        )
        .unwrap();
        results.flush().unwrap();

        if kill_latency > 1000 || result.is_err() {
            eprintln!(
                "[f78] iter {i}: SLOW kill_latency={kill_latency}ms outcome={outcome} \
                 init_final={final_state} {note}"
            );
        }

        // Disposable: delete and sweep before the next.
        let _ = client.delete_container(&id).await;
        let _ = client.remove_snapshot(&id).await;
    }

    load_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for sp in spinners {
        let _ = sp.join();
    }

    // Summary.
    latencies.sort_unstable();
    let n = latencies.len();
    let median = if n > 0 { latencies[n / 2] } else { 0 };
    let p95 = if n > 0 {
        latencies[(n * 95 / 100).min(n - 1)]
    } else {
        0
    };
    eprintln!(
        "\n[f78] DONE: {n} runs, {slow} slow/failed. kill_latency ms: median={median} \
         p95={p95} max={max_latency}. results={results_path}"
    );
    if slow == 0 {
        eprintln!("[f78] not reproduced in {n} runs — a clean measurement is a result.");
    }
}

/// Poll /proc/<pid>/status until told to stop; return the last observed State
/// letter, and note if we ever saw D (uninterruptible) or Z (zombie).
fn monitor_proc(
    pid: u32,
    iter: u32,
    out_dir: &str,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    kill_window: std::sync::Arc<std::sync::Mutex<Option<Instant>>>,
) -> String {
    // The kill window opens ~grace (5s) after stop_task begins, when SIGKILL is
    // sent. A D observed before that is startup/grace (runc exec — normal). A D
    // observed after it is the F-78 candidate: an uninterruptible wait that
    // SIGKILL cannot cut through.
    const GRACE_MS: u128 = 5000;
    let mut saw_d_startup = false;
    let mut saw_d_kill = false;
    let mut saw_z = false;
    let mut last = "?".to_string();
    let mut captured_kill_d = false;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match read_state(pid) {
            Some(state) => {
                last = state.clone();
                if state.starts_with('D') {
                    let in_kill_window = kill_window
                        .lock()
                        .unwrap()
                        .map(|start| start.elapsed().as_millis() >= GRACE_MS)
                        .unwrap_or(false);
                    if in_kill_window {
                        saw_d_kill = true;
                        // This is the one that matters — capture it, once.
                        if !captured_kill_d {
                            captured_kill_d = true;
                            capture_full_state(pid, iter, out_dir);
                        }
                    } else {
                        saw_d_startup = true;
                    }
                }
                if state.starts_with('Z') {
                    saw_z = true;
                }
            }
            None => {
                last = "gone".to_string();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut flags = String::new();
    if saw_d_kill {
        flags.push_str("+Dkill");
    }
    if saw_d_startup {
        flags.push_str("+Dstart");
    }
    if saw_z {
        flags.push_str("+Z");
    }
    format!("{last}{flags}")
}

fn read_state(pid: u32) -> Option<String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("State:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Everything that distinguishes "blocked on I/O" from "reaped but the RPC is
/// slow": the process State, its kernel stack and wchan, the shim, dmesg.
fn capture_full_state(pid: u32, iter: u32, out_dir: &str) {
    let path = format!("{out_dir}/capture-iter{iter}-pid{pid}.txt");
    let mut buf = String::new();
    let grab = |p: &str| fs::read_to_string(p).unwrap_or_else(|e| format!("<unreadable: {e}>"));

    buf.push_str(&format!("=== /proc/{pid}/status ===\n"));
    buf.push_str(&grab(&format!("/proc/{pid}/status")));
    buf.push_str(&format!("\n=== /proc/{pid}/wchan ===\n"));
    buf.push_str(&grab(&format!("/proc/{pid}/wchan")));
    buf.push_str(&format!(
        "\n\n=== /proc/{pid}/stack (kernel stack — the D-state answer) ===\n"
    ));
    buf.push_str(&grab(&format!("/proc/{pid}/stack")));
    buf.push_str("\n=== ps: containerd-shim + container children ===\n");
    if let Ok(out) = std::process::Command::new("ps")
        .args(["-eo", "pid,ppid,stat,wchan:32,comm"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("shim") || line.contains("sleep") || line.contains("PID") {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }
    buf.push_str("\n=== dmesg tail (hung task / I/O errors) ===\n");
    if let Ok(out) = std::process::Command::new("dmesg").arg("--ctime").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            buf.push_str(line);
            buf.push('\n');
        }
    } else {
        buf.push_str("<dmesg needs privilege; run harness with sudo -E to capture>\n");
    }

    let _ = fs::write(&path, buf);
    eprintln!("[f78] captured state -> {path}");
}

/// Capture the shim and the container process tree on a slow or failed kill.
/// The payload pid is likely already gone, so the useful state is the
/// containerd-shim (which owns the Wait RPC and the reaping) and any lingering
/// container process.
fn capture_slow_kill(payload_pid: u32, iter: u32, out_dir: &str, payload_gone: bool, kind: &str) {
    let path = format!("{out_dir}/slowkill-{kind}-iter{iter}.txt");
    let mut buf = String::new();
    buf.push_str(&format!(
        "kind: {kind}
iter: {iter}
payload_pid: {payload_pid}
"
    ));
    buf.push_str(&format!(
        "payload_gone_at_capture: {payload_gone}

"
    ));

    // Every containerd-shim and any runc/sh/sleep, with state and wchan — the
    // state of the machinery that reaps and answers Wait.
    buf.push_str(
        "=== ps: shims + runc + container payloads (STAT, WCHAN) ===
",
    );
    if let Ok(out) = std::process::Command::new("ps")
        .args(["-eo", "pid,ppid,stat,wchan:24,comm"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("PID")
                || line.contains("shim")
                || line.contains("runc")
                || line.contains("sleep")
                || line.contains("containerd")
            {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }

    // For each shim in D-state, its kernel wchan (needs no privilege).
    buf.push_str("\n=== shim /proc state (D = the machinery itself is blocked) ===\n");
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid_s) = name.to_str() else { continue };
            let Ok(_pid) = pid_s.parse::<u32>() else {
                continue;
            };
            let comm = fs::read_to_string(format!("/proc/{pid_s}/comm")).unwrap_or_default();
            if !comm.contains("shim") {
                continue;
            }
            let state = read_state(pid_s.parse().unwrap()).unwrap_or_else(|| "?".into());
            let wchan = fs::read_to_string(format!("/proc/{pid_s}/wchan")).unwrap_or_default();
            buf.push_str(&format!(
                "  shim pid {pid_s}: state={state} wchan={wchan}\n"
            ));
        }
    }

    buf.push_str("\n=== dmesg tail (hung_task_timeout, I/O errors) ===\n");
    match std::process::Command::new("dmesg").arg("--ctime").output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text
                .lines()
                .rev()
                .take(15)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                buf.push_str(line);
                buf.push('\n');
            }
        }
        _ => buf.push_str("<dmesg needs privilege; re-run with sudo -E for hung-task lines>\n"),
    }

    let _ = fs::write(&path, &buf);
    eprintln!("[f78] {kind} kill captured -> {path}");
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
