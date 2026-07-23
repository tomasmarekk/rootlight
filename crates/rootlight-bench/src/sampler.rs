//! Pluggable process-tree resource accounting for benchmark samples.
//!
//! The begin/end boundary encloses the complete parse call so a future
//! platform implementation can compute CPU deltas and interval peak RSS. The
//! portable default reports unavailable instead of parent-only telemetry.

use crate::EvidenceValue;

#[cfg(target_os = "linux")]
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Process-tree CPU and peak-memory evidence for one complete sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeMeasurement {
    /// Process-tree CPU nanoseconds accumulated during the sample.
    pub cpu_ns: EvidenceValue<u64>,
    /// Process-tree peak resident bytes observed during the sample.
    pub peak_rss_bytes: EvidenceValue<u64>,
}

/// An active process-tree measurement interval.
pub trait ProcessTreeSample {
    /// Ends the interval and returns its process-tree delta and peak evidence.
    fn finish(self) -> ProcessTreeMeasurement;
}

/// Begins scoped process-tree accounting without shell commands.
pub trait ProcessTreeSampler: Send + Sync {
    /// Concrete active interval returned by this sampler.
    type Sample: ProcessTreeSample;

    /// Begins accounting immediately before the parser call.
    fn begin(&self) -> Self::Sample;
}

/// Honest fallback used until an audited platform sampler is available.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableProcessTreeSampler;

/// Active unavailable interval preserving the same begin/end lifecycle.
#[derive(Debug, Clone, Copy)]
pub struct UnavailableProcessTreeSample;

impl ProcessTreeSampler for UnavailableProcessTreeSampler {
    type Sample = UnavailableProcessTreeSample;

    fn begin(&self) -> Self::Sample {
        UnavailableProcessTreeSample
    }
}

impl ProcessTreeSample for UnavailableProcessTreeSample {
    fn finish(self) -> ProcessTreeMeasurement {
        ProcessTreeMeasurement {
            cpu_ns: EvidenceValue::unavailable("process_tree_cpu_sampler_unavailable"),
            peak_rss_bytes: EvidenceValue::unavailable("process_tree_rss_sampler_unavailable"),
        }
    }
}

/// Linux `/proc` process-tree sampler using a bounded polling interval.
///
/// CPU is the sampled sum of user and system ticks for the root and live
/// descendants. RSS is the maximum sampled sum of resident pages. A child
/// that starts and exits between polls may be missed, so the method must be
/// reported as sampled rather than exact.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct LinuxProcTreeSampler {
    root_pid: u32,
    polling_interval: Duration,
    clock_ticks_per_second: u64,
    page_size_bytes: u64,
}

