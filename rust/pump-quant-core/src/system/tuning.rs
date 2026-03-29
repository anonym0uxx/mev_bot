//! Bare-metal system optimizations for minimum-latency trading.
//!
//! Called once at daemon startup. All operations are best-effort —
//! the daemon starts even if tuning fails (e.g. missing CAP_SYS_NICE,
//! insufficient permissions for mlockall, etc.).

use std::os::unix::io::RawFd;

// ── CPU Pinning ──────────────────────────────────────────────────────

/// Pin the calling thread to a specific CPU core.
///
/// Uses `sched_setaffinity(2)` to restrict the calling thread to
/// exactly one core, eliminating cross-core migration and L1/L2
/// cache thrashing on the hot path.
///
/// Returns `Ok(core_id)` on success, `Err` with OS error on failure.
pub fn pin_thread_to_core(core_id: usize) -> Result<usize, String> {
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut cpuset);
        libc::CPU_SET(core_id, &mut cpuset);
        let ret = libc::sched_setaffinity(
            0,
            std::mem::size_of::<libc::cpu_set_t>(),
            &cpuset,
        );
        if ret == 0 {
            Ok(core_id)
        } else {
            Err(format!(
                "sched_setaffinity failed for core {}: {}",
                core_id,
                std::io::Error::last_os_error()
            ))
        }
    }
}

// ── Thread Priority (Real-time scheduling) ───────────────────────────

/// Set the calling thread to `SCHED_FIFO` with the given priority (1–99).
///
/// Requires `CAP_SYS_NICE` or root. Higher priority values preempt lower
/// ones. Typical assignment:
/// - Hot-path thread: 50
/// - Feed thread: 49
/// - Bundle/tx thread: 48
///
/// Returns `Err` if the kernel rejects the request (e.g. no capability).
pub fn set_realtime_priority(priority: i32) -> Result<(), String> {
    unsafe {
        let param = libc::sched_param {
            sched_priority: priority,
        };
        let ret = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
        if ret == 0 {
            Ok(())
        } else {
            Err(format!(
                "sched_setscheduler FIFO prio={} failed: {}",
                priority,
                std::io::Error::last_os_error()
            ))
        }
    }
}

// ── Socket Tuning ────────────────────────────────────────────────────

/// Apply low-latency socket options to a raw file descriptor.
///
/// Options applied:
/// - `TCP_NODELAY` — disable Nagle's algorithm
/// - `SO_KEEPALIVE` — detect dead peers
/// - `SO_RCVBUF` 4 MiB — large receive buffer for bursts
/// - `SO_SNDBUF` 1 MiB — adequate send buffer
/// - `SO_PRIORITY` (optional) — QoS traffic class
///
/// Returns a list of `(option_name, result)` so callers can log
/// per-option success/failure without aborting on partial failure.
pub fn tune_socket(fd: RawFd, priority: Option<i32>) -> Vec<(String, Result<(), String>)> {
    let mut results = Vec::new();

    // TCP_NODELAY
    results.push((
        "TCP_NODELAY".into(),
        set_sockopt_int(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, 1),
    ));

    // SO_KEEPALIVE
    results.push((
        "SO_KEEPALIVE".into(),
        set_sockopt_int(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1),
    ));

    // SO_RCVBUF 4 MiB
    results.push((
        "SO_RCVBUF".into(),
        set_sockopt_int(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, 4 * 1024 * 1024),
    ));

    // SO_SNDBUF 1 MiB
    results.push((
        "SO_SNDBUF".into(),
        set_sockopt_int(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, 1024 * 1024),
    ));

    // SO_PRIORITY (if requested)
    if let Some(prio) = priority {
        results.push((
            "SO_PRIORITY".into(),
            set_sockopt_int(fd, libc::SOL_SOCKET, libc::SO_PRIORITY, prio),
        ));
    }

    results
}

