/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under either of
 *
 *  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
 *  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! The Prometheus Go client's process collector for [prometheus-client], read from `/proc` on
//! Linux: the nine `process_*` families, with the names, types, units and help texts of the earlier go-prom's
//! `process::ProcessCollector` (the Go client v1.24.1 over procfs v0.21.1). Elsewhere the
//! collector encodes nothing.
//!
//! prometheus-client names a counter's sample `<name>_total`, so the counters are registered as
//! `process_cpu_seconds`, `process_network_receive_bytes` and `process_network_transmit_bytes`, and
//! encode as `process_cpu_seconds_total` and so on; no unit is declared, so no `# UNIT` line and
//! no suffix is added. Gauges keep their names.
//!
//! Each value comes from where procfs reads it, with procfs's quirks:
//!
//! - CPU time and the start time count in procfs's hard-coded 100 ticks per second, not the
//!   system's `CLK_TCK`;
//! - RSS is the page count in `/proc/self/stat` times the page size, which is the `AT_PAGESZ` entry
//!   of `/proc/self/auxv` (this crate forbids the `sysconf` call);
//! - `process_open_fds` is the `st_size` of `/proc/self/fd` (Linux 6.2 and later report the
//!   descriptor count there), counting the entries only where it is 0 or the tree is not the real
//!   `/proc`;
//! - the network counters are `IpExt` `InOctets` and `OutOctets` of `/proc/self/net/netstat`, which
//!   count the whole network namespace, not the process; 0 when the file has no such line;
//! - an `unlimited` limit is `u64::MAX`, as procfs reads it.
//!
//! Errors are not reported, as with the Go client's default `ReportErrors: false`: a file that
//! cannot be read or parsed drops the families it feeds from that scrape (`stat` the CPU, memory
//! and start time; `/proc/stat` the start time only; `fd`, `limits` and `net/netstat` theirs).
//!
//! The parsers are simplified from the earlier go-prom's, which also reproduce `fmt.Fscan` and procfs's
//! all-or-nothing reads: a malformed field that these parsers do not read no longer fails the
//! whole file.
//!
//! Extracted from rust-common's crates/go-prom/src/process.rs at 9825e08 (itself extracted from
//! swarm-scheduler-exporter/src/metrics/process.rs at c9e79afa8810), onto prometheus-client.
//!
//! [prometheus-client]: https://docs.rs/prometheus-client

use std::path::{Path, PathBuf};

use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric as _};
use prometheus_client::metrics::counter::ConstCounter;
use prometheus_client::metrics::gauge::ConstGauge;

/// procfs's `userHZ`.
const USER_HZ: f64 = 100.0;

/// procfs's value for `unlimited` in `/proc/self/limits`.
const UNLIMITED: u64 = u64::MAX;

/// The Prometheus Go client's `NewProcessCollector` with no namespace, on prometheus-client.
///
/// ```
/// use prometheus_client::registry::Registry;
/// use service_kit::process::ProcessCollector;
///
/// let mut registry = Registry::default();
/// registry.register_collector(Box::new(ProcessCollector::new()));
/// ```
#[derive(Debug, Clone)]
pub struct ProcessCollector {
    root: PathBuf,
    /// procfs's `isReal`: the fd fast path only applies to the kernel's own `/proc`.
    real: bool,
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCollector {
    /// The collector of this process, reading `/proc`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_root(Path::new("/proc"), true)
    }

