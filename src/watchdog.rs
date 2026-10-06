//! A deadline for waits on the compositor that have no timeout of their own
//! (Wayland roundtrips, the first configure). The tool runs from a hotkey:
//! a hung process would be invisible and keep the next press from working,
//! so on expiry it tells the user and exits.
//!
//! Disarmed by dropping it, which also joins its thread, so no watchdog
//! thread is alive when the clipboard server is forked.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Watchdog {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub fn arm(timeout: Duration, what: &'static str) -> Watchdog {
        let (stop, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("watchdog".into())
            .spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(timeout) {
                    crate::notify::show(&format!(
                        "{what}: the compositor stopped responding; nothing was copied"
                    ));
                    // SAFETY: ends the process at once; the main thread is
                    // stuck in a blocking call and holds nothing to flush.
                    unsafe { libc::_exit(1) }
                }
            })
            .ok();
        Watchdog {
            stop: Some(stop),
            thread,
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // Closing the channel wakes the thread with `Disconnected`.
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disarming_joins_quickly() {
        let t = std::time::Instant::now();
        drop(Watchdog::arm(Duration::from_secs(30), "test"));
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