fn set_sockopt_int(fd: RawFd, level: i32, optname: i32, value: i32) -> Result<(), String> {
    unsafe {
        let ret = libc::setsockopt(
            fd,
            level,
            optname,
            &value as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
}

// ── Memory Optimizations ─────────────────────────────────────────────

/// Lock all current and future pages into physical memory.
///
/// Prevents page faults on the hot path by keeping everything resident.
/// Requires `CAP_IPC_LOCK` or sufficient `RLIMIT_MEMLOCK`.
pub fn mlock_stack() -> Result<(), String> {
    unsafe {
        let ret = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
        if ret == 0 {
            Ok(())
        } else {
            Err(format!(
                "mlockall failed: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
}

/// Pre-fault stack/heap pages by writing to every page boundary.
///
/// This forces the kernel to allocate and map physical pages now,
/// rather than on-demand during latency-critical processing.
pub fn prefault_stack(size_bytes: usize) {
    let mut buf = vec![0u8; size_bytes];
    for i in (0..size_bytes).step_by(4096) {
        unsafe {
            std::ptr::write_volatile(&mut buf[i], 1);
        }
    }
}

// ── Startup Orchestration ────────────────────────────────────────────

/// Configuration for system-level tuning at daemon startup.
#[derive(Debug, Clone)]
pub struct TuningConfig {
    /// Enable CPU core pinning for hot-path, feed, and bundle threads.
    pub enable_cpu_pinning: bool,
    /// Enable SCHED_FIFO real-time scheduling.
    pub enable_realtime_sched: bool,
    /// Enable mlockall to prevent page faults.
    pub enable_mlock: bool,
    /// CPU core for the hot-path (scoring → entry) thread.
    pub hot_path_core: usize,
    /// CPU core for the feed ingestion thread.
    pub feed_core: usize,
    /// CPU core for the bundle/tx submission thread.
    pub bundle_core: usize,
    /// SCHED_FIFO priority for the hot-path thread (1–99).
    pub hot_path_priority: i32,
    /// SCHED_FIFO priority for the feed thread (1–99).
    pub feed_priority: i32,
    /// SCHED_FIFO priority for the bundle/tx thread (1–99).
    pub bundle_priority: i32,
    /// Bytes to pre-fault at startup (default 1 MiB).
    pub prefault_bytes: usize,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            enable_cpu_pinning: true,
            enable_realtime_sched: true,
            enable_mlock: true,
            hot_path_core: 1,
            feed_core: 0,
            bundle_core: 2,
            hot_path_priority: 50,
            feed_priority: 49,
            bundle_priority: 48,
            prefault_bytes: 1024 * 1024, // 1 MiB
        }
    }
}

/// Summary of all tuning actions attempted at startup.
pub struct TuningReport {
    pub actions: Vec<(String, Result<(), String>)>,
}

impl TuningReport {
    /// Log a human-readable summary of all tuning actions.
    pub fn log_summary(&self) {
        for (name, result) in &self.actions {
            match result {
                Ok(()) => tracing::info!("✅ {name}"),
                Err(e) => tracing::warn!("⚠️ {name}: {e}"),
            }
        }
        let ok = self.actions.iter().filter(|(_, r)| r.is_ok()).count();
        let total = self.actions.len();
        tracing::info!(
            "System tuning: {ok}/{total} optimizations applied successfully"
        );
    }

    /// Returns true if all actions succeeded.
    pub fn all_ok(&self) -> bool {
        self.actions.iter().all(|(_, r)| r.is_ok())
    }

    /// Count of failed actions.
    pub fn failure_count(&self) -> usize {
        self.actions.iter().filter(|(_, r)| r.is_err()).count()
    }
}

/// Apply all system tuning. Call once at daemon startup.
///
/// Best-effort — the daemon starts even if individual tuning steps
/// fail. Check [`TuningReport`] to see what succeeded/failed and
/// call [`TuningReport::log_summary`] to emit structured logs.
pub fn apply_tuning(config: &TuningConfig) -> TuningReport {
    let mut actions = Vec::new();

    // CPU pinning (current thread only — caller is expected to call
    // this from each thread, or use apply_tuning for the hot-path
    // thread and tune others separately).
    if config.enable_cpu_pinning {
        actions.push((
            format!("cpu_pin hot_path → core {}", config.hot_path_core),
            pin_thread_to_core(config.hot_path_core).map(|_| ()),
        ));
    }

    // Real-time scheduling
    if config.enable_realtime_sched {
        actions.push((
            format!("sched_fifo hot_path prio={}", config.hot_path_priority),
            set_realtime_priority(config.hot_path_priority),
        ));
    }

    // Memory locking
    if config.enable_mlock {
        actions.push(("mlockall".into(), mlock_stack()));
    }

    // Pre-fault pages (always — this is safe and cheap)
    prefault_stack(config.prefault_bytes);
    actions.push((
        format!("prefault {} bytes", config.prefault_bytes),
        Ok(()),
    ));

    TuningReport { actions }
}

// ── Sysctl Recommendations ──────────────────────────────────────────

/// Recommended sysctl settings for the host.
///
/// These should be applied manually or via `/etc/sysctl.d/99-pump-quant.conf`.
/// The daemon does NOT write these automatically — they require root and
/// persist across reboots.
///
/// ```text
/// # /etc/sysctl.d/99-pump-quant.conf
/// net.core.rmem_max = 16777216
/// net.core.wmem_max = 4194304
/// net.ipv4.tcp_low_latency = 1
/// net.ipv4.tcp_fastopen = 3
/// net.core.busy_poll = 50
/// net.core.busy_read = 50
/// ```
pub const SYSCTL_RECOMMENDATIONS: &[(&str, &str)] = &[
    ("net.core.rmem_max", "16777216"),
    ("net.core.wmem_max", "4194304"),
    ("net.ipv4.tcp_low_latency", "1"),
    ("net.ipv4.tcp_fastopen", "3"),
    ("net.core.busy_poll", "50"),
    ("net.core.busy_read", "50"),
];

/// Print sysctl recommendations to stdout for operator review.
pub fn print_sysctl_recommendations() {
    println!("# Recommended sysctl settings for pump-quant");
    println!("# Copy to /etc/sysctl.d/99-pump-quant.conf and run: sysctl -p");
    for (key, value) in SYSCTL_RECOMMENDATIONS {
        println!("{key} = {value}");
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tuning_config_defaults() {
        let config = TuningConfig::default();
        assert!(config.enable_cpu_pinning);
        assert!(config.enable_realtime_sched);
        assert!(config.enable_mlock);
        assert_eq!(config.hot_path_core, 1);
        assert_eq!(config.feed_core, 0);
        assert_eq!(config.bundle_core, 2);
        assert_eq!(config.hot_path_priority, 50);
        assert_eq!(config.feed_priority, 49);
        assert_eq!(config.bundle_priority, 48);
        assert_eq!(config.prefault_bytes, 1024 * 1024);
    }

    #[test]
    fn test_prefault_stack_runs() {
        // Just verify it doesn't panic or crash
        prefault_stack(8192); // 2 pages
        prefault_stack(0); // edge case: zero bytes
        prefault_stack(4096); // exactly one page
    }

    #[test]
    fn test_sysctl_recommendations_not_empty() {
        assert!(!SYSCTL_RECOMMENDATIONS.is_empty());
        // Verify all entries have non-empty key and value
        for (key, value) in SYSCTL_RECOMMENDATIONS {
            assert!(!key.is_empty(), "sysctl key should not be empty");
            assert!(!value.is_empty(), "sysctl value should not be empty");
        }
        // Verify expected entries exist
        let keys: Vec<&str> = SYSCTL_RECOMMENDATIONS.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"net.core.rmem_max"));
        assert!(keys.contains(&"net.ipv4.tcp_fastopen"));
        assert!(keys.contains(&"net.core.busy_poll"));
    }

    #[test]
    fn test_tuning_report_logs() {
        // Build a mock report with mixed success/failure
        let report = TuningReport {
            actions: vec![
                ("cpu_pin core 1".into(), Ok(())),
                ("sched_fifo prio=50".into(), Ok(())),
                (
                    "mlockall".into(),
                    Err("Operation not permitted".into()),
                ),
                ("prefault 1048576 bytes".into(), Ok(())),
            ],
        };

        assert!(!report.all_ok());
        assert_eq!(report.failure_count(), 1);
        assert_eq!(report.actions.len(), 4);

        // Verify the success/failure breakdown
        let successes: Vec<&str> = report
            .actions
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(successes.len(), 3);
        assert!(successes.contains(&"cpu_pin core 1"));

        let failures: Vec<&str> = report
            .actions
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(failures.len(), 1);
        assert!(failures.contains(&"mlockall"));
    }

    #[test]
    fn test_tuning_report_all_ok() {
        let report = TuningReport {
            actions: vec![
                ("a".into(), Ok(())),
                ("b".into(), Ok(())),
            ],
        };
        assert!(report.all_ok());
        assert_eq!(report.failure_count(), 0);
    }

    #[test]
    fn test_tuning_config_clone() {
        let config = TuningConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.hot_path_core, config.hot_path_core);
        assert_eq!(cloned.hot_path_priority, config.hot_path_priority);
    }
}
