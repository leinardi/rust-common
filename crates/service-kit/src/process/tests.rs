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

//! The collector on a fixture `/proc` tree, whose every value is known, and on the real `/proc`,
//! where each family must be there with a plausible value. Both are encoded through a
//! prometheus-client `Registry` in the OpenMetrics text format, so the names asserted are the ones
//! a scrape shows.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use prometheus_client::encoding::text::encode;
use prometheus_client::registry::Registry;

use super::{ProcessCollector, auxv_page_size, page_size, parse_limits, parse_netstat, parse_stat};

/// The scrape's text for `collector`.
fn scrape(collector: ProcessCollector) -> String {
    let mut registry = Registry::default();
    registry.register_collector(Box::new(collector));
    let mut text = String::new();
    encode(&mut text, &registry).expect("encode");
    text
}

/// The `name value` sample lines of a scrape, by name.
fn samples(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let (name, value) = line.split_once(' ').expect("a sample line");
            (name.to_owned(), value.parse().expect("a number"))
        })
        .collect()
}

/// The `# TYPE` lines of a scrape, by family name.
fn types(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .map(|rest| {
            let (name, kind) = rest.split_once(' ').expect("a type line");
            (name.to_owned(), kind.to_owned())
        })
        .collect()
}

const STAT: &str = "123 (my (cmd) x) S 1 123 123 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 1 0 4000 \
                    1048576 100 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0\n";

const LIMITS: &str = "\
Limit                     Soft Limit           Hard Limit           Units
Max cpu time              unlimited            unlimited            seconds
Max open files            1024                 524288               files
Max address space         unlimited            unlimited            bytes
";

const NETSTAT: &str = "\
TcpExt: SyncookiesSent SyncookiesRecv
TcpExt: 0 0
IpExt: InNoRoutes InOctets OutOctets
IpExt: 0 1234 5678
";

/// A scratch `/proc` tree, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "service-kit-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("self/fd")).expect("tree");
        fs::create_dir_all(root.join("self/net")).expect("tree");
        Self(root)
    }

    fn write(&self, path: &str, text: &str) {
        fs::write(self.0.join(path), text).expect("write");
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> Tree {
    let tree = Tree::new();
    tree.write("self/stat", STAT);
    tree.write("stat", "cpu  1 2 3\nbtime 1700000000\nprocesses 5\n");
    tree.write("self/limits", LIMITS);
    for name in ["0", "1", "2", "3"] {
        tree.write(&format!("self/fd/{name}"), "");
    }
    tree.write("self/net/netstat", NETSTAT);
    tree
}

/// Every value of the fixture comes out as procfs reads it, under the Go client's names.
#[cfg(target_os = "linux")]
#[test]
fn the_fixture_tree_is_encoded_with_the_go_clients_names_and_values() {
    let tree = fixture();
    let text = scrape(ProcessCollector::with_root(tree.root(), false));
    let samples = samples(&text);
    let page_size = page_size().expect("a page size");
    let want = [
        ("process_cpu_seconds_total", 3.0),
        ("process_open_fds", 4.0),
        ("process_max_fds", 1024.0),
        ("process_virtual_memory_bytes", 1_048_576.0),
        ("process_virtual_memory_max_bytes", super::to_f64(u64::MAX)),
        (
            "process_resident_memory_bytes",
            super::to_f64(100 * page_size),
        ),
        ("process_start_time_seconds", 1_700_000_040.0),
        ("process_network_receive_bytes_total", 1234.0),
        ("process_network_transmit_bytes_total", 5678.0),
    ];
    assert_eq!(samples.len(), want.len(), "{text}");
    for (name, value) in want {
        assert_eq!(samples.get(name), Some(&value), "{name}\n{text}");
    }
    let types = types(&text);
    for (name, kind) in [
        ("process_cpu_seconds", "counter"),
        ("process_open_fds", "gauge"),
        ("process_max_fds", "gauge"),
        ("process_virtual_memory_bytes", "gauge"),
        ("process_virtual_memory_max_bytes", "gauge"),
        ("process_resident_memory_bytes", "gauge"),
        ("process_start_time_seconds", "gauge"),
        ("process_network_receive_bytes", "counter"),
        ("process_network_transmit_bytes", "counter"),
    ] {
        assert_eq!(types.get(name).map(String::as_str), Some(kind), "{name}");
    }
    assert!(
        text.contains(
            "# HELP process_cpu_seconds Total user and system CPU time spent in seconds."
        ),
        "{text}"
    );
    assert!(!text.contains("# UNIT"), "{text}");
}

/// A file that is missing or unreadable drops only the families it feeds.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_file_drops_only_its_families() {
    let tree = fixture();
    fs::remove_file(tree.root().join("self/net/netstat")).expect("remove");
    fs::remove_file(tree.root().join("stat")).expect("remove");
    tree.write("self/limits", "garbage\nMax open files  x  y files\n");
    let samples = samples(&scrape(ProcessCollector::with_root(tree.root(), false)));
    let names: Vec<&str> = samples.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "process_cpu_seconds_total",
            "process_open_fds",
            "process_resident_memory_bytes",
            "process_virtual_memory_bytes",
        ]
    );
}

