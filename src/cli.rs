//! The command line: a fixed grammar, no files, no values the shell could
//! be asked to expand.

use crate::capture::Rect;
use crate::modes::{Kind, Mode};

pub const USAGE: &str = "\
Usage: telamon-screenshot [WHAT] [OPTIONS]

Takes a screenshot, saves it in Pictures/Screenshots and copies it to the
clipboard. With no WHAT it does what capture.default_mode says (a region).

What to capture:
  -r, --region [X,Y,WxH]  a region: drag one on the frozen screen, or give
                          it in logical desktop coordinates to skip the overlay
  -f, --full              every screen, as one image
  -m, --screen            the screen the pointer is on
  -a, --active-window     the window that has the focus
  -u, --window            a window you click

Options:
  --delay N        wait N seconds first (0 to 600), with a small countdown
                   that is never in the picture
  --mode MODE      image (default), text (OCR, copied, not saved) or redact
                   (e-mail, IP and MAC addresses covered)
  --no-save        copy only; keep nothing on disk (output.save = false does
                   this for every shot)
  --no-notify      no notification after the capture (errors are still shown)
  --notify         a notification even when output.notify is false (what the
                   org.kde.Spectacle service uses, so no capture is silent)
  --requested-by NAME  who asked for this capture (the D-Bus service says
                   which program); shown in the notification
  --edit           open the editor with the picture
  --cursor, --no-cursor    with or without the pointer
  --no-frame       windows without their title bar and borders
  --no-shadow      windows without their shadow
  -h, --help       show this help
  -V, --version    show the version

While dragging a region, hold Ctrl for the text in it, Alt to cover
addresses, Shift to open the editor; Escape or the right button cancels.
The path of the saved file is printed on stdout.

Helpers (used by the editor): --save-png and --copy-png read a PNG on stdin;
--dbus serves org.kde.Spectacle for programs that ask Spectacle for shots.

Exit status: 0 done, 1 cancelled or failed, 2 bad arguments.
Config: ~/.config/telamon-screenshot/config.toml";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// `None`: the config's `capture.default_mode`.
    pub kind: Option<Kind>,
    pub mode: Option<Mode>,
    /// A region given on the command line (kind is `Region`).
    pub region: Option<Rect>,
    pub delay: u32,
    pub no_save: bool,
    pub no_notify: bool,
    /// A notification even when `output.notify` is false.
    pub notify: bool,
    /// Who asked, for the notification (the D-Bus service sets it).
    pub requested_by: Option<String>,
    pub edit: bool,
    pub cursor: Option<bool>,
    pub frame: Option<bool>,
    pub shadow: Option<bool>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Run(Args),
    Help,
    Version,
    /// Save the PNG on stdin like a capture; print the path.
    SavePng,
    /// Put the PNG on stdin on the clipboard.
    CopyPng,
    /// Serve `org.kde.Spectacle`.
    Dbus,
    /// The notification and editor helper a capture starts (job on stdin).
    Post,
}

pub const MAX_DELAY: u32 = 600;

