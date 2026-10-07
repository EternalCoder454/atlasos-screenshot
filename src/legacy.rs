//! The names before the rename to Telamon (`atlasos-screenshot`).
//!
//! Until 0.2.0 the app's config and OCR models lived in folders named
//! `atlasos-screenshot`. The first run of the renamed app moves such a folder
//! to its new name, once: one `renameat2(RENAME_NOREPLACE)`, which is atomic
//! and never replaces anything, so a folder that exists under the new name
//! wins and the old one is left alone (a downgrade and a second upgrade
//! can't lose either). Nothing is copied and nothing is read from the old
//! name afterwards.

use std::path::Path;

use rustix::fs::{CWD, RenameFlags, renameat_with};

/// The old name of every file and folder the app owns.
pub const OLD_NAME: &str = "atlasos-screenshot";
/// The new one.
pub const NAME: &str = "telamon-screenshot";

/// Moves `base/OLD_NAME` to `base/NAME`. `Ok(true)` when it moved, `Ok(false)`
/// when there was nothing to do (no old folder, or the new one exists).
/// `only_real_dir` refuses an old name that is a symlink or a file: the models
/// folder is checked to be a plain folder of ours and a link would be refused
/// there anyway, so it stays where it is.
pub fn move_once(base: &Path, only_real_dir: bool) -> std::io::Result<bool> {
    let old = base.join(OLD_NAME);
    let meta = match std::fs::symlink_metadata(&old) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if only_real_dir && !meta.is_dir() {
        return Ok(false);
    }
    match renameat_with(CWD, &old, CWD, base.join(NAME), RenameFlags::NOREPLACE) {
        Ok(()) => Ok(true),
        // The new name exists already (a folder, or an empty one): leave both.
        Err(rustix::io::Errno::EXIST | rustix::io::Errno::NOTEMPTY) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "telamon-screenshot-legacy-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn moves_the_folder_with_its_files_once() {
        let base = tmpdir("move");
        fs::create_dir_all(base.join(OLD_NAME).join("models")).unwrap();
        fs::write(
            base.join(OLD_NAME).join("config.toml"),
            "[overlay]\ndim = 0.5\n",
        )
        .unwrap();
        fs::write(base.join(OLD_NAME).join("models/a.rten"), "model").unwrap();

        assert!(move_once(&base, true).unwrap());
        assert!(!base.join(OLD_NAME).exists());
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("config.toml")).unwrap(),
            "[overlay]\ndim = 0.5\n"
        );
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("models/a.rten")).unwrap(),
            "model"
        );
        // The second run has nothing to move.
        assert!(!move_once(&base, true).unwrap());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn nothing_to_move_is_not_an_error() {
        let base = tmpdir("none");
        assert!(!move_once(&base, false).unwrap());
        assert!(!move_once(&base.join("missing"), false).unwrap());
        assert!(!base.join(NAME).exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_new_folder_wins_and_the_old_one_is_kept() {
        let base = tmpdir("both");
        fs::create_dir_all(base.join(OLD_NAME)).unwrap();
        fs::write(base.join(OLD_NAME).join("config.toml"), "old").unwrap();
        fs::create_dir_all(base.join(NAME)).unwrap();
        fs::write(base.join(NAME).join("config.toml"), "new").unwrap();

        assert!(!move_once(&base, true).unwrap());
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("config.toml")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(base.join(OLD_NAME).join("config.toml")).unwrap(),
            "old"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_link_moves_only_when_allowed() {
        let base = tmpdir("link");
        let target = tmpdir("link-target");
        symlink(&target, base.join(OLD_NAME)).unwrap();
        assert!(!move_once(&base, true).unwrap());
        assert!(base.join(OLD_NAME).symlink_metadata().is_ok());
        // A config folder that is a link (dotfiles managers) moves as a link.
        assert!(move_once(&base, false).unwrap());
        assert!(
            base.join(NAME)
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(&base).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }
}