    /// The collector reading a `/proc`-like tree at `root`, for tests; `real` says whether it is
    /// the kernel's `/proc`.
    fn with_root(root: &Path, real: bool) -> Self {
        Self {
            root: root.to_owned(),
            real,
        }
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// procfs's `FileDescriptorsLen`.
    fn open_fds(&self) -> Option<u64> {
        let path = self.root.join("self/fd");
        if self.real {
            let size = std::fs::metadata(&path).ok()?.len();
            if size > 0 {
                return Some(size);
            }
        }
        let mut count = 0_u64;
        for entry in std::fs::read_dir(&path).ok()? {
            entry.ok()?;
            count += 1;
        }
        Some(count)
    }

    /// Every value that could be read, in the Prometheus Go client's order.
    fn sample(&self) -> Sample {
        let mut sample = Sample::default();
        if !cfg!(target_os = "linux") {
            return sample;
        }
        if let Some(stat) = self.read("self/stat").as_deref().and_then(parse_stat) {
            sample.cpu_seconds = Some(to_f64(stat.utime.saturating_add(stat.stime)) / USER_HZ);
            sample.virtual_bytes = Some(to_f64(stat.vsize));
            if let Some(page_size) = page_size() {
                sample.resident_bytes = Some(to_f64(stat.rss_pages.saturating_mul(page_size)));
            }
            if let Some(boot_time) = self.read("stat").as_deref().and_then(parse_boot_time) {
                sample.start_time = Some(to_f64(boot_time) + to_f64(stat.start_ticks) / USER_HZ);
            }
        }
        sample.open_fds = self.open_fds().map(to_f64);
        if let Some(limits) = self.read("self/limits").as_deref().and_then(parse_limits) {
            sample.max_fds = Some(to_f64(limits.open_files));
            sample.max_virtual_bytes = Some(to_f64(limits.address_space));
        }
        if let Some((received, transmitted)) = self
            .read("self/net/netstat")
            .as_deref()
            .and_then(parse_netstat)
        {
            sample.network_receive_bytes = Some(received.unwrap_or(0.0));
            sample.network_transmit_bytes = Some(transmitted.unwrap_or(0.0));
        }
        sample
    }
}

/// `value` as the float Go's `float64(v)` makes of it, which rounds the same way.
#[expect(
    clippy::cast_precision_loss,
    reason = "Go converts the same integers to float64, rounding to nearest"
)]
fn to_f64(value: u64) -> f64 {
    value as f64
}

/// The values of one scrape; a value that could not be read is left out.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Sample {
    cpu_seconds: Option<f64>,
    open_fds: Option<f64>,
    max_fds: Option<f64>,
    virtual_bytes: Option<f64>,
    max_virtual_bytes: Option<f64>,
    resident_bytes: Option<f64>,
    start_time: Option<f64>,
    network_receive_bytes: Option<f64>,
    network_transmit_bytes: Option<f64>,
}

fn counter(
    encoder: &mut DescriptorEncoder<'_>,
    name: &str,
    help: &str,
    value: Option<f64>,
) -> Result<(), std::fmt::Error> {
    let Some(value) = value else {
        return Ok(());
    };
    let metric = ConstCounter::new(value);
    metric.encode(encoder.encode_descriptor(name, help, None, metric.metric_type())?)
}

fn gauge(
    encoder: &mut DescriptorEncoder<'_>,
    name: &str,
    help: &str,
    value: Option<f64>,
) -> Result<(), std::fmt::Error> {
    let Some(value) = value else {
        return Ok(());
    };
    let metric = ConstGauge::new(value);
    metric.encode(encoder.encode_descriptor(name, help, None, metric.metric_type())?)
}

impl Collector for ProcessCollector {
    fn encode(&self, mut encoder: DescriptorEncoder<'_>) -> Result<(), std::fmt::Error> {
        let sample = self.sample();
        let out = &mut encoder;
        counter(
            out,
            "process_cpu_seconds",
            "Total user and system CPU time spent in seconds.",
            sample.cpu_seconds,
        )?;
        gauge(
            out,
            "process_open_fds",
            "Number of open file descriptors.",
            sample.open_fds,
        )?;
        gauge(
            out,
            "process_max_fds",
            "Maximum number of open file descriptors.",
            sample.max_fds,
        )?;
        gauge(
            out,
            "process_virtual_memory_bytes",
            "Virtual memory size in bytes.",
            sample.virtual_bytes,
        )?;
        gauge(
            out,
            "process_virtual_memory_max_bytes",
            "Maximum amount of virtual memory available in bytes.",
            sample.max_virtual_bytes,
        )?;
        gauge(
            out,
            "process_resident_memory_bytes",
            "Resident memory size in bytes.",
            sample.resident_bytes,
        )?;
        gauge(
            out,
            "process_start_time_seconds",
            "Start time of the process since unix epoch in seconds.",
            sample.start_time,
        )?;
        counter(
            out,
            "process_network_receive_bytes",
            "Number of bytes received by the process over the network.",
            sample.network_receive_bytes,
        )?;
        counter(
            out,
            "process_network_transmit_bytes",
            "Number of bytes sent by the process over the network.",
            sample.network_transmit_bytes,
        )
    }
}

/// The page size: `AT_PAGESZ` (6) in `/proc/self/auxv`, native-endian word pairs, read once.
fn page_size() -> Option<u64> {
    static PAGE_SIZE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *PAGE_SIZE.get_or_init(|| {
        std::fs::read("/proc/self/auxv")
            .ok()
            .and_then(|auxv| auxv_page_size(&auxv))
    })
}