pub fn parse_args(it: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut it = it.peekable();
    let mut args = Args::default();
    let set_kind = |args: &mut Args, k: Kind| -> Result<(), String> {
        match args.kind {
            Some(old) if old != k => Err(format!(
                "choose one of --{}, --{} (not both)",
                old.name(),
                k.name()
            )),
            _ => {
                args.kind = Some(k);
                Ok(())
            }
        }
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--save-png" => return only(&mut it, "--save-png", Action::SavePng),
            "--copy-png" => return only(&mut it, "--copy-png", Action::CopyPng),
            "--dbus" => return only(&mut it, "--dbus", Action::Dbus),
            "--post" => return only(&mut it, "--post", Action::Post),
            "-f" | "--full" => set_kind(&mut args, Kind::Full)?,
            "-m" | "--screen" => set_kind(&mut args, Kind::Screen)?,
            "-a" | "--active-window" => set_kind(&mut args, Kind::ActiveWindow)?,
            "-u" | "--window" => set_kind(&mut args, Kind::Window)?,
            "-r" | "--region" => {
                set_kind(&mut args, Kind::Region)?;
                // The geometry is optional: `--region --delay 3` is a region
                // to drag. Anything else after it is an error, not a flag.
                if let Some(next) = it.peek()
                    && !next.starts_with("--")
                    && !next.starts_with("-h")
                    && next != "-V"
                    && !matches!(next.as_str(), "-f" | "-m" | "-a" | "-u" | "-r")
                {
                    let v = it.next().unwrap_or_default();
                    args.region = Some(
                        parse_region(&v)
                            .ok_or_else(|| format!("bad region {v:?}, expected X,Y,WxH"))?,
                    );
                }
            }
            "--delay" => {
                let v = it.next().ok_or("--delay needs a number of seconds")?;
                args.delay = v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n <= MAX_DELAY)
                    .ok_or_else(|| format!("bad delay {v:?}, expected 0 to {MAX_DELAY} seconds"))?;
            }
            "--mode" => {
                let v = it.next().ok_or("--mode needs a value")?;
                args.mode = Some(Mode::parse(&v).ok_or_else(|| format!("unknown mode {v:?}"))?);
            }
            "--no-save" => args.no_save = true,
            "--no-notify" => args.no_notify = true,
            "--notify" => args.notify = true,
            "--requested-by" => {
                let v = it.next().ok_or("--requested-by needs a name")?;
                args.requested_by = Some(
                    check_label(&v).ok_or_else(|| format!("bad name {v:?} for --requested-by"))?,
                );
            }
            "--edit" => args.edit = true,
            "--cursor" => args.cursor = Some(true),
            "--no-cursor" => args.cursor = Some(false),
            "--frame" => args.frame = Some(true),
            "--no-frame" => args.frame = Some(false),
            "--shadow" => args.shadow = Some(true),
            "--no-shadow" => args.shadow = Some(false),
            _ => return Err(format!("unknown argument {a:?}")),
        }
    }
    if args.notify && args.no_notify {
        return Err("choose one of --notify, --no-notify (not both)".into());
    }
    Ok(Action::Run(args))
}

/// The longest name `--requested-by` takes.
pub const MAX_LABEL: usize = 64;

/// A name for the notification: printable, one line, not empty. Anything
/// else is refused rather than cleaned, so what is shown is what was given.
fn check_label(s: &str) -> Option<String> {
    let ok = !s.trim().is_empty()
        && s.chars().count() <= MAX_LABEL
        && !s.chars().any(|c| c.is_control());
    ok.then(|| s.to_string())
}

/// A helper mode takes no other argument.
fn only(
    it: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
    action: Action,
) -> Result<Action, String> {
    match it.next() {
        None => Ok(action),
        Some(extra) => Err(format!("{flag} takes no other argument, not {extra:?}")),
    }
}

