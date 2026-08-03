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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// 0–100, or `None` until a second sample exists to difference against.
    pub cpu_percent: Option<f32>,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub load_average: f32,
    /// The host's own name, which is not always the alias used to reach it —
    /// an `~/.ssh/config` entry can be called anything.
    pub hostname: String,
    pub uptime_seconds: u64,
    /// Bytes per second across every interface except loopback. `None` on the
    /// first sample: these are cumulative counters, so a single reading
    /// describes the whole uptime rather than now — the same reason CPU is
    /// unreported until there are two.
    pub net_rx_bps: Option<u64>,
    pub net_tx_bps: Option<u64>,
    /// How many CPUs the host has.
    ///
    /// `ps` reports %CPU per core, so a busy process on a 16-core box reads
    /// 1600%. Without this the process widget shows numbers like "1000%",
    /// which is not wrong so much as meaningless.
    pub cores: u32,
}

/// One process, as `ps` reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    pub pid: u32,
    /// The command, without its path or arguments.
    pub name: String,
    pub cpu_percent: f32,
    pub memory_percent: f32,
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

/// Cumulative network counters, likewise.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NetTotals {
    rx: u64,
    tx: u64,
    /// The host's own uptime at the moment of the reading, used as the clock.
    ///
    /// Deliberately not the client's wall clock: the interval that matters is
    /// how much time passed *on the host* between the two counter readings, and
    /// network latency or a stalled connection makes those differ badly enough
    /// to invent traffic that never happened.
    at_uptime: u64,
}

/// Samples one target, remembering enough to compute CPU deltas.
#[derive(Debug, Default)]
pub struct Collector {
    previous: Option<CpuTotals>,
    previous_net: Option<NetTotals>,
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

        let (mut sample, totals, net) = if platform.has_procfs() {
            parse_linux(text, self.previous)?
        } else {
            parse_darwin(text, self.previous)?
        };

        if let (Some(current), Some(previous)) = (net, self.previous_net) {
            let (rx, tx) = net_rates(previous, current);
            sample.net_rx_bps = rx;
            sample.net_tx_bps = tx;
        }

        self.previous = totals;
        if net.is_some() {
            self.previous_net = net;
        }
        Ok(sample)
    }

    /// The processes using the most CPU, or the most memory.
    ///
    /// A separate call from [`Collector::sample`] because it is only worth
    /// running while the process widget is open: `ps` over every process is far
    /// more output than the status line needs twice a second.
    pub async fn processes<T: Target + ?Sized>(
        &mut self,
        target: &T,
        by: SortBy,
        limit: usize,
    ) -> anyhow::Result<Vec<Process>> {
        // `ps` rather than `top`: `top` is a curses program whose batch mode
        // flags differ between Linux and the BSDs, while this invocation is
        // POSIX and behaves the same on both.
        let sort = match by {
            SortBy::Cpu => "-pcpu",
            SortBy::Memory => "-pmem",
        };
        let script = format!("ps -eo pid=,pcpu=,pmem=,comm= --sort={sort} 2>/dev/null | head -n 40");

        let out =
            target.exec(&CommandSpec::new("sh").arg("-c").arg(&script).tty(Tty::None)).await?;

        Ok(parse_processes(out.stdout_or_err()?, limit))
    }

    /// Ask a process to stop, or make it.
    ///
    /// **The pid is a `u32`, and that is the security property.** This is the
    /// one place in rmux where the operator points at something on a host and
    /// says "end that", so the argument must not be able to become anything but
    /// a number — a string here would be a shell injection with a `kill` in
    /// front of it. Typing it out of the wire is a stronger guarantee than any
    /// amount of quoting.
    ///
    /// `TERM` first, always. `KILL` is offered separately because it gives the
    /// process no chance to flush, close sockets or remove its own files, and
    /// on a dev host that routinely means a corrupted build or a stale lock —
    /// so it is a deliberate second choice, never the default.
    ///
    /// Signals rather than `pkill`: a name pattern can match processes the
    /// operator never saw, including their own shell.
    pub async fn kill<T: Target + ?Sized>(
        &self,
        target: &T,
        pid: u32,
        hard: bool,
    ) -> anyhow::Result<()> {
        if pid <= 1 {
            // `1` is init and `0` means "every process in my group" — a signal
            // there would take out the operator's whole session, which is
            // never what clicking one row meant.
            anyhow::bail!("refusing to signal pid {pid}");
        }

        let signal = if hard { "KILL" } else { "TERM" };
        let out = target
            .exec(
                &CommandSpec::new("sh")
                    .arg("-c")
                    // `2>&1` so the reason reaches the operator. "Operation not
                    // permitted" is the common case — rmux is built for shared
                    // dev boxes, where half the interesting processes belong to
                    // someone else — and a signal that silently did nothing is
                    // the worst outcome: the row stays, and it reads as the
                    // process ignoring TERM rather than as never having been
                    // sent one.
                    .arg(format!("kill -{signal} {pid} 2>&1"))
                    .tty(Tty::None),
            )
            .await?;

        // **Both halves, and the status first.** `kill` exits non-zero *and*
        // prints why, so reading only the text would report a clean success
        // whenever `stdout_or_err` refused the non-zero status — verified
        // against a real host, where `kill -TERM 999999` exits 1 with
        // "No such process" on stdout. Trusting either signal alone is wrong.
        if out.ok() {
            return Ok(());
        }

        let said = out.stdout.trim();
        let said = if said.is_empty() { out.stderr.trim() } else { said };
        // `sh: 1: kill:` is the shell naming itself, which tells the operator
        // nothing they did not already know.
        let said = said.rsplit(": ").next().unwrap_or(said).trim();

        anyhow::bail!(
            "{}",
            if said.is_empty() { format!("kill exited {}", out.status) } else { said.to_owned() }
        )
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
            // Git Bash / MSYS2 answers `MINGW64_NT-…`, Cygwin `CYGWIN_NT-…`.
            // Both emulate procfs, so they take the Linux path — see
            // `Platform::has_procfs`.
            s if s.starts_with("MINGW") || s.starts_with("CYGWIN") => Platform::Windows,
            _ => Platform::Other,
        };

        self.platform = Some(platform);
        Ok(platform)
    }
}

