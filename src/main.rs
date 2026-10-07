//! telamon-screenshot: a screenshot tool for Telamon OS, the replacement for
//! Spectacle. It captures the whole desktop, one screen, a window, or a region
//! you drag on the frozen desktop, saves the picture in Pictures/Screenshots,
//! copies it to the clipboard and tells you with a notification (Open, Show in
//! Folder, Edit, Copy). The modifier held when a region drag ends picks what
//! you get:
//!
//! | Drag        | Result                                           |
//! |-------------|--------------------------------------------------|
//! | plain       | `image/png` of the region, saved and copied      |
//! | Ctrl        | `text/plain`: the text OCR reads, copied         |
//! | Alt         | `image/png` with e-mails, IPs and MACs covered   |
//! | Shift       | the plain picture, opened in the editor          |
//!
//! Data flow: delay (countdown) -> capture (a frozen frame for regions, or
//! KWin's picture of a screen or window) -> overlay (region + modifiers) ->
//! mode -> save -> clipboard -> notification or editor (a detached child).

mod actions;
mod capture;
mod cli;
mod clipboard;
mod config;
mod countdown;
mod dbus;
mod legacy;
#[cfg(feature = "ocr")]
mod models;
mod modes;
mod notify;
mod ocr;
mod overlay;
mod post;
mod redact;
mod store;
mod watchdog;

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use capture::GrabOpts;
use cli::{Action, Args, USAGE};
use clipboard::Payload;
use config::Config;
use modes::{Kind, Mode};

fn main() -> ExitCode {
    let action = match cli::parse_args(std::env::args().skip(1)) {
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
        Action::Run(args) => Some(args),
        Action::Post => return ExitCode::from(post::run_helper() as u8),
        Action::SavePng | Action::CopyPng | Action::Dbus => None,
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
    let result = match args {
        Some(args) => run(&args, &cfg),
        None => helper(&cfg),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        // Cancelled: silent.
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            notify::show(&e);
            ExitCode::from(1)
        }
    }
}

/// `--save-png`, `--copy-png` and `--dbus`: the editor's and the D-Bus
/// service's way in. Errors are plain lines on stderr (the callers show
/// them), not notifications.
fn helper(cfg: &Config) -> Result<bool, String> {
    let action = cli::parse_args(std::env::args().skip(1))?;
    match action {
        Action::Dbus => dbus::serve().map(|()| true),
        Action::SavePng => {
            let png = read_png(std::io::stdin().lock())?;
            let path = save_capture(&png, cfg)?;
            println!("{}", path.display());
            Ok(true)
        }
        Action::CopyPng => {
            let png = read_png(std::io::stdin().lock())?;
            clipboard::copy(Payload::Png(png))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The biggest PNG taken from stdin.
const MAX_STDIN_PNG: u64 = 256 * 1024 * 1024;

/// A PNG from `input`, checked: the signature, and a size that is a screen's
/// and not a bomb's. The bytes are kept as they are.
fn read_png(input: impl Read) -> Result<Vec<u8>, String> {
    use image::ImageDecoder;

    let mut png = Vec::new();
    input
        .take(MAX_STDIN_PNG + 1)
        .read_to_end(&mut png)
        .map_err(|e| format!("can't read the picture: {e}"))?;
    if png.len() as u64 > MAX_STDIN_PNG {
        return Err("the picture is too large".into());
    }
    let dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&png))
        .map_err(|_| "that is not a PNG picture".to_string())?;
    let (w, h) = dec.dimensions();
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(format!("a {w}x{h} picture is too large"));
    }
    Ok(png)
}

/// `Ok(false)` when the user cancelled.
fn run(args: &Args, cfg: &Config) -> Result<bool, String> {
    let kind = if args.region.is_some() {
        Kind::Region
    } else {
        args.kind.unwrap_or(cfg.capture.default_mode)
    };
    // One capture at a time: a second press while one is up does nothing.
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
    countdown::wait(args.delay, cfg.accent_rgb());

    let Some((mut crop, mods)) = capture_picture(kind, args, cfg)? else {
        return Ok(false);
    };
    let mode = mode_for(mods, args.mode);
    // Text goes to the clipboard only: there is no picture to edit.
    let edit = (args.edit || mods.shift) && !matches!(mode, Mode::Text);

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

    // Saved first: a failed save is reported, and the copy still happens.
    let want_save = cfg.output.save && !args.no_save;
    let (saved, save_error) = match &payload {
        Payload::Png(png) if want_save => match save_capture(png, cfg) {
            Ok(path) => (Some(path), None),
            Err(e) => (None, Some(e)),
        },
        _ => (None, None),
    };
    let notify = (cfg.output.notify && !args.no_notify) || save_error.is_some();
    let wants_post = edit || notify;
    let keep_png = match &payload {
        Payload::Png(png) if wants_post => Some(png.clone()),
        _ => None,
    };
    let thumb =
        (notify && !edit && keep_png.is_some() && saved.is_none()).then(|| notify::thumb(&crop));
    let words = words_for(&payload, saved.as_deref(), save_error.as_deref());
    drop(crop);

    clipboard::copy(payload)?;
    if let Some(path) = &saved {
        // For scripts: where it went.
        println!("{}", path.display());
    }
    post::finish(post::Done {
        title: words.0,
        body: words.1,
        png: keep_png,
        image: saved.clone().map(notify::Image::File).or(thumb),
        path: saved,
        edit,
        notify,
    });
    Ok(true)
}

