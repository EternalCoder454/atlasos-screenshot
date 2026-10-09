//! The invariants of everything in this program that reads untrusted input:
//! file names, the folder of a save, `user-dirs.dirs`, `file://` links, the
//! helper's job pipe, the command line, the config file, D-Bus callers'
//! names, notification text, the PNG on stdin and the text redaction reads.
//!
//! One file for two users, so that a property found by one is checked by the
//! other: the property tests in `src/proptests.rs` (run by `cargo test`) call
//! the typed checks, and the libFuzzer targets in `fuzz/` (which include this
//! file as `checks`) call the byte-level one named after each target. Every
//! check panics when an invariant breaks and returns quietly otherwise.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::capture::Rect;
use crate::cli::{self, Action, Args};
use crate::config::{self, Config};
use crate::modes::Kind;
use crate::notify::{self, Image};
use crate::post::{self, Done};
use crate::{actions, dbus, pngin, redact, store};

/// The separator between the fields of a fuzz input that holds several
/// (arguments, a text and a name).
pub const SEP: u8 = 0x1F;

fn fields(data: &[u8]) -> Vec<String> {
    data.split(|b| *b == SEP)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// File names

/// `YYYYMMDD_HHMMSS`: eight digits, an underscore, six digits.
pub fn is_stamp(s: &str) -> bool {
    s.len() == 15
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'_'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// `^Screenshot_\d{8}_\d{6}(-[1-9]\d?)?\.png$`
pub fn is_screenshot_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("Screenshot_")
        .and_then(|r| r.strip_suffix(".png"))
    else {
        return false;
    };
    if rest.len() < 15 || !rest.is_char_boundary(15) {
        return false;
    }
    let (stamp, suffix) = rest.split_at(15);
    is_stamp(stamp)
        && match suffix.strip_prefix('-') {
            None => suffix.is_empty(),
            Some(n) => {
                (1..=2).contains(&n.len())
                    && n.bytes().all(|b| b.is_ascii_digit())
                    && !n.starts_with('0')
            }
        }
}

/// The stamps of a time and the names built from them.
pub fn stamp_time(t: SystemTime) {
    let utc = store::utc_stamp(t);
    let local = store::local_stamp(t);
    assert!(is_stamp(&utc), "utc {utc:?}");
    assert!(is_stamp(&local), "local {local:?}");
    names_of(&utc);
    names_of(&local);
}

/// The 100 names for a stamp: distinct, in shape, short, one path component.
pub fn names_of(stamp: &str) {
    let names = store::names(stamp);
    assert_eq!(names.len(), 100);
    let distinct: BTreeSet<&String> = names.iter().collect();
    assert_eq!(distinct.len(), 100, "names repeat");
    for (i, n) in names.iter().enumerate() {
        assert!(is_screenshot_name(n), "{n:?}");
        assert!(!n.contains(['/', '\0']), "{n:?}");
        assert!(n.len() <= 64, "{n:?}");
        assert_eq!(n.ends_with(&format!("-{i}.png")), i > 0, "{n:?}");
    }
}

/// Fuzz target `stamp_names`: 8 bytes of seconds, and a ninth to say
/// before 1970.
pub fn stamp_names(data: &[u8]) {
    let Some(first) = data.first_chunk::<8>() else {
        return;
    };
    let d = Duration::from_secs(u64::from_le_bytes(*first));
    let t = if data.get(8).is_some_and(|b| b & 1 == 1) {
        UNIX_EPOCH.checked_sub(d)
    } else {
        UNIX_EPOCH.checked_add(d)
    };
    if let Some(t) = t {
        stamp_time(t);
    }
}

// ---------------------------------------------------------------------------
// Saving

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// A fresh folder (0700) in the temp folder, removed again on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Scratch {
        for _ in 0..1000 {
            let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "telamon-screenshot-check-{}-{n}",
                std::process::id()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Scratch(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("can't make {}: {e}", dir.display()),
            }
        }
        panic!("no free scratch folder");
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names_in(dir: &Path) -> BTreeSet<OsString> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

fn open_dir(dir: &Path) -> rustix::fd::OwnedFd {
    rustix::fs::open(
        dir,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap()
}

/// `path` is a regular file (not a link) holding `png`, mode exactly 0600.
fn saved_file_ok(path: &Path, png: &[u8]) {
    let meta = std::fs::symlink_metadata(path).unwrap();
    assert!(
        meta.file_type().is_file(),
        "{} is not a file",
        path.display()
    );
    assert_eq!(
        meta.permissions().mode() & 0o7777,
        0o600,
        "{}",
        path.display()
    );
    assert_eq!(std::fs::read(path).unwrap(), png);
}

/// `save_png` twice into a fresh folder: the bytes come back, the mode is
/// 0600, the names are Spectacle's and differ, and nothing else is left.
pub fn save_round_trip(png: &[u8]) {
    let dir = Scratch::new();
    let a = store::save_png(dir.path(), png).unwrap();
    let b = store::save_png(dir.path(), png).unwrap();
    assert_ne!(a, b);
    for p in [&a, &b] {
        assert_eq!(p.parent(), Some(dir.path()));
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(is_screenshot_name(name), "{name:?}");
        saved_file_ok(p, png);
    }
    assert_eq!(names_in(dir.path()).len(), 2, "something besides the files");
}

/// What `plant` can leave under a name that is taken.
const KINDS: u8 = 4;

/// Something already at `link`: a file, a link to a file elsewhere, a
/// dangling link to a name elsewhere, or a folder. `outside` is the folder
/// the links point into.
fn plant(kind: u8, link: &Path, outside: &Path) {
    match kind % KINDS {
        0 => std::fs::write(link, b"MINE").unwrap(),
        1 => symlink(outside.join("victim"), link).unwrap(),
        2 => symlink(outside.join("nothing"), link).unwrap(),
        _ => std::fs::create_dir(link).unwrap(),
    }
}

/// What a planted entry looks like, to see that it did not change.
#[derive(Debug, PartialEq)]
enum Seen {
    File(Vec<u8>),
    Link(PathBuf),
    Dir,
}

fn seen(p: &Path) -> Seen {
    let meta = std::fs::symlink_metadata(p).unwrap();
    if meta.file_type().is_symlink() {
        Seen::Link(std::fs::read_link(p).unwrap())
    } else if meta.is_dir() {
        Seen::Dir
    } else {
        Seen::File(std::fs::read(p).unwrap())
    }
}

/// The outside folder: one file that no save may touch.
const VICTIM: &[u8] = b"VICTIM";

fn outside_folder() -> Scratch {
    let outside = Scratch::new();
    std::fs::write(outside.path().join("victim"), VICTIM).unwrap();
    outside
}

fn outside_untouched(outside: &Path) {
    assert_eq!(std::fs::read(outside.join("victim")).unwrap(), VICTIM);
    assert_eq!(
        names_in(outside),
        BTreeSet::from([OsString::from("victim")]),
        "something was made through a link"
    );
}

/// The first `kinds.len()` names of a stamp are taken (a file, links, a
/// folder, as `kinds` says); `save_unnamed` (`renamed` false; it may report
/// that the file system can't do it) or `save_renamed` must take the next
/// name, write through and replace nothing, and leave nothing else.
pub fn save_beside(png: &[u8], kinds: &[u8], renamed: bool) {
    let kinds = &kinds[..kinds.len().min(99)];
    let dir = Scratch::new();
    let outside = outside_folder();
    let names = store::names("20260101_120000");
    let mut before = Vec::new();
    for (name, kind) in names.iter().zip(kinds) {
        let at = dir.path().join(name);
        plant(*kind, &at, outside.path());
        before.push((at.clone(), seen(&at)));
    }
    let planted = names_in(dir.path());
    let dfd = open_dir(dir.path());
    let got = if renamed {
        store::save_renamed(&dfd, png, &names).unwrap()
    } else {
        store::save_unnamed(&dfd, png, &names).unwrap()
    };
    for (at, was) in &before {
        assert_eq!(&seen(at), was, "{} was changed", at.display());
    }
    outside_untouched(outside.path());
    match got {
        Some(name) => {
            assert_eq!(name, names[kinds.len()], "not the next free name");
            saved_file_ok(&dir.path().join(&name), png);
            let mut want = planted;
            want.insert(OsString::from(name));
            assert_eq!(names_in(dir.path()), want, "something was left behind");
        }
        None => {
            assert!(!renamed, "a free name was not used");
            assert_eq!(names_in(dir.path()), planted, "a failed save left a file");
        }
    }
}

/// Every name is taken: nothing is written, nothing left, no error from the
/// renaming way (the unnamed way says so as an error).
pub fn save_all_taken(png: &[u8]) {
    let dir = Scratch::new();
    let outside = outside_folder();
    let names = store::names("20260101_120000");
    for (i, name) in names.iter().enumerate() {
        plant(
            if i % 2 == 0 { 0 } else { 2 },
            &dir.path().join(name),
            outside.path(),
        );
    }
    let planted = names_in(dir.path());
    let dfd = open_dir(dir.path());
    assert!(store::save_renamed(&dfd, png, &names).unwrap().is_none());
    assert_eq!(names_in(dir.path()), planted);
    match store::save_unnamed(&dfd, png, &names) {
        Ok(None) | Err(_) => {}
        Ok(Some(n)) => panic!("saved as {n} with every name taken"),
    }
    assert_eq!(names_in(dir.path()), planted);
    outside_untouched(outside.path());
}

/// `save_png` with dangling links (and files) planted under the first name
/// of the seconds around now: the result is a name that was not planted, and
/// nothing planted or outside changed.
pub fn save_png_beside(png: &[u8], kinds: &[u8]) {
    let dir = Scratch::new();
    let outside = outside_folder();
    let now = SystemTime::now();
    let mut before = Vec::new();
    for (i, kind) in kinds.iter().take(5).enumerate() {
        let t = now + Duration::from_secs(i as u64);
        let name = store::names(&store::local_stamp(t)).remove(0);
        let at = dir.path().join(name);
        if at.symlink_metadata().is_ok() {
            continue;
        }
        plant(*kind, &at, outside.path());
        before.push((at.clone(), seen(&at)));
    }
    let planted = names_in(dir.path());
    let path = store::save_png(dir.path(), png).unwrap();
    let name = path.file_name().unwrap().to_owned();
    assert!(!planted.contains(&name), "{name:?} was planted");
    for (at, was) in &before {
        assert_eq!(&seen(at), was, "{} was changed", at.display());
    }
    outside_untouched(outside.path());
    saved_file_ok(&path, png);
    let mut want = planted;
    want.insert(name);
    assert_eq!(names_in(dir.path()), want);
}

// ---------------------------------------------------------------------------
// user-dirs.dirs

/// A value of `user-dirs.dirs` is absolute, has no `..` and no NUL, or is
/// refused.
pub fn user_dirs_text(text: &str, name: &str) {
    for home in ["/home/z", "/", "/srv/a b"] {
        if let Some(p) = store::user_dir_from(text, name, Path::new(home)) {
            assert!(p.is_absolute(), "{p:?}");
            assert!(
                !p.components().any(|c| c.as_os_str() == ".."),
                "{p:?} climbs"
            );
            assert!(!p.as_os_str().as_bytes().contains(&0), "{p:?} has a NUL");
        }
    }
}

/// Fuzz target `user_dirs`: the file, then the name asked for.
pub fn user_dirs(data: &[u8]) {
    let f = fields(data);
    user_dirs_text(&f[0], f.get(1).map_or("PICTURES", String::as_str));
    user_dirs_text(&f[0], "PICTURES");
}

// ---------------------------------------------------------------------------
// file:// links

fn hex(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("not an uppercase hex digit: {b:#x}"),
    }
}

/// The link is ASCII from a small alphabet and decodes to the same bytes.
pub fn uri_bytes(path: &[u8]) {
    let uri = actions::file_uri(Path::new(std::ffi::OsStr::from_bytes(path)));
    assert!(uri.is_ascii());
    let rest = uri.strip_prefix("file://").expect("the scheme").as_bytes();
    assert!(
        rest.iter()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~/%".contains(b)),
        "{uri:?}"
    );
    let mut back = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b'%' {
            let (h, l) = (rest.get(i + 1), rest.get(i + 2));
            let (Some(h), Some(l)) = (h, l) else {
                panic!("a cut escape in {uri:?}")
            };
            back.push(hex(*h) << 4 | hex(*l));
            i += 3;
        } else {
            back.push(rest[i]);
            i += 1;
        }
    }
    assert_eq!(back, path);
}

