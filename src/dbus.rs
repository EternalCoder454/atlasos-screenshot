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
//!
//! Any program on the user's session bus may call these methods, so a
//! capture asked for this way is never silent, whatever `output.notify` says:
//! the capture that needs no gesture (whole desktop, a screen, the active
//! window) shows the countdown badge for a second first, every capture ends
//! in a notification that names the program that asked, and requests come at
//! most once every few seconds. The caller gets back only the path of the
//! file in `ScreenshotTaken`, never the picture, and no argument of a method
//! reaches the command line except as one of the fixed flags below.

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;

const NAME: &str = "org.kde.Spectacle";
const PATH: &str = "/";
const IDLE_EXIT: Duration = Duration::from_secs(15);
/// The least time between two requests that start a capture.
const MIN_GAP: Duration = Duration::from_secs(3);
/// The countdown shown before a capture that asks nothing of the user.
const VISIBLE_DELAY: &str = "1";
/// What a caller is called when its program can't be found out, and when it
/// is a Flatpak's D-Bus proxy (the proxy's process is not the app's).
const UNKNOWN: &str = "an unknown program";
const SANDBOXED: &str = "a sandboxed app";

struct Service {
    shared: Arc<Shared>,
}

struct Shared {
    conn: Mutex<Option<Connection>>,
    running: AtomicUsize,
    last: Mutex<Instant>,
    limiter: Mutex<Limiter>,
}

/// Lets one request through per `MIN_GAP`.
#[derive(Default)]
struct Limiter {
    last: Option<Instant>,
}

impl Limiter {
    fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(t) if now.saturating_duration_since(t) < MIN_GAP => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// The name of a program for the notification, from its `comm` (at most 15
/// bytes the program chose itself, so anything at all): letters, digits and a
/// few marks as they are, every other byte a `?`.
fn label_from_comm(comm: &[u8]) -> String {
    let name: String = comm
        .iter()
        .take(15)
        .take_while(|&&b| b != b'\n' && b != 0)
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'+' | b' ' => b as char,
            _ => '?',
        })
        .collect();
    let name = name.trim();
    if name.is_empty() {
        UNKNOWN.to_string()
    } else if name == "xdg-dbus-proxy" {
        SANDBOXED.to_string()
    } else {
        name.to_string()
    }
}

/// Who owns the connection that called: the bus daemon says which process,
/// `/proc` what it is called.
async fn caller(conn: &zbus::Connection, hdr: &zbus::message::Header<'_>) -> String {
    let who = async {
        let sender = hdr.sender()?.clone();
        let proxy = zbus::fdo::DBusProxy::new(conn).await.ok()?;
        let creds = proxy
            .get_connection_credentials(zbus::names::BusName::from(sender))
            .await
            .ok()?;
        let pid = creds.process_id()?;
        let comm = std::fs::read(format!("/proc/{pid}/comm")).ok()?;
        Some(label_from_comm(&comm))
    };
    who.await.unwrap_or_else(|| UNKNOWN.to_string())
}

/// `-1` leaves the setting alone; `0` and `1` are no and yes.
fn tri(v: i32, yes: &'static str, no: &'static str) -> Option<&'static str> {
    match v {
        0 => Some(no),
        1 => Some(yes),
        _ => None,
    }
}