/// `X,Y,WxH` (grim/slurp style), bounded to sane desktop sizes.
pub fn parse_region(s: &str) -> Option<Rect> {
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

    fn run(v: &[&str]) -> Args {
        match parse(v).unwrap() {
            Action::Run(a) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_arguments_is_the_default_capture() {
        assert_eq!(run(&[]), Args::default());
    }

    #[test]
    fn every_kind_has_its_flag() {
        assert_eq!(run(&["--full"]).kind, Some(Kind::Full));
        assert_eq!(run(&["-f"]).kind, Some(Kind::Full));
        assert_eq!(run(&["--screen"]).kind, Some(Kind::Screen));
        assert_eq!(run(&["--window"]).kind, Some(Kind::Window));
        assert_eq!(run(&["--active-window"]).kind, Some(Kind::ActiveWindow));
        assert_eq!(run(&["--region"]).kind, Some(Kind::Region));
        assert_eq!(run(&["--region"]).region, None);
    }

    #[test]
    fn one_kind_only() {
        assert!(parse(&["--full", "--window"]).is_err());
        assert!(parse(&["--full", "--region", "0,0,1x1"]).is_err());
        // The same twice is fine.
        assert!(parse(&["--full", "-f"]).is_ok());
    }

    #[test]
    fn region_with_and_without_geometry() {
        let a = run(&["--region", "-10,20,300x40", "--mode", "text"]);
        assert_eq!(a.kind, Some(Kind::Region));
        assert_eq!(a.region, Some(Rect::new(-10, 20, 300, 40)));
        assert_eq!(a.mode, Some(Mode::Text));
        // The next token is a flag, not a geometry.
        let a = run(&["--region", "--delay", "3"]);
        assert_eq!(a.region, None);
        assert_eq!(a.delay, 3);
        let a = run(&["--region", "--edit"]);
        assert!(a.edit && a.region.is_none());
        assert!(parse(&["--region", "1,2,0x5"]).is_err());
        assert!(parse(&["--region", "1,2,3"]).is_err());
        assert!(parse(&["--region", "1,2,99999x5"]).is_err());
        assert!(parse(&["--region", "shot.png"]).is_err());
    }

    #[test]
    fn delay_is_bounded() {
        assert_eq!(run(&["--delay", "0"]).delay, 0);
        assert_eq!(run(&["--delay", "600"]).delay, 600);
        assert!(parse(&["--delay", "601"]).is_err());
        assert!(parse(&["--delay", "-1"]).is_err());
        assert!(parse(&["--delay", "2.5"]).is_err());
        assert!(parse(&["--delay"]).is_err());
    }

    #[test]
    fn modes_and_switches() {
        assert_eq!(run(&["--mode", "redact"]).mode, Some(Mode::Redact));
        assert!(parse(&["--mode"]).is_err());
        assert!(parse(&["--mode", "ocr"]).is_err());
        let a = run(&[
            "--window",
            "--no-save",
            "--no-notify",
            "--edit",
            "--no-cursor",
            "--no-frame",
            "--shadow",
        ]);
        assert!(a.no_save && a.no_notify && a.edit);
        assert_eq!(a.cursor, Some(false));
        assert_eq!(a.frame, Some(false));
        assert_eq!(a.shadow, Some(true));
    }

    #[test]
    fn notify_and_requester() {
        let a = run(&["--full", "--notify", "--requested-by", "KDE Connect"]);
        assert!(a.notify && !a.no_notify);
        assert_eq!(a.requested_by.as_deref(), Some("KDE Connect"));
        assert!(parse(&["--notify", "--no-notify"]).is_err());
        // The value is taken whatever it looks like, and checked.
        let a = run(&["--requested-by", "--full"]);
        assert_eq!(a.requested_by.as_deref(), Some("--full"));
        assert_eq!(a.kind, None);
        for bad in ["", "   ", "a\nb", "a\u{1b}[31m", &"x".repeat(MAX_LABEL + 1)] {
            assert!(parse(&["--requested-by", bad]).is_err(), "{bad:?}");
        }
        assert!(parse(&["--requested-by"]).is_err());
        assert!(parse(&["--requested-by", &"x".repeat(MAX_LABEL)]).is_ok());
    }

    #[test]
    fn helpers_and_info_flags() {
        assert_eq!(parse(&["--help", "--bogus"]).unwrap(), Action::Help);
        assert_eq!(parse(&["-V"]).unwrap(), Action::Version);
        assert_eq!(parse(&["--save-png"]).unwrap(), Action::SavePng);
        assert_eq!(parse(&["--copy-png"]).unwrap(), Action::CopyPng);
        assert_eq!(parse(&["--dbus"]).unwrap(), Action::Dbus);
        assert_eq!(parse(&["--post"]).unwrap(), Action::Post);
        assert!(parse(&["--dbus", "--full"]).is_err());
        assert!(parse(&["--save-png", "x.png"]).is_err());
        assert!(parse(&["shot.png"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