/// Fuzz target `file_uri`.
pub fn file_uri(data: &[u8]) {
    uri_bytes(data);
}

// ---------------------------------------------------------------------------
// The helper's job

fn same_done(a: &Done, b: &Done) {
    assert_eq!(a.title, b.title);
    assert_eq!(a.body, b.body);
    assert_eq!(a.png, b.png);
    assert_eq!(
        a.path.as_ref().map(|p| p.as_os_str().as_bytes()),
        b.path.as_ref().map(|p| p.as_os_str().as_bytes())
    );
    match (&a.image, &b.image) {
        (None, None) => {}
        (
            Some(Image::Thumb { w, h, rgba }),
            Some(Image::Thumb {
                w: w2,
                h: h2,
                rgba: rgba2,
            }),
        ) => assert_eq!((w, h, rgba), (w2, h2, rgba2)),
        _ => panic!("the pictures differ"),
    }
    assert_eq!((a.edit, a.notify), (b.edit, b.notify));
}

/// What a decoded job promises, whatever it was cut from: sizes within a
/// small multiple of the input (text read lossily grows 3 times at most),
/// an absolute path, a thumbnail that is as big as it says, and a job that
/// encodes to one that decodes to the same.
fn done_ok(d: &Done, input_len: usize) {
    let mut total = d.title.len() + d.body.len();
    total += d.png.as_ref().map_or(0, Vec::len);
    total += d.path.as_ref().map_or(0, |p| p.as_os_str().len());
    if let Some(img) = &d.image {
        let Image::Thumb { w, h, rgba } = img else {
            panic!("a file picture from the pipe");
        };
        assert!((1..=notify::THUMB_MAX).contains(w) && (1..=notify::THUMB_MAX).contains(h));
        assert_eq!(rgba.len(), *w as usize * *h as usize * 4);
        total += rgba.len();
    }
    assert!(total <= input_len * 3, "{total} bytes from {input_len}");
    if let Some(p) = &d.path {
        assert!(p.is_absolute());
    }
    let again = post::decode(&post::encode(d)[..]).expect("an encoded job decodes");
    same_done(d, &again);
}

