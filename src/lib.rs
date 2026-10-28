// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(unix), allow(unused))]
#[cfg(not(unix))]
compile_error!("Timber 0.1 supports macOS and Linux only");
pub mod capture;
pub mod convention;
pub mod dependencies;
pub mod fsx;
pub mod gc;
pub mod git;
pub mod inspect;
pub mod model;
pub mod planning;
pub mod preview;
pub mod recovery;
pub mod report;
pub mod restore;
pub mod store;