/// A netstat without the `IpExt` octets counts 0, as procfs's zero values do.
#[cfg(target_os = "linux")]
#[test]
fn netstat_without_octets_is_zero() {
    let tree = fixture();
    tree.write("self/net/netstat", "TcpExt: A\nTcpExt: 1\n");
    let samples = samples(&scrape(ProcessCollector::with_root(tree.root(), false)));
    assert_eq!(
        samples.get("process_network_receive_bytes_total"),
        Some(&0.0)
    );
    assert_eq!(
        samples.get("process_network_transmit_bytes_total"),
        Some(&0.0)
    );
}

#[test]
fn stat_is_read_past_a_command_name_with_parentheses() {
    let stat = parse_stat(STAT).expect("stat");
    assert_eq!(
        (
            stat.utime,
            stat.stime,
            stat.start_ticks,
            stat.vsize,
            stat.rss_pages
        ),
        (250, 50, 4000, 1_048_576, 100)
    );
    assert_eq!(parse_stat("1 (a) S 1 2"), None);
    assert_eq!(parse_stat("no parentheses"), None);
}

#[test]
fn limits_read_the_soft_values() {
    let limits = parse_limits(LIMITS).expect("limits");
    assert_eq!((limits.open_files, limits.address_space), (1024, u64::MAX));
    assert_eq!(parse_limits("h\nMax open files  -1  2 files\n"), None);
}

#[test]
fn netstat_pairs_must_line_up() {
    assert_eq!(parse_netstat(NETSTAT), Some((Some(1234.0), Some(5678.0))));
    assert_eq!(parse_netstat("IpExt: A B\nIpExt: 1\n"), None);
    assert_eq!(parse_netstat("IpExt: InOctets\nIpExt: x\n"), None);
}

#[test]
fn the_page_size_is_found_in_the_auxiliary_vector() {
    let words = |pairs: &[(usize, usize)]| -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(key, value)| [key.to_ne_bytes(), value.to_ne_bytes()].concat())
            .collect()
    };
    assert_eq!(
        auxv_page_size(&words(&[(3, 7), (6, 4096), (0, 0)])),
        Some(4096)
    );
    assert_eq!(auxv_page_size(&words(&[(3, 7), (0, 0), (6, 4096)])), None);
    assert_eq!(auxv_page_size(&words(&[(3, 7)])), None);
}

/// On the real `/proc` every family is there, typed as Go's, with a value that fits this process.
#[cfg(target_os = "linux")]
#[test]
fn the_real_proc_gives_every_family_a_plausible_value() {
    let text = scrape(ProcessCollector::new());
    let samples = samples(&text);
    let value = |name: &str| {
        *samples
            .get(name)
            .unwrap_or_else(|| panic!("{name}\n{text}"))
    };
    assert!(value("process_cpu_seconds_total") >= 0.0);
    assert!(value("process_open_fds") >= 3.0);
    assert!(value("process_max_fds") >= value("process_open_fds"));
    let resident = value("process_resident_memory_bytes");
    assert!((1.0e5..1.0e12).contains(&resident), "{resident}");
    assert!(value("process_virtual_memory_bytes") >= resident);
    assert!(value("process_virtual_memory_max_bytes") > 0.0);
    assert!(value("process_network_receive_bytes_total") >= 0.0);
    assert!(value("process_network_transmit_bytes_total") >= 0.0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs_f64();
    let start = value("process_start_time_seconds");
    assert!(
        (now - 86_400.0..=now + 5.0).contains(&start),
        "{start} vs {now}"
    );
    let types = types(&text);
    assert_eq!(types.len(), 9, "{types:?}");
    assert_eq!(
        types.get("process_cpu_seconds").map(String::as_str),
        Some("counter")
    );
    assert_eq!(
        types.get("process_open_fds").map(String::as_str),
        Some("gauge")
    );
}

/// Elsewhere than Linux the collector encodes nothing.
#[cfg(not(target_os = "linux"))]
#[test]
fn nothing_is_encoded_off_linux() {
    let text = scrape(ProcessCollector::new());
    assert!(samples(&text).is_empty(), "{text}");
}
