//! CPU and memory for a [`Target`].
//!
//! Collected with one shell command per sample, over the connection that already
//! exists. There is no agent to install: a host you can `ssh` into reports its
//! load immediately.
//!
//! CPU percentage cannot be read from a single sample. `/proc/stat` counts
//! cumulative jiffies since boot, so a lone reading describes the machine's
//! entire uptime, not this moment. Two samples are differenced instead, and the
//! **client** keeps the previous one — that way the remote side stays stateless
//! and a dropped connection cannot leave a counter stranded.

use rmux_transport::{CommandSpec, Platform, Target, Tty};
use serde::{Deserialize, Serialize};

/// One reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// 0–100, or `None` until a second sample exists to difference against.
    pub cpu_percent: Option<f32>,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub load_average: f32,
}

impl Sample {
    pub fn memory_percent(&self) -> f32 {
        if self.memory_total_bytes == 0 {
            return 0.0;
        }
        (self.memory_used_bytes as f32 / self.memory_total_bytes as f32) * 100.0
    }
}

/// Cumulative CPU counters, meaningful only as a difference.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CpuTotals {
    idle: u64,
    total: u64,
}

/// Samples one target, remembering enough to compute CPU deltas.
#[derive(Debug, Default)]
pub struct Collector {
    previous: Option<CpuTotals>,
    /// Detected once per target, when the transport did not already know.
    platform: Option<Platform>,
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a reading.
    pub async fn sample<T: Target + ?Sized>(&mut self, target: &T) -> anyhow::Result<Sample> {
        let platform = match target.platform() {
            Some(platform) => platform,
            // The target has not been connected yet, so nothing has detected its
            // OS. Guessing Linux here would run `/proc` reads against a BSD or
            // macOS host and fail with a confusing "No such file" rather than a
            // reading — so ask, once, and remember it.
            None => self.detect_platform(target).await?,
        };
        let script = if platform.has_procfs() { LINUX_SCRIPT } else { DARWIN_SCRIPT };

        let out = target.exec(&CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None)).await?;
        let text = out.stdout_or_err()?;

        let (sample, totals) = if platform.has_procfs() {
            parse_linux(text, self.previous)?
        } else {
            parse_darwin(text, self.previous)?
        };

        self.previous = totals;
        Ok(sample)
    }

    /// Ask the target what it is, caching the answer for later samples.
    async fn detect_platform<T: Target + ?Sized>(
        &mut self,
        target: &T,
    ) -> anyhow::Result<Platform> {
        if let Some(known) = self.platform {
            return Ok(known);
        }

        let out = target.exec(&CommandSpec::new("uname").arg("-s").tty(Tty::None)).await?;
        let platform = match out.stdout_or_err()? {
            s if s.eq_ignore_ascii_case("linux") => Platform::Linux,
            s if s.eq_ignore_ascii_case("darwin") => Platform::MacOs,
            _ => Platform::Other,
        };

        self.platform = Some(platform);
        Ok(platform)
    }
}

/// `/proc/stat` first line, `/proc/meminfo`, and the load average.
///
/// `MemAvailable` is used rather than `MemFree` deliberately: free memory on a
/// busy Linux box is near zero because the kernel uses everything spare as cache,
/// so reporting it would show every healthy server as out of memory.
const LINUX_SCRIPT: &str = r#"head -n1 /proc/stat
grep -E '^(MemTotal|MemAvailable):' /proc/meminfo
cut -d' ' -f1 /proc/loadavg"#;

const DARWIN_SCRIPT: &str = r#"sysctl -n hw.memsize
vm_stat | head -n 6
sysctl -n vm.loadavg | awk '{print $2}'"#;

fn parse_linux(
    text: &str,
    previous: Option<CpuTotals>,
) -> anyhow::Result<(Sample, Option<CpuTotals>)> {
    let mut sample = Sample::default();
    let mut totals = CpuTotals::default();
    let mut mem_total = 0u64;
    let mut mem_available = 0u64;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
            // user nice system idle iowait irq softirq steal
            // iowait counts as idle: the CPU is not executing anything during it,
            // and counting it as busy makes disk-bound hosts look pegged.
            let idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
            totals = CpuTotals { idle, total: fields.iter().sum() };
        } else if let Some(rest) = line.strip_prefix("MemTotal:") {
            mem_total = parse_kib(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            mem_available = parse_kib(rest);
        } else if let Ok(load) = line.trim().parse::<f32>() {
            sample.load_average = load;
        }
    }

    sample.memory_total_bytes = mem_total;
    sample.memory_used_bytes = mem_total.saturating_sub(mem_available);
    sample.cpu_percent = cpu_from_delta(previous, totals);

    Ok((sample, Some(totals)))
}

fn parse_darwin(
    text: &str,
    _previous: Option<CpuTotals>,
) -> anyhow::Result<(Sample, Option<CpuTotals>)> {
    let mut sample = Sample::default();
    let mut page_size = 4096u64;
    let mut free_pages = 0u64;
    let mut speculative = 0u64;

    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            sample.memory_total_bytes = line.trim().parse().unwrap_or(0);
            continue;
        }
        if let Some(rest) = line.split("page size of").nth(1) {
            page_size =
                rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(4096);
        } else if let Some(rest) = line.strip_prefix("Pages free:") {
            free_pages = parse_pages(rest);
        } else if let Some(rest) = line.strip_prefix("Pages speculative:") {
            speculative = parse_pages(rest);
        } else if let Ok(load) = line.trim().parse::<f32>() {
            sample.load_average = load;
        }
    }

    let available = (free_pages + speculative) * page_size;
    sample.memory_used_bytes = sample.memory_total_bytes.saturating_sub(available);
    // macOS has no cheap cumulative counter equivalent to /proc/stat, so CPU is
    // left unreported rather than fabricated from an unrelated figure.
    sample.cpu_percent = None;

    Ok((sample, None))
}