/// Which resource the process list is ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortBy {
    Cpu,
    Memory,
}

/// `/proc/stat` first line, `/proc/meminfo`, and the load average.
///
/// `MemAvailable` is used rather than `MemFree` deliberately: free memory on a
/// busy Linux box is near zero because the kernel uses everything spare as cache,
/// so reporting it would show every healthy server as out of memory.
/// Everything after the load average is **tagged**, deliberately.
///
/// The parser identifies the load average as "the line that parses as a float",
/// which was fine when it was the only bare number. Adding an untagged uptime
/// would have been read as the load and vice versa — a silent swap that shows a
/// plausible wrong number rather than failing. Tags make each line
/// unambiguous, and `NET` sums every interface except loopback, which otherwise
/// double-counts local traffic.
/// **Every optional reading is guarded.** A procfs emulation need not be
/// complete: measured on a real Windows host reached through Git Bash,
/// `/proc/stat` and `/proc/meminfo` are faithful but `/proc/net/dev`,
/// `/proc/loadavg` and `/proc/uptime` are simply absent. Unguarded, the missing
/// one exited non-zero and took the *whole* sample with it — so CPU and memory,
/// which were sitting right there, were reported as a failed connection. A
/// metric that cannot be read is a blank field, not an error.
const LINUX_SCRIPT: &str = r#"head -n1 /proc/stat
grep -E '^(MemTotal|MemAvailable|MemFree):' /proc/meminfo
cut -d' ' -f1 /proc/loadavg 2>/dev/null || true
echo "HOST $(cat /proc/sys/kernel/hostname 2>/dev/null || hostname 2>/dev/null)"
echo "UPTIME $(cut -d' ' -f1 /proc/uptime 2>/dev/null || echo 0)"
echo "CORES $(nproc 2>/dev/null || echo 1)"
if [ -r /proc/net/dev ]; then
awk 'NR>2 {sub(/:/, " "); if ($1 != "lo") { rx += $2; tx += $10 }} END {print "NET", rx+0, tx+0}' /proc/net/dev
fi"#;

const DARWIN_SCRIPT: &str = r#"sysctl -n hw.memsize
vm_stat | head -n 6
sysctl -n vm.loadavg | awk '{print $2}'
echo "HOST $(hostname -s)"
echo "UPTIME $(( $(date +%s) - $(sysctl -n kern.boottime | sed -n 's/.*sec = \([0-9]*\).*/\1/p') ))""#;

