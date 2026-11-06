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

//! `install_global` installs once; a second install is refused with the shared error. A test binary
//! of its own, since the global default is process-wide.
//!
//! Written for this crate; awtrix-controller's and swarm-rootless-docker's `init` map the same
//! refusal to their own messages.

use tracing::Level;
use tracing_setup::testing::Capture;
use tracing_setup::{Format, Options, build, install_global};

#[test]
fn the_global_default_installs_once() {
    let capture = Capture::default();
    install_global(build(
        capture.clone(),
        Format::Json,
        Level::INFO,
        Options::new(),
    ))
    .expect("the first install");
    std::thread::spawn(|| tracing::info!("from another thread"))
        .join()
        .expect("the thread runs");
    assert_eq!(capture.json_lines()[0]["message"], "from another thread");

    let second = install_global(build(
        Capture::default(),
        Format::Text,
        Level::INFO,
        Options::new(),
    ));
    let error = second.expect_err("a second install");
    assert_eq!(error.to_string(), "a global logger is already installed");
}
