//! telamon-screenshot: freeze the screen, drag a region, get it on the
//! clipboard. The modifier held when the drag ends picks what you get:
//!
//! | Drag        | Clipboard                                        |
//! |-------------|--------------------------------------------------|
//! | plain       | `image/png` of the region                        |
//! | Ctrl        | `text/plain`: the text OCR reads in the region   |
//! | Alt         | `image/png` with e-mails, IPs and MACs covered   |
//!
//! Data flow: capture (one frozen frame) -> overlay (selection + mode) ->
//! crop -> mode -> optional save (only if configured) -> clipboard.

mod capture;
mod clipboard;
mod config;
mod legacy;
#[cfg(feature = "ocr")]
mod models;
mod notify;
mod ocr;
mod overlay;
mod redact;
mod watchdog;

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use capture::Rect;
use clipboard::Payload;
use config::Config;

const USAGE: &str = "\
Usage: telamon-screenshot [--mode image|text|redact] [--region X,Y,WxH]

Freezes the screen and copies the region you drag to the clipboard.
Hold Ctrl when releasing for the text in it (OCR), or Alt for the image
with e-mail, IP and MAC addresses covered. Escape cancels.

  --mode MODE      what a plain drag (or --region) copies: image (default),
                   text or redact
  --region X,Y,WxH skip the overlay and take this region, in logical
                   desktop coordinates
  -h, --help       show this help
  -V, --version    show the version

Exit status: 0 copied, 1 cancelled or failed, 2 bad arguments.
Config: ~/.config/telamon-screenshot/config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Image,
    Text,
    Redact,
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    mode: Option<Mode>,
    region: Option<Rect>,
}

#[derive(Debug, PartialEq)]
enum Action {
    Run(Args),
    Help,
    Version,
}

fn main() -> ExitCode {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("telamon-screenshot: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let args = match action {
        Action::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Action::Version => {
            println!("telamon-screenshot {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Action::Run(args) => args,
    };

    // Descriptors 0-2 must be taken, or a socket opened later could land
    // there and be replaced by /dev/null in the clipboard server.
    for fd in 0..3 {
        // SAFETY: probing and opening plain descriptors.
        unsafe {
            if libc::fcntl(fd, libc::F_GETFD) == -1 {
                libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            }
        }
    }
    // From a hotkey stderr goes nowhere: a bug must still be reported.
    std::panic::set_hook(Box::new(|info| {
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let at = info
            .location()
            .map(|l| format!(" ({l})"))
            .unwrap_or_default();
        notify::show(&format!(
            "something went wrong: {what}{at}; nothing was copied"
        ));
    }));

    let (cfg, warning) = config::load();
    if let Some(w) = warning {
        notify::show(&w);
    }
    match run(&args, &cfg) {
        Ok(true) => ExitCode::SUCCESS,
        // Cancelled: silent.
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            notify::show(&e);
            ExitCode::from(1)
        }
    }
}

/// `Ok(false)` when the user cancelled.
fn run(args: &Args, cfg: &Config) -> Result<bool, String> {
    // One overlay at a time: a second press while one is up does nothing.
    let _lock = match args.region {
        Some(_) => None,
        None => match single_instance() {
            Some(lock) => Some(lock),
            None => {
                notify::show("a screenshot is already in progress");
                return Ok(false);
            }
        },
    };
    let watchdog = watchdog::Watchdog::arm(
        std::time::Duration::from_secs(45),
        "the screen couldn't be captured",
    );
    let mut session = capture::Session::connect()?;
    let frame = Rc::new(capture::capture_workspace(
        &mut session,
        cfg.capture.include_cursor,
    )?);
    drop(watchdog);

    let (rect, mode) = match args.region {
        Some(r) => (r, args.mode.unwrap_or(Mode::Image)),
        None => {
            let style = overlay::Style {
                dim: cfg.overlay.dim,
                accent: cfg.accent_rgb(),
            };
            match overlay::select(session.connection(), Rc::clone(&frame), style)? {
                None => return Ok(false),
                Some((r, m)) => (r, mode_for(m, args.mode)),
            }
        }
    };
    // Close the Wayland connection now: the clipboard server is forked off
    // later and must not inherit it.
    drop(session);

    let mut crop = frame
        .crop(&rect)
        .ok_or("the selection is outside every screen")?;
    drop(frame);

    // Too small to hold a line of text (and below what the models take).
    let readable = crop.width() >= MIN_OCR_PX && crop.height() >= MIN_OCR_PX;
    let payload = match mode {
        Mode::Image => Payload::Png(encode_png(&crop)?),
        Mode::Text => {
            if !readable {
                return Err("the selection is too small to read; nothing was copied".into());
            }
            let text = load_ocr()?.text(&crop)?;
            if text.is_empty() {
                return Err("no text found in the selection; nothing was copied".into());
            }
            Payload::Text(text)
        }
        Mode::Redact => {
            let boxes = if readable {
                let lines = load_ocr()?.lines(&crop)?;
                redact::boxes(&lines, &cfg.redact)
            } else {
                Vec::new()
            };
            if boxes.is_empty() {
                // Redaction is best-effort: say when it covered nothing.
                notify::show("nothing to redact was found; the image was copied as it is");
            }
            redact::apply(&mut crop, &boxes, cfg.fill_rgb());
            Payload::Png(encode_png(&crop)?)
        }
    };
    if let (Payload::Png(png), Some(dir)) = (&payload, &cfg.output.save_dir)
        && let Err(e) = save_png(dir, png)
    {
        // The copy still happens; only the saved file is missing.
        notify::show(&e);
    }
    clipboard::copy(payload)?;
    Ok(true)
}

const MIN_OCR_PX: u32 = 8;

/// Exclusive locks on `$XDG_RUNTIME_DIR/telamon-screenshot.lock` and, for this
/// release, on `atlasos-screenshot.lock`, which a still running
/// `atlasos-screenshot` of an earlier version holds: `None` when another
/// instance holds either. Without a runtime dir (or if a lock can't be made)
/// there is no guard, rather than no screenshot.
fn single_instance() -> Option<Vec<std::fs::File>> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return Some(Vec::new());
    };
    if !dir.is_absolute() {
        return Some(Vec::new());
    }
    lock_both(&dir)
}

