//! System health monitoring — alpha.41.
//!
//! Today's gap: nothing tells the operator that CPU/RAM is heading
//! into "everything is about to fall over" territory. 4 concurrent
//! FFmpeg encoders + 4 NDI/OMT receivers + the DeckLink driver
//! overhead is heavy; Windows OOM doesn't kill gracefully (the
//! system becomes unresponsive while paging, then maybe never
//! recovers without a hard reset). Pre-show this is fine; mid-show
//! it loses you the broadcast.
//!
//! Fix: a background task polls CPU% + memory% every 1.5s via the
//! sysinfo crate, computes a Green/Yellow/Red health classification,
//! and exposes it via /api/system-health for the multiview header
//! to render as a traffic-light pill with a tooltip.
//!
//! Thresholds (subject to tuning from operator feedback):
//!   - Green:  CPU <70%, mem <70%
//!   - Yellow: CPU 70-90%, mem 70-85%, OR brief CPU spike >=90%
//!     (1-2 ticks)
//!   - Red:    CPU >=90% for 3+ consecutive ticks (4.5s sustained),
//!     OR mem >=85% (no streak — memory pressure doesn't recover
//!     on its own the way CPU does)
//!
//! The Red threshold is intentionally sticky-by-streak on the CPU
//! side because momentary 100% spikes happen during normal encoder
//! warm-up — calling those Red would constantly oscillate. Memory
//! has no such grace because once the system is paging, you're
//! already in trouble.
//!
//! Future extensions (not in this alpha): per-FFmpeg-PID CPU usage,
//! GPU utilization (NVML for NVIDIA, harder for AMD/Intel), thermal
//! sensor readings, encoder-session-count for NVENC (consumer 8-cap).

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::System;

#[derive(Clone, Copy, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Green,
    Yellow,
    Red,
    /// Returned during the first ~1s after boot before sysinfo has
    /// enough samples to compute a meaningful CPU%. UI renders this
    /// as a neutral pill ("Warming up…") so the operator knows the
    /// monitor is live but hasn't decided yet.
    Unknown,
}

#[derive(Clone, Serialize, Debug)]
pub struct SystemHealth {
    pub cpu_percent: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub mem_percent: f32,
    pub status: HealthStatus,
    /// Human-readable reason when not Green. None when Green.
    pub warning: Option<String>,
    /// Unix epoch seconds when this snapshot was computed.
    /// UI uses this to detect a stalled monitor task (if the value
    /// stops advancing, something killed the polling loop).
    pub updated_at: f64,
    /// Number of CPU cores. Useful context for the operator
    /// interpreting "system load".
    pub cpu_count: u32,
}

impl Default for SystemHealth {
    fn default() -> Self {
        Self {
            cpu_percent: 0.0,
            mem_used_mb: 0,
            mem_total_mb: 0,
            mem_percent: 0.0,
            status: HealthStatus::Unknown,
            warning: None,
            updated_at: 0.0,
            cpu_count: 0,
        }
    }
}

pub struct SystemMonitor {
    health: Arc<Mutex<SystemHealth>>,
}

impl SystemMonitor {
    /// Start the background polling task. Returns an Arc<Self> that
    /// HTTP handlers + Tauri commands can clone to read the latest
    /// snapshot via `.snapshot()`.
    ///
    /// alpha.47 fix: the original alpha.41 implementation called
    /// `tokio::spawn` directly, which panics when invoked from outside
    /// a running Tokio runtime — exactly the situation in Tauri's
    /// synchronous setup() hook on Windows. On macOS the same code
    /// happened to be invoked after the runtime was already pinned to
    /// the main thread so the panic didn't surface (which is why
    /// alpha.41-.46 shipped clean on Mac but every Windows install
    /// crashed at startup with
    ///   thread 'main' panicked at src\system_monitor.rs:97:9:
    ///   there is no reactor running, must be called from the
    ///   context of a Tokio 1.x runtime
    /// Switch to `tauri::async_runtime::spawn` which goes through
    /// Tauri's managed runtime regardless of the caller's context.
    pub fn start() -> Arc<Self> {
        let health = Arc::new(Mutex::new(SystemHealth::default()));
        let health_for_task = health.clone();

        tauri::async_runtime::spawn(async move {
            // sysinfo needs an initial sample before CPU usage is
            // meaningful (it computes deltas). Take one, sleep
            // briefly, then enter the steady loop.
            let mut sys = System::new();
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            tokio::time::sleep(Duration::from_millis(600)).await;

            let mut high_cpu_streak: u32 = 0;
            let mut tick = tokio::time::interval(Duration::from_millis(1500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tick.tick().await;
                sys.refresh_cpu_usage();
                sys.refresh_memory();

                let cpu = global_cpu_avg(&sys);
                let mem_used = sys.used_memory();
                let mem_total = sys.total_memory().max(1);
                let mem_percent = (mem_used as f64 / mem_total as f64 * 100.0) as f32;
                let cpu_count = sys.cpus().len() as u32;

                let (status, warning) = compute_status(cpu, mem_percent, &mut high_cpu_streak);

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);

                let snap = SystemHealth {
                    cpu_percent: cpu,
                    mem_used_mb: mem_used / (1024 * 1024),
                    mem_total_mb: mem_total / (1024 * 1024),
                    mem_percent,
                    status,
                    warning,
                    updated_at: now,
                    cpu_count,
                };

                if let Ok(mut guard) = health_for_task.lock() {
                    *guard = snap;
                }
            }
        });

        Arc::new(Self { health })
    }

