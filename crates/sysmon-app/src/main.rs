// SPDX-License-Identifier: GPL-3.0-or-later
//! One line over the library so integration tests can reach every
//! module; see lib.rs for the real dispatch.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(sysmon_app::run_cli() as u8)
}
