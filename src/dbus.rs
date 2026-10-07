//! `org.kde.Spectacle` for programs that ask Spectacle for screenshots
//! (`dbus-send ... org.kde.Spectacle.FullScreen`, KDE Connect's remote
//! screenshot, scripts). `telamon-screenshot --dbus` is what the D-Bus
//! activation file starts: it owns the name, takes each requested shot by
//! running this program (so every shot is a normal one: saved, copied,
//! notified), answers with `ScreenshotTaken(path)` or `ScreenshotFailed`,
//! and exits when it has been idle for a while.
//!
//! Spectacle's interface, as in `org.kde.Spectacle.xml`: the capture methods
//! take `-1` for "use the setting", `0` for no and `1` for yes. Recording
//! isn't supported: the `Record*` methods answer `RecordingFailed`.

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;

const NAME: &str = "org.kde.Spectacle";
const PATH: &str = "/";
const IDLE_EXIT: Duration = Duration::from_secs(15);

struct Service {
    shared: Arc<Shared>,
}

struct Shared {
    conn: Mutex<Option<Connection>>,
    running: AtomicUsize,
    last: Mutex<Instant>,
}

/// `-1` leaves the setting alone; `0` and `1` are no and yes.
fn tri(v: i32, yes: &'static str, no: &'static str) -> Option<&'static str> {
    match v {
        0 => Some(no),
        1 => Some(yes),
        _ => None,
    }
}

/// The command line a method becomes.
pub fn argv(what: &'static str, pointer: i32, frame: i32, shadow: i32) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![what.into()];
    for flag in [
        tri(pointer, "--cursor", "--no-cursor"),
        tri(frame, "--frame", "--no-frame"),
        tri(shadow, "--shadow", "--no-shadow"),
    ]
    .into_iter()
    .flatten()
    {
        a.push(flag.into());
    }
    a
}

impl Shared {
    fn emit(&self, signal: &str, text: &str) {
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = guard.as_ref() {
            let _ = conn.emit_signal(None::<&str>, PATH, NAME, signal, &(text,));
        }
    }

    fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// Runs one capture and answers with the signal.
    fn capture(self: &Arc<Self>, args: Vec<OsString>) {
        // One at a time, as the program itself allows: a caller in a loop
        // gets failures, not a process and a notification each.
        if self
            .running
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            self.emit("ScreenshotFailed", "A screenshot is already in progress");
            return;
        }
        let exe = crate::actions::self_exe();
        let child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(child) = child else {
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.emit("ScreenshotFailed", "Screenshot capture couldn't be started");
            return;
        };
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let out = child.wait_with_output();
            match out {
                Ok(o) if o.status.success() => {
                    let path = String::from_utf8_lossy(&o.stdout);
                    me.emit("ScreenshotTaken", path.lines().next().unwrap_or(""));
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let why = err
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty() && !l.starts_with("Usage"))
                        .map(|l| l.trim_start_matches("telamon-screenshot: ").to_string())
                        .unwrap_or_else(|| "Screenshot capture canceled or failed".into());
                    me.emit("ScreenshotFailed", &why);
                }
                Err(_) => me.emit("ScreenshotFailed", "Screenshot capture failed"),
            }
            me.touch();
            me.running.fetch_sub(1, Ordering::SeqCst);
        });
    }

    fn editor(&self) {
        // One editor from here at a time.
        static OPEN: AtomicBool = AtomicBool::new(false);
        if OPEN.swap(true, Ordering::SeqCst) {
            return;
        }
        let exe = crate::actions::self_exe();
        let editor = crate::actions::editor_path(&exe);
        let editor = Command::new(editor)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match editor {
            Ok(mut c) => {
                std::thread::spawn(move || {
                    let _ = c.wait();
                    OPEN.store(false, Ordering::SeqCst);
                });
            }
            Err(_) => OPEN.store(false, Ordering::SeqCst),
        }
    }
}