fn parse_kib(s: &str) -> u64 {
    s.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
}

fn parse_pages(s: &str) -> u64 {
    s.trim().trim_end_matches('.').parse().unwrap_or(0)
}

/// CPU busy percentage between two cumulative readings.
fn cpu_from_delta(previous: Option<CpuTotals>, current: CpuTotals) -> Option<f32> {
    let previous = previous?;

    // A reboot resets the counters, so a shrinking total means the baseline is
    // meaningless — report nothing rather than a wild number.
    let total_delta = current.total.checked_sub(previous.total)?;
    let idle_delta = current.idle.saturating_sub(previous.idle);

    if total_delta == 0 {
        return None;
    }

    let busy = total_delta.saturating_sub(idle_delta) as f32 / total_delta as f32;
    Some((busy * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_STAT: &str = "cpu  1000 0 500 8000 500 0 0 0";

    #[test]
    fn the_first_sample_reports_no_cpu_figure() {
        // A single cumulative reading describes the whole uptime, not now.
        // Inventing a number from it would be fabricating data.
        let (sample, totals) =
            parse_linux(&format!("{PROC_STAT}\nMemTotal: 1024 kB\nMemAvailable: 512 kB\n0.5"), None)
                .unwrap();

        assert_eq!(sample.cpu_percent, None);
        assert!(totals.is_some(), "the baseline must be kept for the next sample");
    }

    #[test]
    fn cpu_is_the_difference_between_two_samples() {
        let (_, first) = parse_linux(&format!("{PROC_STAT}\nMemTotal: 0 kB"), None).unwrap();

        // 1000 more jiffies total, 500 of them idle → 50% busy.
        let later = "cpu  1400 0 600 8400 600 0 0 0";
        let (sample, _) = parse_linux(&format!("{later}\nMemTotal: 0 kB"), first).unwrap();

        let cpu = sample.cpu_percent.expect("a second sample should yield a figure");
        assert!((cpu - 50.0).abs() < 0.01, "expected ~50%, got {cpu}");
    }

    #[test]
    fn iowait_counts_as_idle() {
        let (_, first) = parse_linux("cpu  100 0 100 1000 0 0 0 0\nMemTotal: 0 kB", None).unwrap();
        // Everything new is iowait: the CPU executed nothing, so it is not busy.
        let (sample, _) =
            parse_linux("cpu  100 0 100 1000 1000 0 0 0\nMemTotal: 0 kB", first).unwrap();

        assert_eq!(sample.cpu_percent, Some(0.0), "iowait must not read as busy");
    }

    #[test]
    fn a_reboot_does_not_produce_a_nonsense_figure() {
        let stale = Some(CpuTotals { idle: 999_999, total: 9_999_999 });
        // Counters reset to something far lower.
        assert_eq!(cpu_from_delta(stale, CpuTotals { idle: 10, total: 100 }), None);
    }

    #[test]
    fn memory_uses_available_not_free() {
        // On a busy Linux host free memory is near zero because the kernel caches
        // with it; reporting that would show every healthy server as full.
        let (sample, _) = parse_linux(
            &format!("{PROC_STAT}\nMemTotal: 8000000 kB\nMemAvailable: 6000000 kB"),
            None,
        )
        .unwrap();

        assert_eq!(sample.memory_total_bytes, 8_000_000 * 1024);
        assert_eq!(sample.memory_used_bytes, 2_000_000 * 1024);
        assert!((sample.memory_percent() - 25.0).abs() < 0.01);
    }

    #[test]
    fn the_load_average_is_read() {
        let (sample, _) = parse_linux(&format!("{PROC_STAT}\nMemTotal: 0 kB\n1.75"), None).unwrap();
        assert!((sample.load_average - 1.75).abs() < 0.001);
    }

    #[test]
    fn macos_memory_is_derived_from_page_counts() {
        let text = "17179869184\n\
                    Mach Virtual Memory Statistics: (page size of 4096 bytes)\n\
                    Pages free:                    1000000.\n\
                    Pages active:                  500000.\n\
                    Pages inactive:                200000.\n\
                    Pages speculative:             48576.\n\
                    2.5";

        let (sample, _) = parse_darwin(text, None).unwrap();

        assert_eq!(sample.memory_total_bytes, 17_179_869_184);
        assert_eq!(sample.memory_used_bytes, 17_179_869_184 - (1_048_576 * 4096));
        // No fabricated percentage where no cumulative counter exists.
        assert_eq!(sample.cpu_percent, None);
    }

    #[test]
    fn garbage_output_does_not_panic() {
        // A host mid-reboot, or one whose shell printed a banner, must degrade to
        // zeroes rather than taking the status bar down.
        let (sample, _) = parse_linux("not what we expected at all", None).unwrap();
        assert_eq!(sample.memory_total_bytes, 0);
        assert_eq!(sample.memory_percent(), 0.0);
    }
}
