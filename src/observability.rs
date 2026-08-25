//! Logging and tracing setup (B3).
//!
//! # What this is for
//!
//! VOL-05 is the reference defect: `start` ran a container against the host root
//! filesystem instead of the project volume, and every message the engine
//! printed said success. It was found after a reboot, by inspecting host state
//! by hand — not from a log, because the log had nothing in it that would have
//! distinguished the two cases.
//!
//! The requirement for this module is therefore falsifiable: **in debug mode,
//! VOL-05 must be obvious on the first run.** That means the volume-mount
//! decision has to log what it checked, what it found, and which device the
//! working directory actually resolves to — not merely that it proceeded.
//!
//! # Levels
//!
//! - **default** (`info`): the audit trail NFR-04 requires — every mount, loop
//!   device attach/detach, and elevated invocation, in plain text on stderr.
//!   This is the operator-facing output and is deliberately readable without
//!   any tooling.
//! - **`NEMR_DEBUG=1`** (`debug`): adds the decision points — mount checks and
//!   their result, resolved paths, backing devices, task and exec lifecycle.
//! - **`NEMR_LOG=<filter>`**: full `tracing-subscriber` env-filter syntax for
//!   anything more specific, e.g. `NEMR_LOG=nemr_containerd=trace,nemr_engine=debug`.

use tracing_subscriber::filter::EnvFilter;

/// Install the process-wide subscriber.
///
/// Called once from the CLI. The library crates never install a subscriber —
/// they only emit spans and events — so the E-09 daemon can render them
/// differently (structured JSON to a log service, say) without either library
/// changing.
pub fn init(verbose: bool) {
    let filter = if let Ok(spec) = std::env::var("NEMR_LOG") {
        EnvFilter::new(spec)
    } else if verbose || std::env::var_os("NEMR_DEBUG").is_some() {
        EnvFilter::new("nemr_engine=debug,nemr_containerd=debug")
    } else {
        // `info` and above. The provisioning trail moved to `debug`, so this is
        // a summary plus the one-line elevation note — not silence. WARN and
        // ERROR are above INFO and therefore unaffected by this choice: quieting
        // the success path cannot quiet the failure path, and
        // `verbose_does_not_gate_warnings_or_errors` asserts it.
        EnvFilter::new("nemr_engine=info,nemr_containerd=info")
    };

    // Deliberately terse: no timestamps, no target, no level for the common
    // case. This output doubles as the user-facing audit trail (NFR-04), and a
    // timestamped, target-prefixed line for every mount would bury the thing an
    // operator is trying to read. Debug output does carry the target, because at
    // that point you are debugging and want to know where a line came from.
    let debug = verbose
        || std::env::var_os("NEMR_DEBUG").is_some()
        || std::env::var_os("NEMR_LOG").is_some();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()));

    if debug {
        builder.with_target(true).with_level(true).init();
    } else {
        builder
            .with_target(false)
            .with_level(false)
            .without_time()
            .init();
    }
}

/// Install the daemon's subscriber: the terse fmt log (the durable NFR-04 audit
/// trail) plus the audit Layer that streams events to clients (E-09).
///
/// The audit Layer carries its OWN filter at `debug`, independent of the fmt
/// log's verbosity, so an elevation (info) is always captured for the stream and
/// a client's `--verbose` can receive the debug trace even when the daemon log
/// is running terse. Emission follows the union of all layers' interest, so the
/// fmt log stays clean while the audit Layer still sees debug.
pub fn init_daemon(verbose: bool, registry: crate::daemon::audit::AuditRegistry) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let fmt_filter = if let Ok(spec) = std::env::var("NEMR_LOG") {
        EnvFilter::new(spec)
    } else if verbose || std::env::var_os("NEMR_DEBUG").is_some() {
        EnvFilter::new("nemr_engine=debug,nemr_containerd=debug")
    } else {
        EnvFilter::new("nemr_engine=info,nemr_containerd=info")
    };
    let debug = verbose
        || std::env::var_os("NEMR_DEBUG").is_some()
        || std::env::var_os("NEMR_LOG").is_some();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(debug)
        .with_level(debug);
    // `without_time` is only available before boxing, so branch on it here.
    let fmt_layer = if debug {
        fmt_layer.boxed()
    } else {
        fmt_layer.without_time().boxed()
    }
    .with_filter(fmt_filter);

    let audit_layer = crate::daemon::audit::AuditLayer::new(registry)
        .with_filter(EnvFilter::new("nemr_engine=debug"));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(audit_layer)
        .init();
}
