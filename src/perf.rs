//! A lightweight built-in performance sampler.
//!
//! Reads this process's own CPU time (via `getrusage`) and derives the CPU
//! percentage used between calls (100% = one full core). Samples are tagged
//! with a mode label so different app states (RGB, animation, peek, mini,
//! HID mode) can be compared.

use std::time::Instant;

/// Samples the process CPU% between calls.
pub struct CpuSampler {
    last_cpu: f64,
    last_wall: Instant,
}

impl CpuSampler {
    pub fn new() -> Self {
        CpuSampler { last_cpu: cpu_seconds(), last_wall: Instant::now() }
    }

    /// CPU used since the last call, as a percentage of one core.
    pub fn sample(&mut self) -> f32 {
        let now = Instant::now();
        let cpu = cpu_seconds();
        let dt = now.duration_since(self.last_wall).as_secs_f64().max(1e-6);
        let pct = ((cpu - self.last_cpu) / dt * 100.0).max(0.0);
        self.last_cpu = cpu;
        self.last_wall = now;
        pct as f32
    }
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
fn cpu_seconds() -> f64 {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return 0.0;
        }
        let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        tv(ru.ru_utime) + tv(ru.ru_stime)
    }
}

#[cfg(not(unix))]
fn cpu_seconds() -> f64 {
    0.0
}

/// A snapshot of what the app was doing when a sample was taken.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct PerfState {
    /// LEDs actively driven by keyjitsu (a constant effect is running).
    pub anim: bool,
    /// A press effect is in flight (LEDs briefly driven).
    pub press_fx: bool,
    pub peek: bool,
    pub glow_sync: bool,
    pub connected: bool,
}

impl PerfState {
    /// A compact label describing the active state.
    pub fn label(self) -> String {
        let mut p: Vec<&str> = Vec::new();
        if self.anim {
            p.push("anim");
        } else if self.press_fx {
            p.push("press-fx");
        } else if self.glow_sync {
            p.push("glow-sync");
        }
        if self.peek {
            p.push("peek");
        }
        if p.is_empty() {
            if self.connected { "idle" } else { "disconnected" }.to_string()
        } else {
            p.join("+")
        }
    }
}

/// Per-mode aggregate.
#[derive(Clone)]
pub struct ModeStat {
    pub label: String,
    pub avg: f32,
    pub max: f32,
    pub n: usize,
}

/// A finished session's results.
#[derive(Clone, Default)]
pub struct Summary {
    pub avg: f32,
    pub max: f32,
    pub n: usize,
    pub secs: u64,
    pub modes: Vec<ModeStat>,
}

/// Aggregate `(label, cpu)` samples into an overall + per-mode summary.
pub fn summarize(samples: &[(String, f32)], secs: u64) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let n = samples.len();
    let avg = samples.iter().map(|(_, c)| *c).sum::<f32>() / n as f32;
    let max = samples.iter().map(|(_, c)| *c).fold(0.0, f32::max);

    // Group by label, preserving first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<f32>> = std::collections::HashMap::new();
    for (label, cpu) in samples {
        if !groups.contains_key(label) {
            order.push(label.clone());
        }
        groups.entry(label.clone()).or_default().push(*cpu);
    }
    let modes = order
        .into_iter()
        .map(|label| {
            let v = &groups[&label];
            ModeStat {
                avg: v.iter().sum::<f32>() / v.len() as f32,
                max: v.iter().copied().fold(0.0, f32::max),
                n: v.len(),
                label,
            }
        })
        .collect();

    Summary { avg, max, n, secs, modes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_overall_and_per_mode() {
        let s = summarize(
            &[
                ("idle".into(), 2.0),
                ("idle".into(), 4.0),
                ("anim".into(), 10.0),
                ("anim".into(), 20.0),
            ],
            60,
        );
        assert_eq!(s.n, 4);
        assert!((s.avg - 9.0).abs() < 0.01);
        assert!((s.max - 20.0).abs() < 0.01);
        assert_eq!(s.modes.len(), 2);
        assert_eq!(s.modes[0].label, "idle");
        assert!((s.modes[0].avg - 3.0).abs() < 0.01);
        assert!((s.modes[1].max - 20.0).abs() < 0.01);
    }

    #[test]
    fn state_label() {
        let mut st = PerfState { connected: true, ..Default::default() };
        assert_eq!(st.label(), "idle");
        st.anim = true;
        st.peek = true;
        assert_eq!(st.label(), "anim+peek");
    }
}