/// Fuzz target `post_decode`: the bytes are a job, or nonsense.
pub fn post_decode(data: &[u8]) {
    match post::decode(data) {
        Ok(d) => done_ok(&d, data.len()),
        Err(e) => assert!(!e.is_empty()),
    }
}

/// A job made of these comes back from `decode` as it went in. The path is
/// made absolute and the thumbnail made to fit (its size cut to
/// `1..=THUMB_MAX`, its pixels `seed` repeated).
pub fn job_round_trip(
    title: &str,
    body: &str,
    png: Option<&[u8]>,
    path: Option<&[u8]>,
    thumb: Option<(u32, u32, &[u8])>,
    edit: bool,
    notify_flag: bool,
) {
    let path = path.map(|p| {
        let mut v = b"/".to_vec();
        v.extend_from_slice(p);
        PathBuf::from(OsString::from(std::ffi::OsStr::from_bytes(&v)))
    });
    let image = thumb.map(|(w, h, seed)| {
        let (w, h) = (1 + w % notify::THUMB_MAX, 1 + h % notify::THUMB_MAX);
        let rgba = (0..w as usize * h as usize * 4)
            .map(|i| seed.get(i % seed.len().max(1)).copied().unwrap_or(0))
            .collect();
        Image::Thumb { w, h, rgba }
    });
    let done = Done {
        title: title.to_string(),
        body: body.to_string(),
        png: png.map(<[u8]>::to_vec),
        path,
        image,
        edit,
        notify: notify_flag,
    };
    let job = post::encode(&done);
    let back = post::decode(&job[..]).expect("a job decodes");
    same_done(&done, &back);
    done_ok(&back, job.len());
}

