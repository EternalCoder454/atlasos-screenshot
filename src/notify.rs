//! Messages the user can see. The tool usually runs from a hotkey, where
//! stderr goes nowhere, so anything the user must know about also goes out
//! as a desktop notification (`org.freedesktop.Notifications`). Best effort:
//! with no notification server the stderr line is all there is.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use zbus::zvariant::Value;

const APP: &str = "AtlasOS Screenshot";
const ICON: &str = "applets-screenshooter";

/// Prints `text` and shows it as a normal-urgency notification.
pub fn show(text: &str) {
    // Not eprintln!: it panics when stderr is closed.
    let _ = writeln!(std::io::stderr(), "atlasos-screenshot: {text}");
    let mut chars = text.chars();
    let body: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    send(&escape(&body));
}

/// The body may be read as markup (`<b>`, `<a href>`); the text can hold
/// paths and config parser messages, so it goes out as plain text.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn send(body: &str) {
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(Duration::from_secs(3)).build());
    let Ok(conn) = conn else { return };
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("urgency", Value::from(1u8));
    hints.insert(
        "desktop-entry",
        Value::from("net.eterneon.atlas.screenshot"),
    );
    let _ = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(APP, 0u32, ICON, APP, body, Vec::<&str>::new(), hints, -1i32),
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            super::escape("<a href=\"x\">a & b</a>"),
            "&lt;a href=\"x\"&gt;a &amp; b&lt;/a&gt;"
        );
    }
}
