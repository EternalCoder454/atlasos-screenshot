//! The fuzz targets' library: the program's own source files, compiled into
//! this crate (the program is a binary, so there is no library to link), and
//! the invariants the property tests in `src/proptests.rs` check too.
//!
//! `src/main.rs` is left out; every module it declares is here, at the crate
//! root as `crate::<module>` (the modules name each other that way), from
//! the real files under `../../src`. The unit tests in those files are not
//! compiled (this crate is not built with `cfg(test)`).

#![allow(dead_code, unused)]

#[path = "../../src"]
mod app {
    pub mod actions;
    pub mod capture;
    pub mod cli;
    pub mod clipboard;
    pub mod config;
    pub mod countdown;
    pub mod dbus;
    pub mod harden;
    pub mod legacy;
    #[cfg(feature = "ocr")]
    pub mod models;
    pub mod modes;
    pub mod notify;
    pub mod ocr;
    pub mod overlay;
    pub mod pngin;
    pub mod post;
    pub mod redact;
    pub mod store;
    pub mod watchdog;
}

#[cfg(feature = "ocr")]
use app::models;
use app::{
    actions, capture, cli, clipboard, config, countdown, dbus, harden, legacy, modes, notify, ocr,
    overlay, pngin, post, redact, store, watchdog,
};

/// One function per fuzz target, each taking the fuzzer's bytes.
#[path = "../../src/checks.rs"]
pub mod checks;