/// The command line a method becomes: the fixed flags of the method, always
/// `--notify`, a one second countdown unless the user picks something on
/// screen anyway, and who asked.
pub fn argv(
    what: &'static str,
    pointer: i32,
    frame: i32,
    shadow: i32,
    requested_by: &str,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![what.into(), "--notify".into()];
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
    if !matches!(what, "--region" | "--window") {
        a.extend(["--delay".into(), VISIBLE_DELAY.into()]);
    }
    a.extend(["--requested-by".into(), requested_by.into()]);
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
        if !self
            .limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allow(Instant::now())
        {
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.emit(
                "ScreenshotFailed",
                "Screenshots over D-Bus are limited to one every few seconds",
            );
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

    async fn full_screen(
        &self,
        include_mouse_pointer: i32,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) {
        let who = caller(conn, &hdr).await;
        self.go("--full", include_mouse_pointer, -1, -1, &who);
    }

    async fn current_screen(
        &self,
        include_mouse_pointer: i32,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) {
        let who = caller(conn, &hdr).await;
        self.go("--screen", include_mouse_pointer, -1, -1, &who);
    }

    async fn active_window(
        &self,
        include_window_decorations: i32,
        include_mouse_pointer: i32,
        include_window_shadow: i32,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) {
        let who = caller(conn, &hdr).await;
        self.go(
            "--active-window",
            include_mouse_pointer,
            include_window_decorations,
            include_window_shadow,
            &who,
        );
    }

    async fn window_under_cursor(
        &self,
        include_window_decorations: i32,
        include_mouse_pointer: i32,
        include_window_shadow: i32,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) {
        let who = caller(conn, &hdr).await;
        // KWin has no "window under the pointer" call: the user clicks it.
        self.go(
            "--window",
            include_mouse_pointer,
            include_window_decorations,
            include_window_shadow,
            &who,
        );
    }

    async fn rectangular_region(
        &self,
        include_mouse_pointer: i32,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) {
        let who = caller(conn, &hdr).await;
        self.go("--region", include_mouse_pointer, -1, -1, &who);
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
    fn go(&self, what: &'static str, pointer: i32, frame: i32, shadow: i32, who: &str) {
        self.shared.touch();
        self.shared.capture(argv(what, pointer, frame, shadow, who));
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
        limiter: Mutex::new(Limiter::default()),
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
        // The fixed tail: never silent, and who asked.
        let tail = ["--requested-by", "curl"];
        let wait = ["--delay", "1"];
        let line = |parts: &[&[&str]]| -> Vec<OsString> {
            a(&parts
                .iter()
                .flat_map(|p| p.iter().copied())
                .collect::<Vec<_>>())
        };
        assert_eq!(
            argv("--full", -1, -1, -1, "curl"),
            line(&[&["--full", "--notify"], &wait, &tail])
        );
        assert_eq!(
            argv("--full", 1, -1, -1, "curl"),
            line(&[&["--full", "--notify", "--cursor"], &wait, &tail])
        );
        assert_eq!(
            argv("--screen", 0, -1, -1, "curl"),
            line(&[&["--screen", "--notify", "--no-cursor"], &wait, &tail])
        );
        assert_eq!(
            argv("--active-window", 1, 0, 0, "curl"),
            line(&[
                &[
                    "--active-window",
                    "--notify",
                    "--cursor",
                    "--no-frame",
                    "--no-shadow"
                ],
                &wait,
                &tail
            ])
        );
        // What the user picks on screen needs no countdown.
        assert_eq!(
            argv("--window", -1, 1, 1, "curl"),
            line(&[&["--window", "--notify", "--frame", "--shadow"], &tail])
        );
        // Any other number is "use the setting".
        assert_eq!(
            argv("--region", 7, -5, 2, "curl"),
            line(&[&["--region", "--notify"], &tail])
        );
    }

    #[test]
    fn every_command_line_parses_and_is_never_silent() {
        for what in [
            "--full",
            "--screen",
            "--active-window",
            "--window",
            "--region",
        ] {
            for (p, f, s) in [(1, -1, -1), (0, 0, 0), (-1, 1, 1), (9, -9, 9)] {
                let args = argv(what, p, f, s, "KDE Connect")
                    .into_iter()
                    .map(|s| s.into_string().unwrap());
                let crate::cli::Action::Run(run) = crate::cli::parse_args(args).unwrap() else {
                    panic!("{what} is not a capture");
                };
                assert!(run.notify && !run.no_notify, "{what}");
                assert_eq!(run.requested_by.as_deref(), Some("KDE Connect"));
                let interactive = matches!(what, "--region" | "--window");
                assert_eq!(run.delay, if interactive { 0 } else { 1 }, "{what}");
                // Nothing a caller sends can turn a capture into the editor,
                // a region without the overlay, or a copy that saves nothing.
                assert!(!run.edit && !run.no_save && run.region.is_none(), "{what}");
            }
        }
    }

    #[test]
    fn a_caller_is_named_by_its_comm_and_nothing_else() {
        assert_eq!(label_from_comm(b"curl\n"), "curl");
        assert_eq!(label_from_comm(b"gdbus"), "gdbus");
        assert_eq!(label_from_comm(b"kdeconnectd"), "kdeconnectd");
        assert_eq!(label_from_comm(b"xdg-dbus-proxy\n"), SANDBOXED);
        assert_eq!(label_from_comm(b""), UNKNOWN);
        assert_eq!(label_from_comm(b"\n"), UNKNOWN);
        assert_eq!(label_from_comm(b"   "), UNKNOWN);
        // The program chose its own name: markup, escapes and non-UTF-8 are
        // defanged, and the length is bounded.
        assert_eq!(label_from_comm(b"<b>x</b>"), "?b?x??b?");
        assert_eq!(label_from_comm(b"a\x1b[31mred"), "a??31mred");
        assert_eq!(label_from_comm(&[0xff, 0xfe, b'a']), "??a");
        assert_eq!(label_from_comm(&[b'x'; 200]).len(), 15);
        // And whatever it is, the command line accepts it.
        for comm in [&b"<b>x</b>"[..], &[0xff; 15][..], b"a b", b"--full"] {
            let label = label_from_comm(comm);
            let args = argv("--full", -1, -1, -1, &label)
                .into_iter()
                .map(|s| s.into_string().unwrap());
            assert!(matches!(
                crate::cli::parse_args(args),
                Ok(crate::cli::Action::Run(_))
            ));
        }
    }

    #[test]
    fn requests_are_limited_to_one_every_few_seconds() {
        let t0 = Instant::now();
        let mut l = Limiter::default();
        assert!(l.allow(t0));
        assert!(!l.allow(t0));
        assert!(!l.allow(t0 + MIN_GAP - Duration::from_millis(1)));
        // A refused request does not push the next one back.
        assert!(l.allow(t0 + MIN_GAP));
        assert!(!l.allow(t0 + MIN_GAP + Duration::from_secs(1)));
        assert!(l.allow(t0 + MIN_GAP * 2));
        // An earlier time (a clock that stepped back) is a refusal, not a panic.
        assert!(!l.allow(t0));
    }
}