// ---------------------------------------------------------------------------
// The command line

/// A name for `--requested-by`: printable, one line, not blank, short.
pub fn label_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.chars().count() <= cli::MAX_LABEL && !s.chars().any(char::is_control)
}

pub fn rect_ok(r: Rect) {
    assert!(
        (-65535..=65535).contains(&r.x) && (-65535..=65535).contains(&r.y),
        "{r:?}"
    );
    assert!(
        (1..=65535).contains(&r.w) && (1..=65535).contains(&r.h),
        "{r:?}"
    );
}

fn run_ok(a: &Args) {
    assert!(a.delay <= cli::MAX_DELAY);
    if let Some(r) = a.region {
        assert_eq!(a.kind, Some(Kind::Region), "a region of another kind");
        rect_ok(r);
    }
    if let Some(who) = &a.requested_by {
        assert!(label_ok(who), "{who:?}");
    }
    assert!(!(a.notify && a.no_notify));
}

/// Any argument list gives an answer, and a capture's arguments are within
/// their bounds.
pub fn args_list(args: &[String]) {
    match cli::parse_args(args.iter().cloned()) {
        Ok(Action::Run(a)) => run_ok(&a),
        Ok(_) => {}
        Err(e) => assert!(!e.is_empty()),
    }
}

/// Fuzz target `cli_args`: arguments separated by 0x1F (invalid UTF-8
/// replaced: `std::env::args` would have refused it before this).
pub fn cli_args(data: &[u8]) {
    args_list(&fields(data));
}

