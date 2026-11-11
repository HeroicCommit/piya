//! Lightweight self-monitoring of this process's CPU use.
//!
//! A background thread samples CPU every 30s (via `ps`, one tiny subprocess) and
//! logs a warning when use stays high, so a runaway loop shows up in
//! `piya-worker.log` instead of being noticed as a hot fan.
//!
//! Env: `PIYA_CPU_LOG=1` logs every sample, `PIYA_CPU_WARN_PCT` (default 80) sets
//! the sustained-high threshold, `PIYA_CPU_MONITOR=off` disables it.

use std::process::Command;
use std::sync::Once;
use std::time::Duration;

use tracing::{info, warn};

const SAMPLE_EVERY: Duration = Duration::from_secs(30);
/// Consecutive high samples before warning (3 x 30s = 90s sustained).
const SUSTAINED_SAMPLES: u32 = 3;

/// Start the monitor once per process (safe to call from several places).
pub fn start(label: &'static str) {
    static STARTED: Once = Once::new();
    if matches!(
        std::env::var("PIYA_CPU_MONITOR").unwrap_or_default().to_lowercase().as_str(),
        "0" | "off" | "false" | "no"
    ) {
        return;
    }
    STARTED.call_once(|| {
        let warn_pct = std::env::var("PIYA_CPU_WARN_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(80.0);
        let verbose = std::env::var("PIYA_CPU_LOG").is_ok_and(|v| v == "1");
        let _ = std::thread::Builder::new()
            .name("piya-cpu-monitor".into())
            .spawn(move || run(label, warn_pct, verbose));
    });
}

fn run(label: &str, warn_pct: f64, verbose: bool) {
    let pid = std::process::id();
    let mut high = 0u32;
    loop {
        std::thread::sleep(SAMPLE_EVERY);
        let Some(cpu) = sample_cpu_percent(pid) else {
            continue;
        };
        if verbose {
            info!("cpu[{label}] {cpu:.1}%");
        }
        if cpu >= warn_pct {
            high += 1;
            if high == SUSTAINED_SAMPLES {
                warn!(
                    "cpu[{label}] sustained high CPU: {cpu:.0}% for ~{}s (threshold {warn_pct:.0}%) — \
                     possible busy loop; check running tasks and MCP servers",
                    SAMPLE_EVERY.as_secs() * u64::from(SUSTAINED_SAMPLES)
                );
            }
        } else {
            high = 0;
        }
    }
}

/// Recent CPU % of `pid` as reported by `ps` (can exceed 100 on multi-core).
fn sample_cpu_percent(pid: u32) -> Option<f64> {
    let out = Command::new("ps")
        .args(["-o", "pcpu=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_pcpu(&String::from_utf8_lossy(&out.stdout))
}

fn parse_pcpu(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_output() {
        assert_eq!(parse_pcpu("  12.5\n"), Some(12.5));
        assert_eq!(parse_pcpu(""), None);
        assert_eq!(parse_pcpu("abc"), None);
    }

    #[test]
    fn samples_own_process() {
        assert!(sample_cpu_percent(std::process::id()).is_some());
    }
}
