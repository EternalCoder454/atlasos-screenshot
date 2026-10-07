//! What the notification's buttons do: Open, Show in Folder, Edit and Copy.
//!
//! Every action is an argument vector run directly (`xdg-open`, the editor,
//! this binary), or a D-Bus call to the file manager. Nothing goes through a
//! shell, and the file's name is only ever one argument or one escaped URI.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;

/// The actions a notification offers, as `(key, label)`. Labels are KDE
/// wording: Title Case.
pub const OPEN: (&str, &str) = ("open", "Open");
pub const FOLDER: (&str, &str) = ("folder", "Show in Folder");
pub const EDIT: (&str, &str) = ("edit", "Edit");
pub const COPY: (&str, &str) = ("copy", "Copy");
/// The notification body being clicked (the server's "default" action).
pub const DEFAULT: &str = "default";

pub const EDITOR_NAME: &str = "telamon-screenshot-editor";

/// What the actions act on.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// This program (for Copy).
    pub exe: PathBuf,
    pub editor: PathBuf,
    /// The saved file, if it was saved.
    pub path: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// Run `prog args`; the PNG goes to its standard input when asked.
    Argv {
        prog: PathBuf,
        args: Vec<OsString>,
        png_on_stdin: bool,
    },
    /// `org.freedesktop.FileManager1.ShowItems`, else run `fallback`.
    ShowItems {
        uri: String,
        fallback: Vec<OsString>,
    },
}

/// This program's path, as the kernel knows it (a `(deleted)` suffix, left
/// when the package was updated while we run, is cut off).
pub fn self_exe() -> PathBuf {
    let exe =
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/telamon-screenshot"));
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => exe,
    }
}

/// The editor next to this program, else the packaged one.
pub fn editor_path(exe: &Path) -> PathBuf {
    let sibling = exe.with_file_name(EDITOR_NAME);
    if sibling.is_file() {
        sibling
    } else {
        Path::new("/usr/bin").join(EDITOR_NAME)
    }
}

pub fn ctx(path: Option<PathBuf>) -> Ctx {
    let exe = self_exe();
    let editor = editor_path(&exe);
    Ctx { exe, editor, path }
}

/// What to run for the action `key`; `None` when it is unknown or needs a
/// file that doesn't exist.
pub fn plan(key: &str, ctx: &Ctx) -> Option<Launch> {
    match key {
        "open" | DEFAULT => {
            let path = ctx.path.as_ref()?;
            Some(Launch::Argv {
                prog: "xdg-open".into(),
                args: vec![path.as_os_str().to_owned()],
                png_on_stdin: false,
            })
        }
        "folder" => {
            let path = ctx.path.as_ref()?;
            let dir = path.parent()?;
            Some(Launch::ShowItems {
                uri: file_uri(path),
                fallback: vec!["xdg-open".into(), dir.as_os_str().to_owned()],
            })
        }
        "edit" => Some(match &ctx.path {
            Some(path) => Launch::Argv {
                prog: ctx.editor.clone(),
                args: vec![path.as_os_str().to_owned()],
                png_on_stdin: false,
            },
            None => Launch::Argv {
                prog: ctx.editor.clone(),
                args: vec!["-".into()],
                png_on_stdin: true,
            },
        }),
        "copy" => Some(Launch::Argv {
            prog: ctx.exe.clone(),
            args: vec!["--copy-png".into()],
            png_on_stdin: true,
        }),
        _ => None,
    }
}

/// `file:///a%20b/c.png`: unreserved characters and `/` as they are, every
/// other byte as `%XX`.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Starts the action and returns at once; the program keeps running on its
/// own (a thread reaps it). `png` is what goes to its standard input.
pub fn run(launch: Launch, png: Option<&[u8]>, conn: Option<&Connection>) {
    match launch {
        Launch::Argv {
            prog,
            args,
            png_on_stdin,
        } => spawn(&prog, &args, png_on_stdin.then_some(png).flatten()),
        Launch::ShowItems { uri, fallback } => {
            let shown = conn.is_some_and(|c| {
                c.call_method(
                    Some("org.freedesktop.FileManager1"),
                    "/org/freedesktop/FileManager1",
                    Some("org.freedesktop.FileManager1"),
                    "ShowItems",
                    &(vec![uri.as_str()], ""),
                )
                .is_ok()
            });
            if !shown && let Some((prog, args)) = fallback.split_first() {
                spawn(Path::new(prog), args, None);
            }
        }
    }
}