pub fn region_text(s: &str) {
    if let Some(r) = cli::parse_region(s) {
        rect_ok(r);
    }
    args_list(&["--region".to_string(), s.to_string()]);
    args_list(&[s.to_string(), "--region".to_string()]);
}

/// Fuzz target `region`.
pub fn region(data: &[u8]) {
    region_text(&String::from_utf8_lossy(data));
}

// ---------------------------------------------------------------------------
// The config file

pub fn config_ok(c: &Config) {
    assert!(
        (0.0..=0.9).contains(&c.overlay.dim),
        "dim {}",
        c.overlay.dim
    );
    assert!(config::parse_hex_color(&c.overlay.accent).is_some());
    assert!(config::parse_hex_color(&c.redact.fill).is_some());
    assert_eq!(
        Some(c.accent_rgb()),
        config::parse_hex_color(&c.overlay.accent)
    );
    assert!(c.redact.padding <= 32);
    if let Some(d) = &c.output.save_dir {
        assert!(d.is_absolute(), "{d:?}");
    }
}

pub fn config_text(text: &str) {
    match config::parse(text) {
        Ok(c) => config_ok(&c),
        Err(e) => assert!(!e.is_empty()),
    }
}

/// Fuzz target `config_parse`.
pub fn config_parse(data: &[u8]) {
    if let Ok(text) = std::str::from_utf8(data) {
        config_text(text);
    }
}

// ---------------------------------------------------------------------------
// D-Bus callers

const FIXED: [&str; 2] = [dbus::UNKNOWN, dbus::SANDBOXED];

/// The caller's name as the service makes it from `/proc/<pid>/comm` and
/// the name of the file it runs. (The one line to change when the service's
/// function does.)
pub fn label_of(comm: &[u8], exe: Option<&[u8]>) -> String {
    dbus::label(comm, exe)
}

fn plain_chars(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.chars().count() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+ ?".contains(c))
        && s == s.trim()
}

/// A caller's name is one of the two fixed phrases, or at most 15 plain
/// characters (the program's own name for itself) with, optionally, the
/// name of its file in parentheses (at most 24 plain characters): short,
/// plain, and it takes a command line.
pub fn comm_bytes(comm: &[u8], exe: Option<&[u8]>) {
    let label = label_of(comm, exe);
    assert!(!label.is_empty());
    if !FIXED.contains(&label.as_str()) {
        let (name, file) = match label.split_once(" (") {
            Some((name, rest)) => (
                name,
                Some(rest.strip_suffix(')').expect("a closed bracket")),
            ),
            None => (label.as_str(), None),
        };
        assert!(plain_chars(name, 15), "{label:?}");
        if let Some(file) = file {
            assert!(plain_chars(file, 24), "{label:?}");
        }
        assert!(label.chars().count() <= 42, "{label:?}");
        assert_ne!(name, "xdg-dbus-proxy");
    }
    assert!(label_ok(&label));
    for what in 0..5 {
        argv_parses(what, 1, 0, -1, &label);
    }
}

/// The command line of a method parses to a capture that notifies, with
/// nothing a caller chose beyond the fixed flags.
pub fn argv_parses(what: u8, pointer: i32, frame: i32, shadow: i32, label: &str) {
    let what = [
        "--full",
        "--screen",
        "--active-window",
        "--window",
        "--region",
    ][what as usize % 5];
    let argv: Vec<String> = dbus::argv(what, pointer, frame, shadow, label)
        .into_iter()
        .map(|s| s.into_string().expect("the arguments are text"))
        .collect();
    let parsed = cli::parse_args(argv.into_iter());
    if label_ok(label) {
        let Ok(Action::Run(a)) = parsed else {
            panic!("{what} with {label:?}: {parsed:?}");
        };
        assert!(a.notify && !a.no_notify);
        assert_eq!(a.requested_by.as_deref(), Some(label));
        assert!(!a.edit && !a.no_save && a.region.is_none());
        assert_eq!(a.kind.map(Kind::name), Some(&what[2..]));
    } else {
        assert!(parsed.is_err());
    }
}

