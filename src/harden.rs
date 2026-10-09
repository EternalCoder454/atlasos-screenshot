//! Process hardening that is safe for a program KWin identifies by
//! `/proc/<pid>/exe`.
//!
//! The process holds the screen's pixels (and OCR'd text) in memory, so a
//! crash must not write them to a core file or hand them to a crash
//! collector such as systemd-coredump: the core size limit goes to 0, for
//! this process and for everything it starts.
//!
//! `prctl(PR_SET_DUMPABLE, 0)` would also keep other processes of the user
//! from reading this one's memory, but it makes `/proc/<pid>/exe` unreadable
//! to them, and KWin reads exactly that to decide who may use ScreenShot2.
//! It is not used for that reason; see docs/SECURITY.md.

use rustix::process::{Resource, Rlimit, setrlimit};

/// Sets the core file size limit, soft and hard, to 0. Failing to is not an
/// error worth stopping a screenshot for: the limit may already be 0.
pub fn no_core_dumps() {
    let _ = setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::process::getrlimit;

    #[test]
    fn core_dumps_are_off_for_good() {
        no_core_dumps();
        let l = getrlimit(Resource::Core);
        assert_eq!((l.current, l.maximum), (Some(0), Some(0)));
        // The hard limit is 0 as well, so it can't be raised again.
        let raise = setrlimit(
            Resource::Core,
            Rlimit {
                current: Some(1 << 20),
                maximum: Some(1 << 20),
            },
        );
        assert!(raise.is_err() || getrlimit(Resource::Core).current == Some(0));
    }
}
