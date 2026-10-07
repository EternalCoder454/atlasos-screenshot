//! After the capture: the notification with its buttons, or the editor.
//!
//! Done in a detached child (forked like the clipboard server), so the
//! command returns at once and the hotkey's process list stays clean. The
//! child shows the notification, then waits for a button: Open, Show in
//! Folder, Edit or Copy run through `actions`. It stops when the
//! notification is dismissed, after a button is used, or after a few
//! minutes.

use std::path::PathBuf;
use std::time::Duration;

use crate::actions::{self, Ctx};
use crate::notify::{self, Event, Image, Toast};

/// How long the child waits for a button at most (a notification that
/// expired is still in the history, where its buttons can be used).
const LINGER: Duration = Duration::from_secs(300);

pub struct Done {
    pub title: String,
    pub body: String,
    /// The PNG, for Copy and for an editor that has no file.
    pub png: Option<Vec<u8>>,
    /// The saved file.
    pub path: Option<PathBuf>,
    /// A thumbnail, when there is no file to show.
    pub image: Option<Image>,
    pub edit: bool,
    pub notify: bool,
}

/// The buttons a capture offers: file actions only with a file, picture
/// actions only with a picture.
pub fn buttons(has_file: bool, has_png: bool) -> Vec<(&'static str, &'static str)> {
    let mut b = Vec::new();
    if has_file {
        b.push(actions::OPEN);
        b.push(actions::FOLDER);
    }
    if has_png {
        b.push(actions::EDIT);
        b.push(actions::COPY);
    }
    b
}

/// Hands the rest to a detached child and returns.
pub fn finish(done: Done) {
    if !done.edit && !done.notify {
        return;
    }
    let inherited = crate::clipboard::open_fds();
    // SAFETY: the child only runs `child` on state it owns, then `_exit`s;
    // the caller has no other thread or bus connection open (see
    // `clipboard::copy`, which does the same).
    if unsafe { libc::fork() } == 0 {
        std::panic::set_hook(Box::new(|_| {}));
        crate::clipboard::detach(&inherited);
        let code = child(done);
        // SAFETY: ends the child without the parent's atexit handlers.
        unsafe { libc::_exit(code) }
    }
}

fn child(done: Done) -> i32 {
    let ctx = actions::ctx(done.path.clone());
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(Duration::from_secs(3)).build())
        .ok();
    if done.edit {
        if let Some(launch) = actions::plan("edit", &ctx) {
            actions::run(launch, done.png.as_deref(), conn.as_ref());
        }
        return 0;
    }
    let Some(conn) = conn else { return 0 };
    let Ok(mut events) = notify::watch(&conn) else {
        return 0;
    };
    let toast = Toast {
        title: done.title.clone(),
        body: done.body.clone(),
        image: done.image,
        actions: buttons(done.path.is_some(), done.png.is_some()),
    };
    let Ok(id) = notify::send_toast(&conn, &toast) else {
        return 0;
    };
    if toast.actions.is_empty() {
        return 0;
    }
    std::thread::spawn(|| {
        std::thread::sleep(LINGER);
        // SAFETY: ends the child; it holds nothing to flush.
        unsafe { libc::_exit(0) }
    });
    serve(&conn, &mut events, id, &ctx, done.png.as_deref());
    0
}

fn serve(
    conn: &zbus::blocking::Connection,
    events: &mut zbus::blocking::MessageIterator,
    id: u32,
    ctx: &Ctx,
    png: Option<&[u8]>,
) {
    while let Some(ev) = notify::next_event(events, id) {
        match ev {
            Event::Action(key) => {
                if let Some(launch) = actions::plan(&key, ctx) {
                    actions::run(launch, png, Some(conn));
                }
            }
            // Expired: it is in the history, and the buttons still work.
            Event::Closed(1) => {}
            Event::Closed(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_follow_what_exists() {
        let keys = |b: Vec<(&'static str, &'static str)>| {
            b.into_iter().map(|(k, _)| k).collect::<Vec<_>>()
        };
        assert_eq!(
            keys(buttons(true, true)),
            ["open", "folder", "edit", "copy"]
        );
        assert_eq!(keys(buttons(false, true)), ["edit", "copy"]);
        assert_eq!(keys(buttons(true, false)), ["open", "folder"]);
        assert!(buttons(false, false).is_empty());
    }

    #[test]
    fn labels_are_title_case() {
        let labels: Vec<_> = buttons(true, true).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["Open", "Show in Folder", "Edit", "Copy"]);
    }
}