/// The notification's title and body.
fn words_for(
    payload: &Payload,
    saved: Option<&Path>,
    save_error: Option<&str>,
) -> (String, String) {
    match (payload, saved, save_error) {
        (Payload::Text(t), _, _) => (
            "Text Copied".into(),
            format!("{} characters are on the clipboard.", t.chars().count()),
        ),
        (_, Some(path), _) => (
            "Screenshot Saved".into(),
            format!(
                "{}\nIt is also on the clipboard.",
                path.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ),
        ),
        (_, None, Some(e)) => (
            "Screenshot Copied".into(),
            format!("It is on the clipboard, but it couldn't be saved: {e}"),
        ),
        (_, None, None) => ("Screenshot Copied".into(), "It is on the clipboard.".into()),
    }
}

/// Saves into the configured folder, making the default one when it is
/// missing. The path of the new file.
fn save_capture(png: &[u8], cfg: &Config) -> Result<PathBuf, String> {
    let dir = store::save_dir(cfg).ok_or("there is no home folder to save in")?;
    store::ensure_default_dir(&dir, cfg)
        .map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    store::save_png(&dir, png)
}

/// Takes the picture. `Ok(None)`: cancelled. The modifiers are the ones held
/// when a region was released.
fn capture_picture(
    kind: Kind,
    args: &Args,
    cfg: &Config,
) -> Result<Option<(image::RgbaImage, overlay::Mods)>, String> {
    let cursor = args.cursor.unwrap_or(cfg.capture.include_cursor);
    if kind != Kind::Region {
        let _watchdog = (!kind.interactive()).then(|| {
            watchdog::Watchdog::arm(
                std::time::Duration::from_secs(45),
                "the screen couldn't be captured",
            )
        });
        let shot = capture::grab(
            kind,
            GrabOpts {
                cursor,
                frame: args.frame.unwrap_or(cfg.capture.window_frame),
                shadow: args.shadow.unwrap_or(cfg.capture.window_shadow),
            },
        )?;
        return Ok(shot.map(|img| (img, overlay::Mods::default())));
    }

    let watchdog = watchdog::Watchdog::arm(
        std::time::Duration::from_secs(45),
        "the screen couldn't be captured",
    );
    let mut session = capture::Session::connect()?;
    let frame = Rc::new(capture::capture_workspace(&mut session, cursor)?);
    drop(watchdog);

    let (rect, mods) = match args.region {
        Some(r) => (r, overlay::Mods::default()),
        None => {
            let style = overlay::Style {
                dim: cfg.overlay.dim,
                accent: cfg.accent_rgb(),
            };
            match overlay::select(session.connection(), Rc::clone(&frame), style)? {
                None => return Ok(None),
                Some(sel) => sel,
            }
        }
    };
    // Close the Wayland connection now: the clipboard server is forked off
    // later and must not inherit it.
    drop(session);
    let crop = frame
        .crop(&rect)
        .ok_or("the selection is outside every screen")?;
    Ok(Some((crop, mods)))
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let m = |ctrl, alt| Mods {
            ctrl,
            alt,
            shift: false,
        };
        assert_eq!(mode_for(m(false, false), None), Mode::Image);
        assert_eq!(mode_for(m(false, false), Some(Mode::Redact)), Mode::Redact);
        assert_eq!(mode_for(m(true, false), None), Mode::Text);
        assert_eq!(mode_for(m(false, true), Some(Mode::Text)), Mode::Redact);
        assert_eq!(mode_for(m(true, true), None), Mode::Text);
    }

    #[test]
    fn png_round_trip() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let png = encode_png(&img).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), img);
    }

    #[test]
    fn stdin_pictures_are_checked() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let png = encode_png(&img).unwrap();
        assert_eq!(read_png(&png[..]).unwrap(), png);
        assert!(read_png(&b"GIF89a"[..]).is_err());
        assert!(read_png(&b""[..]).is_err());
        // Too wide for a screen: refused.
        let wide = encode_png(&image::RgbaImage::new(16385, 1)).unwrap();
        assert!(read_png(&wide[..]).unwrap_err().contains("too large"));
    }

    #[test]
    fn notification_words() {
        let png = Payload::Png(vec![]);
        let (t, b) = words_for(&png, Some(Path::new("/p/Screenshot_1.png")), None);
        assert_eq!(t, "Screenshot Saved");
        assert!(b.starts_with("Screenshot_1.png"));
        let (t, b) = words_for(&png, None, Some("no room"));
        assert_eq!(t, "Screenshot Copied");
        assert!(b.contains("no room"));
        assert_eq!(words_for(&png, None, None).0, "Screenshot Copied");
        let (t, b) = words_for(&Payload::Text("héllo".into()), None, None);
        assert_eq!(t, "Text Copied");
        assert!(b.starts_with("5 characters"));
    }
}
