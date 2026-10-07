//! Saving a capture: where (`Pictures/Screenshots`, or `output.save_dir`),
//! under what name, and the safe way to write it.
//!
//! The folder is refused when other users can write to it. The file is written
//! unnamed (`O_TMPFILE`, 0600) and linked in under its final name with
//! `linkat`, which never replaces a file; without `O_TMPFILE` (or /proc) a
//! random temp name is renamed with `RENAME_NOREPLACE`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;

/// The folder a capture is saved in: `output.save_dir`, else
/// `<Pictures>/Screenshots`. `None` without a home folder.
pub fn save_dir(cfg: &Config) -> Option<PathBuf> {
    if let Some(dir) = &cfg.output.save_dir {
        return Some(dir.clone());
    }
    Some(pictures_dir()?.join("Screenshots"))
}

/// What `xdg-user-dir PICTURES` says, without running a script: the
/// `XDG_PICTURES_DIR` line of `~/.config/user-dirs.dirs`, else the
/// environment, else `~/Pictures`.
pub fn pictures_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let from_file = read_small(&config.join("user-dirs.dirs"))
        .and_then(|text| user_dir_from(&text, "PICTURES", &home));
    let from_env = || {
        std::env::var_os("XDG_PICTURES_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    Some(
        from_file
            .or_else(from_env)
            .filter(|p| *p != home)
            .unwrap_or_else(|| home.join("Pictures")),
    )
}

fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(64 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The value of `XDG_<name>_DIR="..."` in a `user-dirs.dirs` file; `$HOME`
/// at its start is the home folder. Relative or odd values are ignored.
fn user_dir_from(text: &str, name: &str, home: &Path) -> Option<PathBuf> {
    let key = format!("XDG_{name}_DIR=");
    let value = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(&key))?;
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    if value.contains(['\0', '\\']) {
        return None;
    }
    let path = match value.strip_prefix("$HOME") {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(&rest[1..]),
        Some(_) => return None,
        None => PathBuf::from(value),
    };
    (path.is_absolute() && !path.components().any(|c| c.as_os_str() == "..")).then_some(path)
}

/// Makes the folder (and its parents) when it is missing: only the default
/// folder; a configured `save_dir` that isn't there is the user's mistake to
/// hear about.
pub fn ensure_default_dir(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    if cfg.output.save_dir.is_some() || dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new().recursive(true).create(dir)
}

/// `Screenshot_YYYYMMDD_HHMMSS.png` (local time, like Spectacle) in `dir`,
/// never replacing a file that's there. The file is made unnamed (`O_TMPFILE`) and linked in under its
/// final name, so no half-written or temp file is ever visible. Where that
/// can't work (no `O_TMPFILE`, no /proc), a random temp name is renamed.
pub fn save_png(dir: &Path, png: &[u8]) -> Result<PathBuf, String> {
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

/// `20261005_174012` (UTC), from the civil-from-days algorithm.
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
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_250_812);
        assert_eq!(utc_stamp(t), "20261006_014012");
        assert_eq!(utc_stamp(std::time::UNIX_EPOCH), "19700101_000000");
    }

    #[test]
    fn names_follow_spectacle() {
        let name = format!("Screenshot_{}.png", utc_stamp(std::time::UNIX_EPOCH));
        assert_eq!(name, "Screenshot_19700101_000000.png");
    }

    #[test]
    fn user_dirs_file() {
        let home = Path::new("/home/z");
        let file =
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n";
        assert_eq!(
            user_dir_from(file, "PICTURES", home),
            Some(PathBuf::from("/home/z/Bilder"))
        );
        assert_eq!(user_dir_from(file, "VIDEOS", home), None);
        let abs = "XDG_PICTURES_DIR=\"/data/pics\"\n";
        assert_eq!(
            user_dir_from(abs, "PICTURES", home),
            Some(PathBuf::from("/data/pics"))
        );
        // Odd values are ignored.
        for bad in [
            "XDG_PICTURES_DIR=\"pics\"",
            "XDG_PICTURES_DIR=\"$HOMEX/pics\"",
            "XDG_PICTURES_DIR=\"$HOME/../etc\"",
            "XDG_PICTURES_DIR=$HOME/pics",
            "#XDG_PICTURES_DIR=\"$HOME/x\"",
        ] {
            assert_eq!(user_dir_from(bad, "PICTURES", home), None, "{bad}");
        }
        assert_eq!(
            user_dir_from("XDG_PICTURES_DIR=\"$HOME\"", "PICTURES", home),
            Some(PathBuf::from("/home/z"))
        );
    }

    #[test]
    fn configured_dir_wins_and_default_is_screenshots() {
        let mut cfg = Config::default();
        cfg.output.save_dir = Some(PathBuf::from("/srv/shots"));
        assert_eq!(save_dir(&cfg), Some(PathBuf::from("/srv/shots")));
        cfg.output.save_dir = None;
        if let Some(d) = save_dir(&cfg) {
            assert_eq!(d.file_name().and_then(|n| n.to_str()), Some("Screenshots"));
        }
    }

    #[test]
    fn save_never_overwrites_and_refuses_open_folders() {
        let png = [0x89, b'P', b'N', b'G', 1, 2, 3];
        let dir =
            std::env::temp_dir().join(format!("telamon-screenshot-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = save_png(&dir, &png).unwrap();
        let b = save_png(&dir, &png).unwrap();
        assert_ne!(a, b);
        assert!(
            a.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("Screenshot_")
        );
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
        // A missing folder is an error, not a created one.
        assert!(save_png(&dir.join("nope"), &png).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