fn parse_linux(
    text: &str,
    previous: Option<CpuTotals>,
) -> anyhow::Result<(Sample, Option<CpuTotals>, Option<NetTotals>)> {
    let mut sample = Sample::default();
    let mut totals = CpuTotals::default();
    let mut net: Option<NetTotals> = None;
    let mut mem_total = 0u64;
    let mut mem_available = 0u64;
    // Fallback for procfs emulations that omit `MemAvailable` — MSYS2 on
    // Windows is one, and without this its host panel reported 100% memory
    // used on an idle machine, which is a wrong number presented as a fact.
    let mut mem_free = 0u64;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("HOST ") {
            sample.hostname = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("UPTIME ") {
            // Printed with a fractional part; seconds are all anyone reads.
            sample.uptime_seconds = rest.trim().split('.').next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("CORES ") {
            sample.cores = rest.trim().parse().unwrap_or(1).max(1);
        } else if let Some(rest) = line.strip_prefix("NET ") {
            let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
            if fields.len() == 2 {
                net = Some(NetTotals { rx: fields[0], tx: fields[1], at_uptime: 0 });
            }
        } else if let Some(rest) = line.strip_prefix("cpu ") {
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
        } else if let Some(rest) = line.strip_prefix("MemFree:") {
            mem_free = parse_kib(rest);
        } else if let Ok(load) = line.trim().parse::<f32>() {
            sample.load_average = load;
        }
    }

    sample.memory_total_bytes = mem_total;
    // `MemAvailable` accounts for reclaimable cache and is the honest figure;
    // `MemFree` overstates usage, but overstating beats reporting every host
    // that lacks the field as completely full.
    let mem_available = if mem_available > 0 { mem_available } else { mem_free };
    sample.memory_used_bytes = mem_total.saturating_sub(mem_available);
    sample.cpu_percent = cpu_from_delta(previous, totals);

    // The host's uptime is the clock the network rates are differenced against.
    let net = net.map(|n| NetTotals { at_uptime: sample.uptime_seconds, ..n });

    Ok((sample, Some(totals), net))
}

fn parse_darwin(
    text: &str,
    _previous: Option<CpuTotals>,
) -> anyhow::Result<(Sample, Option<CpuTotals>, Option<NetTotals>)> {
    let mut sample = Sample::default();
    let mut page_size = 4096u64;
    let mut free_pages = 0u64;
    let mut speculative = 0u64;

    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            sample.memory_total_bytes = line.trim().parse().unwrap_or(0);
            continue;
        }
        if let Some(rest) = line.strip_prefix("HOST ") {
            sample.hostname = rest.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("UPTIME ") {
            sample.uptime_seconds = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.split("page size of").nth(1) {
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

    Ok((sample, None, None))
}

/// Bytes per second between two cumulative network readings.
fn net_rates(previous: NetTotals, current: NetTotals) -> (Option<u64>, Option<u64>) {
    // Uptime going backwards means a reboot, which also resets the byte
    // counters — the baseline is meaningless, so report nothing rather than a
    // number derived from two unrelated runs.
    let Some(seconds) = current.at_uptime.checked_sub(previous.at_uptime) else {
        return (None, None);
    };
    if seconds == 0 {
        return (None, None);
    }

    // The counters themselves also wrap on 32-bit interfaces. A decrease is
    // never real traffic, so it is dropped rather than shown as a huge spike.
    let rate = |now: u64, before: u64| now.checked_sub(before).map(|d| d / seconds);
    (rate(current.rx, previous.rx), rate(current.tx, previous.tx))
}

/// Processes that exist only because we asked.
///
/// `ps` itself routinely tops its own output — it is the thing that just ran —
/// and reporting it as the busiest process on the box is both wrong and
/// faintly absurd.
const OWN_PROCESSES: [&str; 8] =
    ["ps", "awk", "head", "sh", "sort", "nproc", "printf", "sleep"];

/// Parse `ps -eo pid=,pcpu=,pmem=,comm=` output.
fn parse_processes(text: &str, limit: usize) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let cpu_percent = fields.next()?.parse().ok()?;
            let memory_percent = fields.next()?.parse().ok()?;
            // Everything left is the command. `comm` is already the basename on
            // Linux, but a path shows up on some systems and the widget has room
            // for a name, not a path.
            let rest = fields.collect::<Vec<_>>().join(" ");
            let name = rest.rsplit('/').next().unwrap_or(&rest).trim().to_owned();

            if name.is_empty() || OWN_PROCESSES.contains(&name.as_str()) {
                return None;
            }
            Some(Process { pid, name, cpu_percent, memory_percent })
        })
        .take(limit)
        .collect()
}