fn spawn(prog: &Path, args: &[OsString], stdin: Option<&[u8]>) {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: it must outlive us and not get our signals.
        .process_group(0);
    let Ok(mut child) = cmd.spawn() else { return };
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(bytes);
    }
    // Reaped where nobody waits for it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(path: Option<&str>) -> Ctx {
        Ctx {
            exe: "/usr/bin/telamon-screenshot".into(),
            editor: "/usr/bin/telamon-screenshot-editor".into(),
            path: path.map(PathBuf::from),
        }
    }

    fn argv(l: Option<Launch>) -> (PathBuf, Vec<OsString>, bool) {
        match l.expect("an action") {
            Launch::Argv {
                prog,
                args,
                png_on_stdin,
            } => (prog, args, png_on_stdin),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn open_runs_xdg_open_with_one_argument() {
        // A hostile file name is one argument, not a command.
        let p = "/home/z/Pictures/Screenshots/a; rm -rf ~ $(x) `y`.png";
        let (prog, args, stdin) = argv(plan("open", &ctx(Some(p))));
        assert_eq!(prog, PathBuf::from("xdg-open"));
        assert_eq!(args, vec![OsString::from(p)]);
        assert!(!stdin);
        // Clicking the notification itself opens the file too.
        assert_eq!(plan(DEFAULT, &ctx(Some(p))), plan("open", &ctx(Some(p))));
    }

    #[test]
    fn folder_shows_the_file_and_falls_back_to_its_folder() {
        let c = ctx(Some("/home/z/Pictures/Screenshots/a b.png"));
        match plan("folder", &c).unwrap() {
            Launch::ShowItems { uri, fallback } => {
                assert_eq!(uri, "file:///home/z/Pictures/Screenshots/a%20b.png");
                assert_eq!(
                    fallback,
                    vec![
                        OsString::from("xdg-open"),
                        OsString::from("/home/z/Pictures/Screenshots")
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn edit_opens_the_file_or_gets_the_picture_on_stdin() {
        let (prog, args, stdin) = argv(plan("edit", &ctx(Some("/p/a.png"))));
        assert_eq!(prog, PathBuf::from("/usr/bin/telamon-screenshot-editor"));
        assert_eq!(args, vec![OsString::from("/p/a.png")]);
        assert!(!stdin);
        let (_, args, stdin) = argv(plan("edit", &ctx(None)));
        assert_eq!(args, vec![OsString::from("-")]);
        assert!(stdin);
    }

    #[test]
    fn copy_runs_this_program_with_the_picture_on_stdin() {
        for path in [None, Some("/p/a.png")] {
            let (prog, args, stdin) = argv(plan("copy", &ctx(path)));
            assert_eq!(prog, PathBuf::from("/usr/bin/telamon-screenshot"));
            assert_eq!(args, vec![OsString::from("--copy-png")]);
            assert!(stdin);
        }
    }

    #[test]
    fn no_file_no_file_actions_and_unknown_keys_do_nothing() {
        assert_eq!(plan("open", &ctx(None)), None);
        assert_eq!(plan("folder", &ctx(None)), None);
        assert_eq!(plan(DEFAULT, &ctx(None)), None);
        for key in ["", "OPEN", "rm", "open ", "../open", "copy; id"] {
            assert_eq!(plan(key, &ctx(Some("/p/a.png"))), None, "{key:?}");
        }
    }

    #[test]
    fn uris_escape_everything_odd() {
        assert_eq!(file_uri(Path::new("/a/b.png")), "file:///a/b.png");
        assert_eq!(
            file_uri(Path::new("/a b/ü#?%.png")),
            "file:///a%20b/%C3%BC%23%3F%25.png"
        );
        assert_eq!(file_uri(Path::new("/a\nb")), "file:///a%0Ab");
    }

    #[test]
    fn editor_is_found_next_to_the_program_else_in_usr_bin() {
        let dir =
            std::env::temp_dir().join(format!("telamon-screenshot-ed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("telamon-screenshot");
        assert_eq!(
            editor_path(&exe),
            PathBuf::from("/usr/bin/telamon-screenshot-editor")
        );
        std::fs::write(dir.join(EDITOR_NAME), b"").unwrap();
        assert_eq!(editor_path(&exe), dir.join(EDITOR_NAME));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
