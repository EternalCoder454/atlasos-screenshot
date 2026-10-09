//! After the capture: the notification with its buttons, or the editor.
//!
//! Done by `telamon-screenshot --post`, a detached process the capture
//! starts and hands its result (over a pipe), so the command returns at once.
//! It shows the notification, then waits for a button: Open, Show in Folder,
//! Edit or Copy run through `actions`. It stops when the notification is
//! dismissed, or after a few minutes.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

/// Hands the rest to `telamon-screenshot --post`, a detached process of its
/// own, and returns. (Not a fork: this process has used D-Bus, whose
/// background thread a forked child would not have.) Best effort.
pub fn finish(done: Done) {
    if !done.edit && !done.notify {
        return;
    }
    let job = encode(&done);
    let spawned = Command::new(actions::self_exe())
        .arg("--post")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: it outlives us and doesn't get our signals.
        .process_group(0)
        .spawn();
    if let Ok(mut child) = spawned {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&job);
        }
        // Not waited for: it runs until the notification is gone.
        std::mem::forget(child);
    }
}

/// The job as the helper reads it: `name:length\n` and that many bytes, for
/// each field that is set.
fn encode(d: &Done) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |name: &str, bytes: &[u8]| {
        out.extend_from_slice(format!("{name}:{}\n", bytes.len()).as_bytes());
        out.extend_from_slice(bytes);
    };
    put("title", d.title.as_bytes());
    put("body", d.body.as_bytes());
    if let Some(png) = &d.png {
        put("png", png);
    }
    if let Some(path) = &d.path {
        use std::os::unix::ffi::OsStrExt;
        put("path", path.as_os_str().as_bytes());
    }
    if let Some(Image::Thumb { w, h, rgba }) = &d.image {
        let mut t = Vec::with_capacity(8 + rgba.len());
        t.extend_from_slice(&w.to_le_bytes());
        t.extend_from_slice(&h.to_le_bytes());
        t.extend_from_slice(rgba);
        put("thumb", &t);
    }
    if d.edit {
        put("edit", b"1");
    }
    if d.notify {
        put("notify", b"1");
    }
    out
}

/// At most this much is read as a job (a 16384 x 16384 thumbnail-free PNG
/// is far below it).
const MAX_JOB: u64 = 320 * 1024 * 1024;

fn decode(input: impl Read) -> Result<Done, String> {
    let mut data = Vec::new();
    input
        .take(MAX_JOB + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_JOB {
        return Err("the job is too large".into());
    }
    let mut done = Done {
        title: String::new(),
        body: String::new(),
        png: None,
        path: None,
        image: None,
        edit: false,
        notify: false,
    };
    let mut rest = &data[..];
    while !rest.is_empty() {
        let nl = rest.iter().position(|&b| b == b'\n').ok_or("a cut job")?;
        let head = std::str::from_utf8(&rest[..nl]).map_err(|_| "a bad job")?;
        let (name, len) = head.split_once(':').ok_or("a bad job")?;
        let len: usize = len.parse().map_err(|_| "a bad job")?;
        // checked: a length near usize::MAX must be "cut", not a wrapped sum.
        let end = (nl + 1).checked_add(len).ok_or("a cut job")?;
        let body = rest.get(nl + 1..end).ok_or("a cut job")?;
        rest = &rest[end..];
        let text = || String::from_utf8_lossy(body).into_owned();
        match name {
            "title" => done.title = text(),
            "body" => done.body = text(),
            "png" => done.png = Some(body.to_vec()),
            "path" => {
                use std::os::unix::ffi::OsStrExt;
                let p = PathBuf::from(std::ffi::OsStr::from_bytes(body));
                // A path we made is absolute; anything else is refused.
                if p.is_absolute() {
                    done.path = Some(p);
                }
            }
            "thumb" => {
                if body.len() >= 8 {
                    let w = u32::from_le_bytes(body[0..4].try_into().unwrap_or([0; 4]));
                    let h = u32::from_le_bytes(body[4..8].try_into().unwrap_or([0; 4]));
                    let ok = w > 0
                        && h > 0
                        && w <= notify::THUMB_MAX
                        && h <= notify::THUMB_MAX
                        && body.len() == 8 + w as usize * h as usize * 4;
                    if ok {
                        done.image = Some(Image::Thumb {
                            w,
                            h,
                            rgba: body[8..].to_vec(),
                        });
                    }
                }
            }
            "edit" => done.edit = true,
            "notify" => done.notify = true,
            _ => {}
        }
    }
    Ok(done)
}

/// `telamon-screenshot --post`: the job on stdin. Returns the exit code.
pub fn run_helper() -> i32 {
    let Ok(mut done) = decode(std::io::stdin().lock()) else {
        return 1;
    };
    if let Some(path) = &done.path
        && done.image.is_none()
    {
        done.image = Some(Image::File(path.clone()));
    }
    child(done)
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
    let Ok((id, server)) = notify::send_toast(&conn, &toast) else {
        return 0;
    };
    if toast.actions.is_empty() {
        return 0;
    }
    std::thread::spawn(|| {
        std::thread::sleep(LINGER);
        std::process::exit(0)
    });
    serve(&conn, &mut events, id, &server, &ctx, done.png.as_deref());
    0
}

fn serve(
    conn: &zbus::blocking::Connection,
    events: &mut zbus::blocking::MessageIterator,
    id: u32,
    server: &str,
    ctx: &Ctx,
    png: Option<&[u8]>,
) {
    while let Some(ev) = notify::next_event(events, id, server) {
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

    fn sample() -> Done {
        Done {
            title: "Screenshot Saved".into(),
            body: "a\nb: c".into(),
            png: Some(vec![0x89, b'P', 1, 2, 3]),
            path: Some(PathBuf::from("/p/ü b.png")),
            image: Some(Image::Thumb {
                w: 2,
                h: 1,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            }),
            edit: false,
            notify: true,
        }
    }

    #[test]
    fn a_job_survives_the_pipe() {
        let d = decode(&encode(&sample())[..]).unwrap();
        let s = sample();
        assert_eq!((d.title, d.body), (s.title, s.body));
        assert_eq!((d.png, d.path), (s.png, s.path));
        assert!(d.notify && !d.edit);
        match d.image {
            Some(Image::Thumb { w, h, rgba }) => assert_eq!((w, h, rgba.len()), (2, 1, 8)),
            _ => panic!(),
        }
    }

    #[test]
    fn a_bad_job_is_refused_not_trusted() {
        assert!(decode(&b"title:5\nab"[..]).is_err(), "cut");
        assert!(decode(&b"title\n"[..]).is_err(), "no length");
        assert!(decode(&b"title:x\n"[..]).is_err(), "bad length");
        assert!(decode(&b"title:99999999999999999999\n"[..]).is_err());
        // Lengths that would wrap the offset around.
        for len in [
            usize::MAX,
            usize::MAX - 1,
            usize::MAX - 5,
            usize::MAX / 2 + 1,
        ] {
            let job = format!("title:{len}\nx");
            assert!(decode(job.as_bytes()).is_err(), "{len}");
        }
        // A relative path is dropped; a thumbnail of the wrong size too.
        let d = decode(&b"path:3\nabcthumb:9\n12345678x"[..]).unwrap();
        assert!(d.path.is_none() && d.image.is_none());
        // Unknown fields are ignored.
        assert!(decode(&b"nope:1\nx"[..]).is_ok());
    }

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