/// "46d 5h", "5h 12m", "12m" — the largest two units that are non-zero.
///
/// Formatted here rather than in the UI so both the widget and any future
/// caller agree, and so it can be tested without a browser.
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
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

    /// A kill that did not happen must not report that it did.
    ///
    /// This is the exact bug this test exists for, and it was real: `kill`
    /// exits non-zero *and* explains itself on stdout, so reading only the
    /// text — through a helper that refuses non-zero statuses and yields
    /// nothing — reported a clean success for every failure. On a shared dev
    /// box the common failure is "Operation not permitted", and silently
    /// swallowing it makes the row look like a process ignoring TERM rather
    /// than one that was never signalled.
    ///
    /// Run against `LocalTarget` rather than a stub: the behaviour under test
    /// is what a real `sh` does with a real `kill`, which is precisely what a
    /// stub would have to assume and could therefore get wrong.
    #[tokio::test]
    async fn a_kill_that_failed_is_reported_as_a_failure() {
        let target = rmux_transport::local::LocalTarget::new();
        let metrics = Collector::new();

        // Comfortably past any real pid, so nothing is actually signalled.
        let result = metrics.kill(&target, 4_000_000, false).await;

        let error = result.expect_err("a kill of a nonexistent pid must not report success");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("no such process") || message.contains("kill"),
            "the reason must survive to the operator, got: {error}"
        );
        // The shell naming itself is noise the operator cannot act on.
        assert!(!message.starts_with("sh:"), "got: {error}");
    }

    /// Signalling pid 0 or 1 is never what clicking one row meant.
    #[tokio::test]
    async fn init_and_the_process_group_are_refused() {
        let target = rmux_transport::local::LocalTarget::new();
        let metrics = Collector::new();

        // `0` means "every process in my group" — that would take out the
        // operator's own session, and it would look like rmux crashed.
        assert!(metrics.kill(&target, 0, false).await.is_err());
        assert!(metrics.kill(&target, 1, true).await.is_err());
    }

    #[test]
    fn the_first_sample_reports_no_cpu_figure() {
        // A single cumulative reading describes the whole uptime, not now.
        // Inventing a number from it would be fabricating data.
        let (sample, totals, _) =
            parse_linux(&format!("{PROC_STAT}\nMemTotal: 1024 kB\nMemAvailable: 512 kB\n0.5"), None)
                .unwrap();

        assert_eq!(sample.cpu_percent, None);
        assert!(totals.is_some(), "the baseline must be kept for the next sample");
    }

    #[test]
    fn cpu_is_the_difference_between_two_samples() {
        let (_, first, _) = parse_linux(&format!("{PROC_STAT}\nMemTotal: 0 kB"), None).unwrap();

        // 1000 more jiffies total, 500 of them idle → 50% busy.
        let later = "cpu  1400 0 600 8400 600 0 0 0";
        let (sample, _, _) = parse_linux(&format!("{later}\nMemTotal: 0 kB"), first).unwrap();

        let cpu = sample.cpu_percent.expect("a second sample should yield a figure");
        assert!((cpu - 50.0).abs() < 0.01, "expected ~50%, got {cpu}");
    }

    #[test]
    fn iowait_counts_as_idle() {
        let (_, first, _) = parse_linux("cpu  100 0 100 1000 0 0 0 0\nMemTotal: 0 kB", None).unwrap();
        // Everything new is iowait: the CPU executed nothing, so it is not busy.
        let (sample, _, _) =
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
        let (sample, _, _) = parse_linux(
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
        let (sample, _, _) = parse_linux(&format!("{PROC_STAT}\nMemTotal: 0 kB\n1.75"), None).unwrap();
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

        let (sample, _, _) = parse_darwin(text, None).unwrap();

        assert_eq!(sample.memory_total_bytes, 17_179_869_184);
        assert_eq!(sample.memory_used_bytes, 17_179_869_184 - (1_048_576 * 4096));
        // No fabricated percentage where no cumulative counter exists.
        assert_eq!(sample.cpu_percent, None);
    }

    #[test]
    fn the_host_reports_its_own_name_and_uptime() {
        let text = format!("{PROC_STAT}\nMemTotal: 0 kB\n0.5\nHOST reactor-01\nUPTIME 3986705.42");
        let (sample, _, _) = parse_linux(&text, None).unwrap();

        assert_eq!(sample.hostname, "reactor-01");
        // The fractional part is dropped rather than failing the parse.
        assert_eq!(sample.uptime_seconds, 3_986_705);
        // …and the load average is still the load average, not the uptime.
        assert!((sample.load_average - 0.5).abs() < 0.001);
    }

    #[test]
    fn an_untagged_number_cannot_be_mistaken_for_the_uptime() {
        // The parser finds the load average by "the line that parses as a
        // float". Tagging the newer fields is what stops a 3-million-second
        // uptime being read as a load average of 3 million.
        let text = format!("{PROC_STAT}\nMemTotal: 0 kB\n1.25\nUPTIME 999999");
        let (sample, _, _) = parse_linux(&text, None).unwrap();

        assert!((sample.load_average - 1.25).abs() < 0.001, "{}", sample.load_average);
        assert_eq!(sample.uptime_seconds, 999_999);
    }

    #[test]
    fn network_rates_need_two_readings() {
        let first = format!("{PROC_STAT}\nMemTotal: 0 kB\nUPTIME 100\nNET 1000 2000");
        let (sample, _, net) = parse_linux(&first, None).unwrap();
        // One cumulative reading describes the whole uptime, so no rate yet.
        assert_eq!(sample.net_rx_bps, None);
        let first_net = net.expect("counters must be kept for the next sample");
        assert_eq!(first_net.at_uptime, 100);

        // Ten seconds later: 5000 more bytes in, 1000 more out.
        let later = format!("{PROC_STAT}\nMemTotal: 0 kB\nUPTIME 110\nNET 6000 3000");
        let (_, _, net) = parse_linux(&later, None).unwrap();
        let (rx, tx) = net_rates(first_net, net.unwrap());

        assert_eq!(rx, Some(500));
        assert_eq!(tx, Some(100));
    }

    #[test]
    fn a_reboot_or_a_wrapped_counter_reports_no_rate() {
        let before = NetTotals { rx: 900_000, tx: 900_000, at_uptime: 5_000 };
        // Uptime went backwards: the host rebooted and the byte counters reset,
        // so the two readings are from unrelated runs.
        assert_eq!(net_rates(before, NetTotals { rx: 10, tx: 10, at_uptime: 12 }), (None, None));

        // Uptime advanced but a counter wrapped. A decrease is never traffic,
        // and treating it as one would draw a spike of hundreds of gigabits.
        let wrapped = NetTotals { rx: 10, tx: 950_000, at_uptime: 5_010 };
        assert_eq!(net_rates(before, wrapped), (None, Some(5_000)));
    }

    #[test]
    fn two_readings_in_the_same_second_report_nothing() {
        // Dividing by a zero interval is the other way to invent a number.
        let a = NetTotals { rx: 100, tx: 100, at_uptime: 42 };
        let b = NetTotals { rx: 900, tx: 900, at_uptime: 42 };
        assert_eq!(net_rates(a, b), (None, None));
    }

    #[test]
    fn processes_are_parsed_in_the_order_ps_returned_them() {
        // `ps` did the sorting; re-sorting here would silently disagree with the
        // --sort flag the command was given.
        let text = "  1234  17.3   2.1 kubectl\n\
                       987   7.0   3.4 containerd\n\
                       12   3.2   0.5 start.js\n";
        let processes = parse_processes(text, 10);

        assert_eq!(processes.len(), 3);
        assert_eq!(processes[0].pid, 1234);
        assert_eq!(processes[0].name, "kubectl");
        assert!((processes[0].cpu_percent - 17.3).abs() < 0.01);
        assert!((processes[0].memory_percent - 2.1).abs() < 0.01);
        assert_eq!(processes[2].name, "start.js");
    }

    #[test]
    fn the_query_does_not_report_itself_as_the_busiest_process() {
        // `ps` is the thing that just ran, so it reliably tops its own output.
        let text = "1 99.0 0.1 ps\n2 40.0 1.0 awk\n3 12.0 2.0 node\n";
        let processes = parse_processes(text, 10);

        assert_eq!(processes.len(), 1, "{processes:?}");
        assert_eq!(processes[0].name, "node");
    }

    #[test]
    fn the_core_count_is_read_so_per_core_figures_can_be_scaled() {
        // ps reports %CPU per core: 1600% on a 16-core box is one busy process,
        // not a broken reading.
        let text = format!("{PROC_STAT}\nMemTotal: 0 kB\nCORES 16");
        let (sample, _, _) = parse_linux(&text, None).unwrap();
        assert_eq!(sample.cores, 16);

        // Never zero — it is a divisor.
        let (sample, _, _) = parse_linux(&format!("{PROC_STAT}\nCORES 0"), None).unwrap();
        assert_eq!(sample.cores, 1);
    }

    #[test]
    fn a_process_path_is_reduced_to_its_name() {
        // The widget has room for a name, not `/usr/lib/systemd/systemd-resolved`.
        let processes = parse_processes("5 1.0 1.0 /usr/lib/systemd/systemd-resolved\n", 10);
        assert_eq!(processes[0].name, "systemd-resolved");
    }

    #[test]
    fn a_header_line_or_junk_is_skipped_rather_than_shown() {
        // Some `ps` implementations print a header even with the `=` suffixes.
        let text = "  PID %CPU %MEM COMMAND\n  42  1.0  2.0 nginx\n\n";
        let processes = parse_processes(text, 10);

        assert_eq!(processes.len(), 1, "{processes:?}");
        assert_eq!(processes[0].name, "nginx");
    }

    #[test]
    fn the_process_limit_is_respected() {
        let text = (0..30).map(|i| format!("{i} 1.0 1.0 p{i}\n")).collect::<String>();
        assert_eq!(parse_processes(&text, 5).len(), 5);
    }

    #[test]
    fn uptime_reads_as_the_two_largest_units() {
        assert_eq!(format_uptime(46 * 86_400 + 5 * 3_600), "46d 5h");
        assert_eq!(format_uptime(5 * 3_600 + 12 * 60), "5h 12m");
        assert_eq!(format_uptime(12 * 60), "12m");
        // Never blank, and never negative-looking.
        assert_eq!(format_uptime(0), "0m");
        assert_eq!(format_uptime(59), "0m");
    }

    #[test]
    fn garbage_output_does_not_panic() {
        // A host mid-reboot, or one whose shell printed a banner, must degrade to
        // zeroes rather than taking the status bar down.
        let (sample, _, _) = parse_linux("not what we expected at all", None).unwrap();
        assert_eq!(sample.memory_total_bytes, 0);
        assert_eq!(sample.memory_percent(), 0.0);
    }
}

