//! Messages the user can see. The tool usually runs from a hotkey, where
//! stderr goes nowhere, so anything the user must know about also goes out
//! as a desktop notification (`org.freedesktop.Notifications`). Best effort:
//! with no notification server the stderr line is all there is.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{StructureBuilder, Value};

const APP: &str = "Telamon Screenshot";
const ICON: &str = "applets-screenshooter";

/// Prints `text` and shows it as a normal-urgency notification.
pub fn show(text: &str) {
    // Not eprintln!: it panics when stderr is closed.
    let _ = writeln!(std::io::stderr(), "telamon-screenshot: {text}");
    let mut chars = text.chars();
    let body: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    send(&escape(&body));
}

const APP_ID: &str = "net.eterneon.telamon.screenshot";
const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// The picture shown in a capture's notification.
pub enum Image {
    /// The saved file (the server loads and scales it).
    File(PathBuf),
    /// A small copy, for a capture that wasn't saved: RGBA, `w * h * 4` bytes.
    Thumb { w: u32, h: u32, rgba: Vec<u8> },
}

/// A capture's notification: title, body, picture and buttons.
pub struct Toast {
    pub title: String,
    /// Plain text; escaped on the way out.
    pub body: String,
    pub image: Option<Image>,
    /// `(key, label)`
    pub actions: Vec<(&'static str, &'static str)>,
}

/// The longest side of the thumbnail sent when there is no file.
pub const THUMB_MAX: u32 = 320;

/// A copy of `img` no larger than `THUMB_MAX` on a side.
pub fn thumb(img: &image::RgbaImage) -> Image {
    let (w, h) = img.dimensions();
    let k = (THUMB_MAX as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    );
    let small = if (tw, th) == (w, h) {
        img.clone()
    } else {
        image::imageops::thumbnail(img, tw, th)
    };
    Image::Thumb {
        w: tw,
        h: th,
        rgba: small.into_raw(),
    }
}

/// The action list as `Notify` wants it: key, label, key, label...
fn flat_actions(actions: &[(&str, &str)]) -> Vec<String> {
    actions
        .iter()
        .flat_map(|(k, l)| [k.to_string(), l.to_string()])
        .collect()
}

/// The hints of a capture notification.
fn toast_hints(image: &Option<Image>) -> HashMap<&'static str, Value<'static>> {
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("urgency", Value::from(1u8));
    hints.insert("desktop-entry", Value::from(APP_ID));
    match image {
        Some(Image::File(path)) => {
            hints.insert("image-path", Value::from(crate::actions::file_uri(path)));
        }
        Some(Image::Thumb { w, h, rgba }) => {
            let data = StructureBuilder::new()
                .add_field(*w as i32)
                .add_field(*h as i32)
                .add_field(*w as i32 * 4)
                .add_field(true)
                .add_field(8i32)
                .add_field(4i32)
                .add_field(rgba.clone())
                .build();
            if let Ok(data) = data {
                hints.insert("image-data", Value::from(data));
            }
        }
        None => {}
    }
    hints
}

/// Listens for the signals of the notification server. Made before the
/// notification is sent, so a quick click is not missed.
pub fn watch(conn: &Connection) -> Result<MessageIterator, String> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.Notifications")
        .and_then(|b| b.path(PATH))
        .map(|b| b.build())
        .map_err(|e| e.to_string())?;
    MessageIterator::for_match_rule(rule, conn, Some(16)).map_err(|e| e.to_string())
}

/// Shows a capture's notification and returns its id.
pub fn send_toast(conn: &Connection, toast: &Toast) -> Result<u32, String> {
    let reply = conn
        .call_method(
            Some(DEST),
            PATH,
            Some(DEST),
            "Notify",
            &(
                APP,
                0u32,
                ICON,
                toast.title.as_str(),
                escape(&toast.body),
                flat_actions(&toast.actions),
                toast_hints(&toast.image),
                -1i32,
            ),
        )
        .map_err(|e| e.to_string())?;
    reply.body().deserialize::<u32>().map_err(|e| e.to_string())
}

/// What happened to our notification.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Action(String),
    /// Closed, with the server's reason (1 expired, 2 dismissed, 3 closed by
    /// a call, 4 undefined).
    Closed(u32),
}

/// The next event for notification `id`; `None` when the bus goes away.
pub fn next_event(events: &mut MessageIterator, id: u32) -> Option<Event> {
    for msg in events.by_ref() {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let body = msg.body();
        match header.member().map(|m| m.as_str()) {
            Some("ActionInvoked") => {
                if let Ok((nid, key)) = body.deserialize::<(u32, String)>()
                    && nid == id
                {
                    return Some(Event::Action(key));
                }
            }
            Some("NotificationClosed") => {
                if let Ok((nid, reason)) = body.deserialize::<(u32, u32)>()
                    && nid == id
                {
                    return Some(Event::Closed(reason));
                }
            }
            _ => {}
        }
    }
    None
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
        Value::from("net.eterneon.telamon.screenshot"),
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
    use super::*;

    #[test]
    fn actions_are_flattened_as_key_label_pairs() {
        assert_eq!(
            flat_actions(&[crate::actions::OPEN, crate::actions::FOLDER]),
            ["open", "Open", "folder", "Show in Folder"]
        );
    }

    #[test]
    fn the_image_goes_as_a_path_or_as_data() {
        let h = toast_hints(&Some(Image::File(PathBuf::from("/p/a b.png"))));
        assert_eq!(
            <&str>::try_from(&h["image-path"]),
            Ok("file:///p/a%20b.png")
        );
        assert!(!h.contains_key("image-data"));
        assert_eq!(<&str>::try_from(&h["desktop-entry"]), Ok(APP_ID));
        let h = toast_hints(&Some(Image::Thumb {
            w: 2,
            h: 1,
            rgba: vec![0; 8],
        }));
        assert!(h.contains_key("image-data") && !h.contains_key("image-path"));
        assert!(!toast_hints(&None).contains_key("image-data"));
    }

    #[test]
    fn thumbnails_are_small() {
        let big = image::RgbaImage::new(3840, 2160);
        match thumb(&big) {
            Image::Thumb { w, h, rgba } => {
                assert_eq!((w, h), (320, 180));
                assert_eq!(rgba.len(), 320 * 180 * 4);
            }
            _ => panic!(),
        }
        match thumb(&image::RgbaImage::new(40, 30)) {
            Image::Thumb { w, h, .. } => assert_eq!((w, h), (40, 30)),
            _ => panic!(),
        }
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            super::escape("<a href=\"x\">a & b</a>"),
            "&lt;a href=\"x\"&gt;a &amp; b&lt;/a&gt;"
        );
    }
}