fn lock_both(dir: &Path) -> Option<Vec<std::fs::File>> {
    let mut held = Vec::new();
    for name in [
        format!("{}.lock", legacy::NAME),
        format!("{}.lock", legacy::OLD_NAME),
    ] {
        match lock_file(dir, &name) {
            Lock::Held(f) => held.push(f),
            Lock::Busy => return None,
            Lock::Unavailable => {}
        }
    }
    Some(held)
}

enum Lock {
    Held(std::fs::File),
    Busy,
    Unavailable,
}

fn lock_file(dir: &Path, name: &str) -> Lock {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join(name))
    else {
        return Lock::Unavailable;
    };
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Lock::Held(file),
        Err(rustix::io::Errno::WOULDBLOCK) => Lock::Busy,
        Err(_) => Lock::Unavailable,
    }
}

/// Ctrl wins over Alt; with neither, `--mode` (or a plain image).
fn mode_for(m: overlay::Mods, default: Option<Mode>) -> Mode {
    if m.ctrl {
        Mode::Text
    } else if m.alt {
        Mode::Redact
    } else {
        default.unwrap_or(Mode::Image)
    }
}

#[cfg(feature = "ocr")]
fn load_ocr() -> Result<ocr::Ocr, String> {
    let models = models::ensure(&|| notify::show(models::DOWNLOADING))
        .map_err(|e| format!("couldn't download the text recognition models: {e}"))?;
    ocr::Ocr::new(models)
}

#[cfg(not(feature = "ocr"))]
fn load_ocr() -> Result<ocr::Ocr, String> {
    Err(ocr::NO_OCR.into())
}

fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};

    let mut out = Vec::new();
    // Fast compression: the clipboard wants it now, not 30% smaller.
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("can't encode the PNG: {e}"))?;
    Ok(out)
}

/// `Screenshot_<local time>.png` in `dir`, never replacing a file that's
/// there. The file is made unnamed (`O_TMPFILE`) and linked in under its
/// final name, so no half-written or temp file is ever visible. Where that
/// can't work (no `O_TMPFILE`, no /proc), a random temp name is renamed.
fn save_png(dir: &Path, png: &[u8]) -> Result<PathBuf, String> {
    use rustix::fs::{Mode, OFlags};

    let fail = |e: std::io::Error| format!("can't save the screenshot in {}: {e}", dir.display());
    let dfd = rustix::fs::open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| fail(e.into()))?;
    let st = rustix::fs::fstat(&dfd).map_err(|e| fail(e.into()))?;
    // Others able to write here (without the sticky bit) could swap files.
    // Group write is fine for the user's own (private) group.
    let sticky = st.st_mode & 0o1000 != 0;
    let foreign_group = st.st_gid != rustix::process::getegid().as_raw();
    if !sticky && (st.st_mode & 0o002 != 0 || (st.st_mode & 0o020 != 0 && foreign_group)) {
        return Err(format!(
            "can't save the screenshot in {}: other users can write to it",
            dir.display()
        ));
    }
    let stamp = local_stamp(std::time::SystemTime::now());
    let names: Vec<String> = (0..100)
        .map(|n| match n {
            0 => format!("Screenshot_{stamp}.png"),
            n => format!("Screenshot_{stamp}-{n}.png"),
        })
        .collect();
    let saved = match save_unnamed(&dfd, png, &names) {
        Ok(Some(name)) => Ok(Some(name)),
        Ok(None) => save_renamed(&dfd, png, &names),
        Err(e) => Err(e),
    }
    .map_err(fail)?;
    let _ = rustix::fs::fsync(&dfd);
    match saved {
        Some(name) => Ok(dir.join(name)),
        None => Err(format!(
            "can't save the screenshot in {}: too many with the same time",
            dir.display()
        )),
    }
}