/// `AT_PAGESZ` among `auxv`'s `(key, value)` words; the vector ends at `AT_NULL` (0).
fn auxv_page_size(auxv: &[u8]) -> Option<u64> {
    const AT_NULL: usize = 0;
    const AT_PAGESZ: usize = 6;
    let width = size_of::<usize>();
    for pair in auxv.chunks_exact(width * 2) {
        let (key, value) = pair.split_at(width);
        let key = usize::from_ne_bytes(key.try_into().ok()?);
        let value = usize::from_ne_bytes(value.try_into().ok()?);
        match key {
            AT_NULL => return None,
            AT_PAGESZ => return u64::try_from(value).ok(),
            _ => {}
        }
    }
    None
}

/// The fields of `/proc/self/stat` the collector uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    utime: u64,
    stime: u64,
    start_ticks: u64,
    vsize: u64,
    rss_pages: u64,
}

/// The fields after the last `)`, which ends the command name (itself free to hold spaces and
/// parentheses): the state first, then `utime`, `stime`, `starttime`, `vsize` and `rss` at
/// proc(5)'s fields 14, 15, 22, 23 and 24.
fn parse_stat(data: &str) -> Option<Stat> {
    data.find('(')?;
    let close = data.rfind(')')?;
    let fields: Vec<&str> = data.get(close + 2..)?.split_ascii_whitespace().collect();
    let field = |number: usize| -> Option<u64> { fields.get(number - 3)?.parse().ok() };
    Some(Stat {
        utime: field(14)?,
        stime: field(15)?,
        start_ticks: field(22)?,
        vsize: field(23)?,
        rss_pages: field(24)?,
    })
}

/// The `btime` line of `/proc/stat`: 0 without such a line, a parse error fails it; a later line
/// wins, as procfs's.
fn parse_boot_time(data: &str) -> Option<u64> {
    let mut boot_time = 0;
    for line in data.lines() {
        let mut parts = line.split_ascii_whitespace();
        if parts.next() == Some("btime")
            && let Some(value) = parts.next()
        {
            boot_time = parse_uint(value)?;
        }
    }
    Some(boot_time)
}

/// `strconv.ParseUint(s, 10, 64)`: decimal digits only, no sign.
fn parse_uint(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The limits the collector uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    open_files: u64,
    address_space: u64,
}

/// The soft limits of `Max open files` and `Max address space`, from lines of
/// `name  soft  hard [units]`, the name ended by a run of two spaces; 0 for one that is missing,
/// and `None` when either is neither `unlimited` nor a decimal number.
fn parse_limits(data: &str) -> Option<Limits> {
    let mut limits = Limits {
        open_files: 0,
        address_space: 0,
    };
    for line in data.lines().skip(1) {
        let Some((name, rest)) = line.split_once("  ") else {
            continue;
        };
        let soft = rest.split_ascii_whitespace().next()?;
        match name.trim() {
            "Max open files" => limits.open_files = parse_limit(soft)?,
            "Max address space" => limits.address_space = parse_limit(soft)?,
            _ => {}
        }
    }
    Some(limits)
}

fn parse_limit(text: &str) -> Option<u64> {
    if text == "unlimited" {
        return Some(UNLIMITED);
    }
    parse_uint(text)
}

/// `IpExt`'s `InOctets` and `OutOctets` from `/proc/self/net/netstat`: lines in pairs, names then
/// values, split on single spaces, the first field the protocol with its colon; a pair whose field
/// counts differ, or a value that is not a number, fails the read.
fn parse_netstat(data: &str) -> Option<(Option<f64>, Option<f64>)> {
    let (mut received, mut transmitted) = (None, None);
    let mut lines = data.lines();
    while let Some(names) = lines.next() {
        let values = lines.next().unwrap_or("");
        let names: Vec<&str> = names.split(' ').collect();
        let values: Vec<&str> = values.split(' ').collect();
        if names.len() != values.len() {
            return None;
        }
        let protocol = names[0].strip_suffix(':').unwrap_or(names[0]);
        for (name, value) in names.iter().zip(&values).skip(1) {
            let value: f64 = value.parse().ok()?;
            if protocol == "IpExt" {
                match *name {
                    "InOctets" => received = Some(value),
                    "OutOctets" => transmitted = Some(value),
                    _ => {}
                }
            }
        }
    }
    Some((received, transmitted))
}

#[cfg(test)]
mod tests;