#[cfg(test)]
mod msys_tests {
    use super::*;

    /// `/proc/meminfo` exactly as a real Windows host reports it through Git Bash.
    const MSYS_MEMINFO: &str = "cpu  100 0 100 1000 0 0 0 0
MemTotal:      134117944 kB
MemFree:         4030876 kB";

    #[test]
    fn a_procfs_without_memavailable_does_not_report_a_full_machine() {
        // Measured: MSYS2 emulates `/proc/meminfo` but omits `MemAvailable`, so
        // "available" parsed as zero and the host panel showed **100% memory
        // used** on an idle 134 GB machine. A wrong number presented as a fact
        // is worse than no number.
        let (sample, _, _) = parse_linux(MSYS_MEMINFO, None).unwrap();

        assert_eq!(sample.memory_total_bytes, 134_117_944 * 1024);
        assert_eq!(sample.memory_used_bytes, (134_117_944 - 4_030_876) * 1024);
        let used = sample.memory_used_bytes as f64 / sample.memory_total_bytes as f64;
        assert!(used < 0.98, "still reads as full: {:.1}%", used * 100.0);
    }

    #[test]
    fn memavailable_still_wins_where_it_exists() {
        // The fallback must not displace the honest figure on Linux, where
        // `MemAvailable` counts reclaimable cache and `MemFree` badly overstates
        // usage on any host that has been up for a while.
        let both = "cpu  100 0 100 1000 0 0 0 0
MemTotal:      1000 kB
MemFree:        100 kB
MemAvailable:   800 kB";
        let (sample, _, _) = parse_linux(both, None).unwrap();
        assert_eq!(sample.memory_used_bytes, 200 * 1024);
    }

    #[test]
    fn windows_takes_the_procfs_path() {
        // Git Bash gives a faithful `/proc/stat` and `/proc/meminfo`, so the
        // Linux collector is correct there — excluding Windows cost the host
        // panel entirely and bought nothing.
        assert!(Platform::Windows.has_procfs());
        assert!(Platform::Linux.has_procfs());
        assert!(!Platform::MacOs.has_procfs());
    }
}