/// Fuzz target `comm_label`: three bytes for the numbers of a method, then
/// the program's own name, and after a 0x1F the name of its file (when the
/// separator is there).
pub fn comm_label(data: &[u8]) {
    let (nums, rest) = data.split_at(data.len().min(3));
    let (comm, exe) = match rest.iter().position(|b| *b == SEP) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    comm_bytes(comm, exe);
    if let [a, b, c] = nums {
        let n = |x: &u8| i32::from(*x as i8);
        let label = String::from_utf8_lossy(rest);
        argv_parses(*a, n(b), n(c), n(a), &label);
    }
}

// ---------------------------------------------------------------------------
// Notification text

/// Nothing in the escaped text is markup, and it reads back as it was.
pub fn escape_text(s: &str) {
    let out = notify::escape(s);
    assert!(!out.contains(['<', '>']), "{out:?}");
    let mut back = String::new();
    let mut rest = out.as_str();
    while let Some(i) = rest.find('&') {
        back.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (ch, n) = if tail.starts_with("&amp;") {
            ('&', 5)
        } else if tail.starts_with("&lt;") {
            ('<', 4)
        } else if tail.starts_with("&gt;") {
            ('>', 4)
        } else {
            panic!("a bare & in {out:?}");
        };
        back.push(ch);
        rest = &tail[n..];
    }
    back.push_str(rest);
    assert_eq!(back, s);
}

// ---------------------------------------------------------------------------
// The PNG on stdin

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                crc >> 1 ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Makes the checksum of the first chunk right, so that a changed size is
/// judged by the size check and not by the PNG library's checksum.
pub fn fix_ihdr_crc(png: &mut [u8]) {
    if png.len() >= 33 && png.starts_with(SIGNATURE) {
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
    }
}

/// Bytes are accepted only as a PNG with a header and a size a screen has.
pub fn png_bytes(data: &[u8]) {
    match pngin::read_png(data) {
        Ok(out) => {
            assert_eq!(out, data, "the bytes changed");
            assert!(data.len() >= 33 && data.starts_with(SIGNATURE));
            assert_eq!(&data[8..12], &13u32.to_be_bytes());
            assert_eq!(&data[12..16], b"IHDR");
            let w = u32::from_be_bytes(data[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(data[20..24].try_into().unwrap());
            assert!(
                (1..=16384).contains(&w) && (1..=16384).contains(&h),
                "{w}x{h}"
            );
        }
        Err(e) => assert!(!e.is_empty()),
    }
}

/// Fuzz target `png_header`: the bytes as they are, and with the header's
/// checksum made right.
pub fn png_header(data: &[u8]) {
    png_bytes(data);
    let mut fixed = data.to_vec();
    fix_ihdr_crc(&mut fixed);
    png_bytes(&fixed);
}

/// A real PNG of this size (cut to 1..=16 a side) is taken unchanged, and
/// its pixels are the ones that went in.
pub fn png_round_trip(w: u32, h: u32, seed: &[u8]) {
    use image::ImageEncoder;

    let (w, h) = (1 + w % 16, 1 + h % 16);
    let raw: Vec<u8> = (0..w as usize * h as usize * 4)
        .map(|i| seed.get(i % seed.len().max(1)).copied().unwrap_or(0))
        .collect();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&raw, w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    assert_eq!(pngin::read_png(&png[..]).unwrap(), png);
    let img = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (w, h));
    assert_eq!(img.as_raw(), &raw);
    png_bytes(&png);
}

// ---------------------------------------------------------------------------
// Redaction

/// The ranges are in the text, on character boundaries, not empty, sorted.
pub fn redact_flags(text: &str, flags: u8) {
    let cfg = config::RedactConfig {
        emails: flags & 1 != 0,
        ipv4: flags & 2 != 0,
        ipv6: flags & 4 != 0,
        mac: flags & 8 != 0,
        ..Default::default()
    };
    let found = redact::find_sensitive(text, &cfg);
    for &(a, b) in &found {
        assert!(a < b && b <= text.len(), "{a}..{b} of {}", text.len());
        assert!(
            text.is_char_boundary(a) && text.is_char_boundary(b),
            "{a}..{b} cuts a character"
        );
    }
    assert!(
        found.windows(2).all(|w| w[0] < w[1]),
        "not sorted and unique"
    );
    if flags & 15 == 0 {
        assert!(found.is_empty());
    }
}

/// Fuzz target `redact_text`: a byte of switches, then the text.
pub fn redact_text(data: &[u8]) {
    let Some((flags, text)) = data.split_first() else {
        return;
    };
    redact_flags(&String::from_utf8_lossy(text), *flags);
}