/// Linux `/proc` sampler configuration failure.
#[cfg(target_os = "linux")]
#[derive(Debug, thiserror::Error)]
pub enum LinuxProcTreeSamplerError {
    /// The sampling interval is zero.
    #[error("linux process-tree polling interval must be nonzero")]
    ZeroPollingInterval,
    /// `getconf` could not be executed.
    #[error("failed to execute getconf for {name}")]
    GetconfIo {
        /// Requested `getconf` variable.
        name: &'static str,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// `getconf` returned a failure or malformed value.
    #[error("getconf returned an invalid value for {0}")]
    InvalidGetconf(&'static str),
}

#[cfg(target_os = "linux")]
impl LinuxProcTreeSampler {
    /// Creates a sampler rooted at `root_pid`.
    ///
    /// # Errors
    ///
    /// Returns [`LinuxProcTreeSamplerError`] when the interval is zero or
    /// Linux clock/page units cannot be obtained without unsafe code.
    pub fn new(
        root_pid: u32,
        polling_interval: Duration,
    ) -> Result<Self, LinuxProcTreeSamplerError> {
        if polling_interval.is_zero() {
            return Err(LinuxProcTreeSamplerError::ZeroPollingInterval);
        }
        Ok(Self {
            root_pid,
            polling_interval,
            clock_ticks_per_second: getconf_u64("CLK_TCK")?,
            page_size_bytes: getconf_u64("PAGESIZE")?,
        })
    }

    /// Returns the configured polling interval.
    #[must_use]
    pub const fn polling_interval(&self) -> Duration {
        self.polling_interval
    }

    /// Returns the configured CPU clock resolution denominator.
    #[must_use]
    pub const fn clock_ticks_per_second(&self) -> u64 {
        self.clock_ticks_per_second
    }

    /// Returns the configured resident-page size.
    #[must_use]
    pub const fn page_size_bytes(&self) -> u64 {
        self.page_size_bytes
    }
}

/// Active bounded Linux `/proc` sampling interval.
#[cfg(target_os = "linux")]
pub struct LinuxProcTreeSample {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<ProcSamplingResult>>,
    clock_ticks_per_second: u64,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for LinuxProcTreeSample {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxProcTreeSample")
            .field("clock_ticks_per_second", &self.clock_ticks_per_second)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl ProcessTreeSampler for LinuxProcTreeSampler {
    type Sample = LinuxProcTreeSample;

    fn begin(&self) -> Self::Sample {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let root_pid = self.root_pid;
        let polling_interval = self.polling_interval;
        let page_size_bytes = self.page_size_bytes;
        let worker = thread::Builder::new()
            .name("rootlight-proc-tree-sampler".to_owned())
            .spawn(move || {
                sample_proc_tree(root_pid, polling_interval, page_size_bytes, &worker_stop)
            })
            .ok();
        LinuxProcTreeSample {
            stop,
            worker,
            clock_ticks_per_second: self.clock_ticks_per_second,
        }
    }
}

#[cfg(target_os = "linux")]
impl ProcessTreeSample for LinuxProcTreeSample {
    fn finish(mut self) -> ProcessTreeMeasurement {
        self.stop.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return unavailable_proc_measurement("process_tree_sampler_thread_unavailable");
        };
        let Ok(result) = worker.join() else {
            return unavailable_proc_measurement("process_tree_sampler_thread_failed");
        };
        let Some(start_cpu_ticks) = result.start_cpu_ticks else {
            return unavailable_proc_measurement("process_tree_root_unavailable");
        };
        let cpu_ticks = result.maximum_cpu_ticks.saturating_sub(start_cpu_ticks);
        let Some(cpu_ns) = ticks_to_ns(cpu_ticks, self.clock_ticks_per_second) else {
            return unavailable_proc_measurement("process_tree_cpu_conversion_overflow");
        };
        ProcessTreeMeasurement {
            cpu_ns: EvidenceValue::observed(cpu_ns),
            peak_rss_bytes: EvidenceValue::observed(result.peak_rss_bytes),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct ProcObservation {
    cpu_ticks: u64,
    rss_bytes: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Default)]
struct ProcSamplingResult {
    start_cpu_ticks: Option<u64>,
    maximum_cpu_ticks: u64,
    peak_rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn sample_proc_tree(
    root_pid: u32,
    polling_interval: Duration,
    page_size_bytes: u64,
    stop: &AtomicBool,
) -> ProcSamplingResult {
    let mut result = ProcSamplingResult::default();
    loop {
        if let Some(observation) = read_proc_tree(root_pid, page_size_bytes) {
            result.start_cpu_ticks.get_or_insert(observation.cpu_ticks);
            result.maximum_cpu_ticks = result.maximum_cpu_ticks.max(observation.cpu_ticks);
            result.peak_rss_bytes = result.peak_rss_bytes.max(observation.rss_bytes);
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(polling_interval);
    }
    result
}

#[cfg(target_os = "linux")]
fn read_proc_tree(root_pid: u32, page_size_bytes: u64) -> Option<ProcObservation> {
    let mut processes = BTreeMap::new();
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(parsed) = parse_proc_stat(&stat, page_size_bytes) else {
            continue;
        };
        processes.insert(pid, parsed);
    }
    if !processes.contains_key(&root_pid) {
        return None;
    }
    let mut included = BTreeSet::from([root_pid]);
    loop {
        let before = included.len();
        for (pid, process) in &processes {
            if included.contains(&process.parent_pid) {
                included.insert(*pid);
            }
        }
        if included.len() == before {
            break;
        }
    }
    let mut cpu_ticks = 0_u64;
    let mut rss_bytes = 0_u64;
    for pid in included {
        let process = processes.get(&pid)?;
        cpu_ticks = cpu_ticks.saturating_add(process.cpu_ticks);
        rss_bytes = rss_bytes.saturating_add(process.rss_bytes);
    }
    Some(ProcObservation {
        cpu_ticks,
        rss_bytes,
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct ProcStat {
    parent_pid: u32,
    cpu_ticks: u64,
    rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &str, page_size_bytes: u64) -> Option<ProcStat> {
    // `comm` may contain spaces or `)`, so the last closing parenthesis is the
    // only safe boundary before the fixed-position fields.
    let remainder = stat.get(stat.rfind(')')?.saturating_add(1)..)?.trim();
    let fields = remainder.split_ascii_whitespace().collect::<Vec<_>>();
    let parent_pid = fields.get(1)?.parse().ok()?;
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    let resident_pages = fields.get(21)?.parse::<u64>().ok()?;
    Some(ProcStat {
        parent_pid,
        cpu_ticks: user_ticks.saturating_add(system_ticks),
        rss_bytes: resident_pages.saturating_mul(page_size_bytes),
    })
}

#[cfg(target_os = "linux")]
fn ticks_to_ns(ticks: u64, clock_ticks_per_second: u64) -> Option<u64> {
    let nanos = u128::from(ticks)
        .checked_mul(1_000_000_000)?
        .checked_div(u128::from(clock_ticks_per_second))?;
    nanos.try_into().ok()
}

#[cfg(target_os = "linux")]
fn getconf_u64(name: &'static str) -> Result<u64, LinuxProcTreeSamplerError> {
    let output = Command::new("getconf")
        .arg(name)
        .output()
        .map_err(|source| LinuxProcTreeSamplerError::GetconfIo { name, source })?;
    if !output.status.success() {
        return Err(LinuxProcTreeSamplerError::InvalidGetconf(name));
    }
    let value = std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or(LinuxProcTreeSamplerError::InvalidGetconf(name))?;
    Ok(value)
}

#[cfg(target_os = "linux")]
fn unavailable_proc_measurement(reason: &str) -> ProcessTreeMeasurement {
    ProcessTreeMeasurement {
        cpu_ns: EvidenceValue::unavailable(reason),
        peak_rss_bytes: EvidenceValue::unavailable(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_sampler_never_mislabels_parent_metrics_as_process_tree_data() {
        let measurement = UnavailableProcessTreeSampler.begin().finish();

        assert!(matches!(
            measurement.cpu_ns,
            EvidenceValue::Unavailable { .. }
        ));
        assert!(matches!(
            measurement.peak_rss_bytes,
            EvidenceValue::Unavailable { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parser_handles_spaces_and_closing_parentheses_in_comm() {
        let stat = "42 (worker ) name) S 7 0 0 0 0 0 0 0 0 0 11 13 0 0 0 0 0 0 0 0 17";
        let parsed = parse_proc_stat(stat, 4_096).expect("stat parses");

        assert_eq!(parsed.parent_pid, 7);
        assert_eq!(parsed.cpu_ticks, 24);
        assert_eq!(parsed.rss_bytes, 17 * 4_096);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sampler_observes_current_process_without_unsafe_code() {
        let sampler = LinuxProcTreeSampler::new(std::process::id(), Duration::from_millis(1))
            .expect("Linux proc units are available");
        let sample = sampler.begin();
        let mut accumulator = 0_u64;
        for value in 0..100_000 {
            accumulator = accumulator.wrapping_add(value);
        }
        std::hint::black_box(accumulator);
        let measurement = sample.finish();

        assert!(matches!(measurement.cpu_ns, EvidenceValue::Observed { .. }));
        assert!(matches!(
            measurement.peak_rss_bytes,
            EvidenceValue::Observed { value } if value > 0
        ));
    }
}