    pub fn snapshot(&self) -> SystemHealth {
        self.health
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

/// Average CPU usage across all cores. sysinfo exposes per-core
/// `cpu_usage()` on each `Cpu`; we average them so the operator
/// sees one number that matches their intuition. Doing it manually
/// rather than relying on `global_cpu_info` because that API has
/// shifted across sysinfo versions (0.29 vs 0.30 vs 0.32) — the
/// per-core average is stable.
fn global_cpu_avg(sys: &System) -> f32 {
    let cpus = sys.cpus();
    if cpus.is_empty() {
        return 0.0;
    }
    let total: f32 = cpus.iter().map(|c| c.cpu_usage()).sum();
    total / cpus.len() as f32
}

/// Classify (cpu, mem) into a HealthStatus. Mutates `streak` to
/// track sustained-high-CPU across consecutive calls — the caller
/// owns the persistent counter so this remains a pure function
/// per-call but accumulates state across the polling loop.
fn compute_status(
    cpu: f32,
    mem: f32,
    streak: &mut u32,
) -> (HealthStatus, Option<String>) {
    const CPU_RED_PCT: f32 = 90.0;
    const CPU_YELLOW_PCT: f32 = 70.0;
    const CPU_RED_STREAK: u32 = 3; // ~4.5s at 1.5s tick
    const MEM_RED_PCT: f32 = 85.0;
    const MEM_YELLOW_PCT: f32 = 70.0;

    if cpu >= CPU_RED_PCT {
        *streak += 1;
    } else {
        *streak = 0;
    }

    // Memory red is sticky — no streak grace because page-pressure
    // doesn't unstick on its own.
    if mem >= MEM_RED_PCT {
        return (
            HealthStatus::Red,
            Some(format!(
                "Memory at {:.0}% — risk of system unresponsive",
                mem
            )),
        );
    }
    if *streak >= CPU_RED_STREAK {
        return (
            HealthStatus::Red,
            Some(format!(
                "CPU sustained at {:.0}% — encoder frame drops imminent",
                cpu
            )),
        );
    }
    if cpu >= CPU_RED_PCT {
        // Brief spike — yellow not red until it sustains.
        return (
            HealthStatus::Yellow,
            Some(format!("CPU spike at {:.0}%", cpu)),
        );
    }
    if cpu >= CPU_YELLOW_PCT {
        return (HealthStatus::Yellow, Some(format!("CPU at {:.0}%", cpu)));
    }
    if mem >= MEM_YELLOW_PCT {
        return (HealthStatus::Yellow, Some(format!("Memory at {:.0}%", mem)));
    }
    (HealthStatus::Green, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_at_low_load() {
        let mut streak = 0;
        let (s, w) = compute_status(20.0, 30.0, &mut streak);
        assert_eq!(s, HealthStatus::Green);
        assert!(w.is_none());
    }

    #[test]
    fn yellow_at_moderate_cpu() {
        let mut streak = 0;
        let (s, _) = compute_status(75.0, 30.0, &mut streak);
        assert_eq!(s, HealthStatus::Yellow);
    }

    #[test]
    fn yellow_at_brief_cpu_spike() {
        let mut streak = 0;
        let (s, _) = compute_status(95.0, 30.0, &mut streak);
        assert_eq!(s, HealthStatus::Yellow);
        assert_eq!(streak, 1);
    }

    #[test]
    fn red_after_sustained_cpu() {
        let mut streak = 0;
        for _ in 0..3 {
            let _ = compute_status(95.0, 30.0, &mut streak);
        }
        let (s, _) = compute_status(95.0, 30.0, &mut streak);
        assert_eq!(s, HealthStatus::Red);
    }

    #[test]
    fn red_immediately_at_high_memory() {
        let mut streak = 0;
        let (s, _) = compute_status(50.0, 90.0, &mut streak);
        assert_eq!(s, HealthStatus::Red);
    }

    #[test]
    fn cpu_streak_resets_after_drop() {
        let mut streak = 0;
        let _ = compute_status(95.0, 30.0, &mut streak);
        let _ = compute_status(95.0, 30.0, &mut streak);
        assert_eq!(streak, 2);
        let _ = compute_status(40.0, 30.0, &mut streak);
        assert_eq!(streak, 0);
    }
}