#[zbus::interface(name = "org.kde.Spectacle")]
impl Service {
    fn start_agent(&self) {
        self.shared.editor();
        self.shared.touch();
    }

    fn open_without_screenshot(&self) {
        self.shared.editor();
        self.shared.touch();
    }

    fn full_screen(&self, include_mouse_pointer: i32) {
        self.go("--full", include_mouse_pointer, -1, -1);
    }

    fn current_screen(&self, include_mouse_pointer: i32) {
        self.go("--screen", include_mouse_pointer, -1, -1);
    }

    fn active_window(
        &self,
        include_window_decorations: i32,
        include_mouse_pointer: i32,
        include_window_shadow: i32,
    ) {
        self.go(
            "--active-window",
            include_mouse_pointer,
            include_window_decorations,
            include_window_shadow,
        );
    }

    fn window_under_cursor(
        &self,
        include_window_decorations: i32,
        include_mouse_pointer: i32,
        include_window_shadow: i32,
    ) {
        // KWin has no "window under the pointer" call: the user clicks it.
        self.go(
            "--window",
            include_mouse_pointer,
            include_window_decorations,
            include_window_shadow,
        );
    }

    fn rectangular_region(&self, include_mouse_pointer: i32) {
        self.go("--region", include_mouse_pointer, -1, -1);
    }

    fn record_region(&self, _include_mouse_pointer: i32) {
        self.no_recording();
    }

    fn record_screen(&self, _include_mouse_pointer: i32) {
        self.no_recording();
    }

    fn record_window(&self, _include_mouse_pointer: i32) {
        self.no_recording();
    }
}

impl Service {
    fn go(&self, what: &'static str, pointer: i32, frame: i32, shadow: i32) {
        self.shared.touch();
        self.shared.capture(argv(what, pointer, frame, shadow));
    }

    fn no_recording(&self) {
        self.shared.touch();
        self.shared
            .emit("RecordingFailed", "Screen recording isn't supported");
    }
}

/// Owns the name and serves until idle. `Err` when the name is taken (the
/// real Spectacle is running) or there is no session bus.
pub fn serve() -> Result<(), String> {
    let shared = Arc::new(Shared {
        conn: Mutex::new(None),
        running: AtomicUsize::new(0),
        last: Mutex::new(Instant::now()),
    });
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(NAME))
        .and_then(|b| {
            b.serve_at(
                PATH,
                Service {
                    shared: Arc::clone(&shared),
                },
            )
        })
        .and_then(|b| b.build())
        .map_err(|e| format!("can't serve {NAME}: {e}"))?;
    *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let idle = shared
            .last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed();
        if shared.running.load(Ordering::SeqCst) == 0 && idle > IDLE_EXIT {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn methods_become_command_lines() {
        assert_eq!(argv("--full", -1, -1, -1), a(&["--full"]));
        assert_eq!(argv("--full", 1, -1, -1), a(&["--full", "--cursor"]));
        assert_eq!(argv("--screen", 0, -1, -1), a(&["--screen", "--no-cursor"]));
        assert_eq!(
            argv("--active-window", 1, 0, 0),
            a(&["--active-window", "--cursor", "--no-frame", "--no-shadow"])
        );
        assert_eq!(
            argv("--window", -1, 1, 1),
            a(&["--window", "--frame", "--shadow"])
        );
        // Any other number is "use the setting".
        assert_eq!(argv("--region", 7, -5, 2), a(&["--region"]));
    }

    #[test]
    fn every_command_line_parses() {
        for (what, p, f, s) in [
            ("--full", 1, -1, -1),
            ("--screen", 0, -1, -1),
            ("--active-window", 1, 0, 0),
            ("--window", -1, 1, 1),
            ("--region", 1, -1, -1),
        ] {
            let args = argv(what, p, f, s)
                .into_iter()
                .map(|s| s.into_string().unwrap());
            assert!(matches!(
                crate::cli::parse_args(args),
                Ok(crate::cli::Action::Run(_))
            ));
        }
    }
}
