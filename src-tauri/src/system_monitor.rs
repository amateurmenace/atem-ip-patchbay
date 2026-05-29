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
use sysinfo::{ProcessRefreshKind, System};

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
    /// Per-core CPU usage. Same order as the OS reports them; UI
    /// renders a small bar grid in the system drawer (alpha.50). On
    /// systems with hyperthreading these are LOGICAL cores.
    pub cpu_per_core: Vec<f32>,
    /// Running FFmpeg processes (from our sidecar or otherwise).
    /// Operators want to see which encoders are eating CPU during a
    /// multiview stream — separates "system is hot because OS" from
    /// "system is hot because FFmpegs". alpha.50 also lets the
    /// drawer cross-reference these PIDs against per-tile streams
    /// in the future, but for now it's just a visibility panel.
    pub ffmpeg_processes: Vec<FfmpegProcess>,
    /// Total swap used. High swap with low free RAM is the
    /// "system is about to thrash" pattern; surfacing it lets the
    /// operator see paging before the broadcast suffers.
    pub swap_used_mb: u64,
    pub swap_total_mb: u64,
}

#[derive(Clone, Serialize, Debug)]
pub struct FfmpegProcess {
    pub pid: u32,
    pub cpu_percent: f32,
    pub mem_mb: u64,
    /// First 40 chars of the cmdline so the operator can tell
    /// streaming-FFmpeg apart from the OMT-tee-FFmpeg or any other
    /// they might have running.
    pub cmd_excerpt: String,
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
            cpu_per_core: Vec::new(),
            ffmpeg_processes: Vec::new(),
            swap_used_mb: 0,
            swap_total_mb: 0,
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
                // alpha.50: also refresh process info so we can list
                // FFmpeg PIDs + per-process CPU. ProcessRefreshKind
                // with .with_cpu()/.with_memory() keeps the refresh
                // scoped to what we need — full process refresh would
                // be heavier than necessary at 1.5s cadence.
                sys.refresh_processes_specifics(
                    ProcessRefreshKind::new().with_cpu().with_memory(),
                );

                let cpu = global_cpu_avg(&sys);
                let cpu_per_core: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
                let mem_used = sys.used_memory();
                let mem_total = sys.total_memory().max(1);
                let mem_percent = (mem_used as f64 / mem_total as f64 * 100.0) as f32;
                let swap_used = sys.used_swap();
                let swap_total = sys.total_swap();
                let cpu_count = sys.cpus().len() as u32;

                // alpha.50: collect running FFmpeg processes. Match
                // by basename containing "ffmpeg" so we catch both
                // our sidecar and any operator-launched debug ffmpeg.
                // sysinfo reports per-process CPU as percent-of-one-
                // core so a fully-loaded 8-core encoder shows ~800%
                // here — operators expect that convention, matches
                // Activity Monitor / Task Manager's "process CPU".
                let mut ffmpeg_processes: Vec<FfmpegProcess> = sys
                    .processes()
                    .iter()
                    .filter(|(_, p)| {
                        // alpha.56: sysinfo 0.30's Process::name returns
                        // &str directly (not &OsStr) — drop the
                        // to_string_lossy that compiled clean on my Mac
                        // because of a different sysinfo feature
                        // resolution but blew up Windows CI:
                        //   error[E0599]: no method named `to_string_lossy`
                        //   found for reference `&str` in the current scope
                        p.name().to_lowercase().contains("ffmpeg")
                    })
                    .map(|(pid, p)| {
                        // p.cmd() returns &[String] in sysinfo 0.30, so
                        // each element is already a String — join
                        // directly without per-element conversion.
                        let cmd_str: String = p.cmd().join(" ");
                        // Trim to first 80 chars and try to find a
                        // human-meaningful sub-arg (-format_code,
                        // destination url, etc.) for the excerpt.
                        let excerpt = if cmd_str.len() > 80 {
                            // Look for "decklink", "srt://", "rtmp://" first
                            for marker in ["-format_code", "srt://", "rtmp://", "decklink"] {
                                if let Some(pos) = cmd_str.find(marker) {
                                    let start = pos.saturating_sub(0);
                                    let end = (start + 80).min(cmd_str.len());
                                    return FfmpegProcess {
                                        pid: pid.as_u32(),
                                        cpu_percent: p.cpu_usage(),
                                        mem_mb: p.memory() / (1024 * 1024),
                                        cmd_excerpt: cmd_str[start..end].to_string(),
                                    };
                                }
                            }
                            format!("{}…", &cmd_str[..80])
                        } else {
                            cmd_str
                        };
                        FfmpegProcess {
                            pid: pid.as_u32(),
                            cpu_percent: p.cpu_usage(),
                            mem_mb: p.memory() / (1024 * 1024),
                            cmd_excerpt: excerpt,
                        }
                    })
                    .collect();
                // Sort by CPU desc so the hottest encoders are first.
                ffmpeg_processes.sort_by(|a, b| {
                    b.cpu_percent
                        .partial_cmp(&a.cpu_percent)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

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
                    cpu_per_core,
                    ffmpeg_processes,
                    swap_used_mb: swap_used / (1024 * 1024),
                    swap_total_mb: swap_total / (1024 * 1024),
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