fn write_synced(fd: rustix::fd::OwnedFd, png: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::from(fd);
    f.write_all(png)?;
    f.sync_all()
}

const SAVE_MODE: rustix::fs::Mode = rustix::fs::Mode::RUSR.union(rustix::fs::Mode::WUSR);

/// `O_TMPFILE` + `linkat`. `Ok(None)` (before anything is visible) when the
/// file system or a missing /proc rules it out.
fn save_unnamed(
    dfd: &rustix::fd::OwnedFd,
    png: &[u8],
    names: &[String],
) -> std::io::Result<Option<String>> {
    use rustix::fs::{AtFlags, OFlags};
    use rustix::io::Errno;

    let flags = OFlags::WRONLY | OFlags::CLOEXEC | OFlags::TMPFILE;
    let fd = match rustix::fs::openat(dfd, ".", flags, SAVE_MODE) {
        Ok(fd) => fd,
        Err(Errno::OPNOTSUPP | Errno::ISDIR | Errno::INVAL) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let proc_path = format!("/proc/self/fd/{}", rustix::fd::AsRawFd::as_raw_fd(&fd));
    write_synced(rustix::io::dup(&fd)?, png)?;
    for name in names {
        match rustix::fs::linkat(
            rustix::fs::CWD,
            &proc_path,
            dfd,
            name,
            AtFlags::SYMLINK_FOLLOW,
        ) {
            Ok(()) => return Ok(Some(name.clone())),
            Err(Errno::EXIST) => continue,
            Err(Errno::NOENT | Errno::PERM | Errno::NOSYS) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::other("too many with the same time"))
}

/// A random temp name, renamed without replacing. `Ok(None)`: no free name.
fn save_renamed(
    dfd: &rustix::fd::OwnedFd,
    png: &[u8],
    names: &[String],
) -> std::io::Result<Option<String>> {
    use rustix::fs::{AtFlags, OFlags};
    use std::hash::{BuildHasher, Hasher};

    let r = std::hash::RandomState::new().build_hasher().finish();
    let tmp = format!(".telamon-screenshot-{r:016x}.tmp");
    let flags = OFlags::WRONLY | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
    let fd = rustix::fs::openat(dfd, &tmp, flags, SAVE_MODE)?;
    let result = write_synced(fd, png).and_then(|()| {
        for name in names {
            match rustix::fs::renameat_with(
                dfd,
                &tmp,
                dfd,
                name,
                rustix::fs::RenameFlags::NOREPLACE,
            ) {
                Ok(()) => return Ok(Some(name.clone())),
                Err(rustix::io::Errno::EXIST) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    });
    if !matches!(result, Ok(Some(_))) {
        let _ = rustix::fs::unlinkat(dfd, &tmp, AtFlags::empty());
    }
    result
}

/// The time on the user's clock (the UTC stamp shifted by the local offset).
fn local_stamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = secs as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` we own.
    let off = if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        0
    } else {
        tm.tm_gmtoff
    };
    utc_stamp(std::time::UNIX_EPOCH + std::time::Duration::from_secs((secs + off).max(0) as u64))
}

/// `2026-10-05_17-40-12` (UTC), from the civil-from-days algorithm.
fn utc_stamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut args = Args::default();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--mode" => {
                let v = it.next().ok_or("--mode needs a value")?;
                args.mode = Some(match v.as_str() {
                    "image" => Mode::Image,
                    "text" => Mode::Text,
                    "redact" => Mode::Redact,
                    _ => return Err(format!("unknown mode {v:?}")),
                });
            }
            "--region" => {
                let v = it.next().ok_or("--region needs a value")?;
                args.region = Some(
                    parse_region(&v)
                        .ok_or_else(|| format!("bad region {v:?}, expected X,Y,WxH"))?,
                );
            }
            _ => return Err(format!("unknown argument {a:?}")),
        }
    }
    Ok(Action::Run(args))
}

/// `X,Y,WxH` (grim/slurp style), bounded to sane desktop sizes.
fn parse_region(s: &str) -> Option<Rect> {
    let (x, rest) = s.split_once(',')?;
    let (y, size) = rest.split_once(',')?;
    let (w, h) = size.split_once('x')?;
    let num = |v: &str, lo: i32| {
        v.trim()
            .parse::<i32>()
            .ok()
            .filter(|n| (lo..=65535).contains(n))
    };
    Some(Rect::new(
        num(x, -65535)?,
        num(y, -65535)?,
        num(w, 1)?,
        num(h, 1)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Result<Action, String> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]).unwrap(), Action::Run(Args::default()));
        assert_eq!(parse(&["--help", "--bogus"]).unwrap(), Action::Help);
        assert_eq!(
            parse(&["--mode", "text", "--region", "-10,20,300x40"]).unwrap(),
            Action::Run(Args {
                mode: Some(Mode::Text),
                region: Some(Rect::new(-10, 20, 300, 40))
            })
        );
        assert!(parse(&["--mode"]).is_err());
        assert!(parse(&["--mode", "ocr"]).is_err());
        assert!(parse(&["--region", "1,2,0x5"]).is_err());
        assert!(parse(&["--region", "1,2,3"]).is_err());
        assert!(parse(&["--region", "1,2,99999x5"]).is_err());
        assert!(parse(&["shot.png"]).is_err());
    }

    #[test]
    fn either_lock_name_keeps_a_second_instance_out() {
        let dir =
            std::env::temp_dir().join(format!("telamon-screenshot-locks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = lock_both(&dir).expect("free");
        assert_eq!(first.len(), 2, "both names are held");
        assert!(lock_both(&dir).is_none(), "a second instance is refused");
        drop(first);

        // An instance of the old name holds only its own lock file.
        let Lock::Held(old) = lock_file(&dir, "atlasos-screenshot.lock") else {
            panic!()
        };
        assert!(
            lock_both(&dir).is_none(),
            "the old name's lock is respected"
        );
        drop(old);
        // And the other way: the new name's lock stops an old one's try.
        let Lock::Held(new) = lock_file(&dir, "telamon-screenshot.lock") else {
            panic!()
        };
        assert!(lock_both(&dir).is_none());
        drop(new);
        assert!(lock_both(&dir).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn modifiers_pick_the_mode() {
        use overlay::Mods;
        assert_eq!(
            mode_for(
                Mods {
                    ctrl: false,
                    alt: false
                },
                None
            ),
            Mode::Image
        );
        assert_eq!(
            mode_for(
                Mods {
                    ctrl: false,
                    alt: false
                },
                Some(Mode::Redact)
            ),
            Mode::Redact
        );
        assert_eq!(
            mode_for(
                Mods {
                    ctrl: true,
                    alt: false
                },
                None
            ),
            Mode::Text
        );
        assert_eq!(
            mode_for(
                Mods {
                    ctrl: false,
                    alt: true
                },
                Some(Mode::Text)
            ),
            Mode::Redact
        );
        assert_eq!(
            mode_for(
                Mods {
                    ctrl: true,
                    alt: true
                },
                None
            ),
            Mode::Text
        );
    }

    #[test]
    fn timestamps() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_250_812);
        assert_eq!(utc_stamp(t), "2026-10-06_01-40-12");
        assert_eq!(utc_stamp(std::time::UNIX_EPOCH), "1970-01-01_00-00-00");
    }

    #[test]
    fn png_round_trip_and_save_never_overwrites() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let png = encode_png(&img).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), img);

        let dir =
            std::env::temp_dir().join(format!("telamon-screenshot-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = save_png(&dir, &png).unwrap();
        let b = save_png(&dir, &png).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), png);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);

        // A folder others can write to (no sticky bit) is refused.
        let open = dir.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o777))
            .unwrap();
        assert!(save_png(&open, &png).unwrap_err().contains("other users"));
        // Group write in the user's own group is fine (umask 002).
        std::fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o775))
            .unwrap();
        assert!(save_png(&open, &png).is_ok());
        // Both ways of saving leave exactly the file.
        let names = vec!["x.png".to_string()];
        let dfd = rustix::fs::open(
            &open,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        assert_eq!(
            save_renamed(&dfd, &png, &names).unwrap().as_deref(),
            Some("x.png")
        );
        assert_eq!(save_renamed(&dfd, &png, &names).unwrap(), None);
        assert_eq!(std::fs::read_dir(&open).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
