//! `~/.config/telamon-screenshot/config.toml`.
//!
//! The file is untrusted input: it is size-capped, unknown keys are refused,
//! and every value is range-checked. A missing file gives the defaults
//! silently; a bad one gives the defaults plus one plain-words warning.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::legacy;

/// A config file this large is not a config file.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub capture: CaptureConfig,
    pub overlay: OverlayConfig,
    pub redact: RedactConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Draw the pointer into the frozen frame.
    pub include_cursor: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayConfig {
    /// How dark the area outside the selection gets, 0.0 to 0.9.
    pub dim: f32,
    /// Selection border, `#RRGGBB`. Telamon.Ui's dark accent by default.
    pub accent: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedactConfig {
    pub emails: bool,
    pub ipv4: bool,
    pub ipv6: bool,
    pub mac: bool,
    /// Colour of the boxes drawn over matches, `#RRGGBB`.
    pub fill: String,
    /// Extra pixels around each box, 0 to 32.
    pub padding: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Also save each image capture as a PNG here. Unset: never write to disk.
    pub save_dir: Option<PathBuf>,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            dim: 0.45,
            accent: "#8A7AF4".into(),
        }
    }
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            emails: true,
            ipv4: true,
            ipv6: true,
            mac: true,
            fill: "#000000".into(),
            padding: 2,
        }
    }
}

impl Config {
    /// Colours after validation; `validate` has already rejected bad ones.
    pub fn accent_rgb(&self) -> [u8; 3] {
        parse_hex_color(&self.overlay.accent).unwrap_or([0x8A, 0x7A, 0xF4])
    }

    pub fn fill_rgb(&self) -> [u8; 3] {
        parse_hex_color(&self.redact.fill).unwrap_or([0, 0, 0])
    }

    fn validate(&self) -> Result<(), String> {
        if !(0.0..=0.9).contains(&self.overlay.dim) {
            return Err(format!(
                "overlay.dim must be between 0.0 and 0.9, not {}",
                self.overlay.dim
            ));
        }
        if parse_hex_color(&self.overlay.accent).is_none() {
            return Err(format!(
                "overlay.accent must look like #8A7AF4, not {:?}",
                self.overlay.accent
            ));
        }
        if parse_hex_color(&self.redact.fill).is_none() {
            return Err(format!(
                "redact.fill must look like #000000, not {:?}",
                self.redact.fill
            ));
        }
        if self.redact.padding > 32 {
            return Err(format!(
                "redact.padding must be 0 to 32, not {}",
                self.redact.padding
            ));
        }
        if let Some(dir) = &self.output.save_dir
            && !dir.is_absolute()
        {
            return Err(format!(
                "output.save_dir must be an absolute path, not {dir:?}"
            ));
        }
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn path_in(base: &Path, folder: &str) -> PathBuf {
    base.join(folder).join("config.toml")
}

/// Loads `$XDG_CONFIG_HOME/telamon-screenshot/config.toml` (else under
/// `~/.config`). Never fails: problems come back as the warning to print.
/// A folder left by `atlasos-screenshot` (before 0.2.0) is moved to the new
/// name first, once; if it can't be moved (a read-only home), its file is
/// read where it is.
pub fn load() -> (Config, Option<String>) {
    let Some(base) = config_base() else {
        return (Config::default(), None);
    };
    load_migrating(&base)
}

fn load_migrating(base: &Path) -> (Config, Option<String>) {
    let new = path_in(base, legacy::NAME);
    let moved = legacy::move_once(base, false);
    if moved.is_err() && new.symlink_metadata().is_err() {
        let old = path_in(base, legacy::OLD_NAME);
        if old.symlink_metadata().is_ok() {
            return load_from(&old);
        }
    }
    load_from(&new)
}

pub fn load_from(path: &Path) -> (Config, Option<String>) {
    match read_capped(path) {
        Ok(None) => (Config::default(), None),
        Ok(Some(text)) => match parse(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => (
                Config::default(),
                Some(format!("{}: {e}; using the defaults", path.display())),
            ),
        },
        Err(e) => (
            Config::default(),
            Some(format!(
                "can't read {}: {e}; using the defaults",
                path.display()
            )),
        ),
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let cfg: Config = toml::from_str(text).map_err(|e| e.message().to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

/// `Ok(None)` when there is no file. Reads at most `MAX_CONFIG_BYTES`, and
/// only from a regular file (not a FIFO that would block us forever).
fn read_capped(path: &Path) -> std::io::Result<Option<String>> {
    // Non-blocking, so a FIFO in place of the file can't hang the open.
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("it is not a regular file"));
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(std::io::Error::other("it is larger than 64 KiB"));
    }
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES).read_to_string(&mut text)?;
    Ok(Some(text))
}

pub fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = parse("[redact]\nipv6 = false\n").unwrap();
        assert!(!cfg.redact.ipv6);
        assert!(cfg.redact.emails);
        assert_eq!(cfg.overlay, OverlayConfig::default());
    }

    #[test]
    fn rejects_unknown_keys_bad_ranges_and_colours() {
        assert!(parse("[overlay]\ndimm = 0.3\n").is_err());
        assert!(parse("[overlay]\ndim = 1.5\n").is_err());
        assert!(parse("[overlay]\ndim = nan\n").is_err());
        assert!(parse("[overlay]\naccent = \"red\"\n").is_err());
        assert!(parse("[redact]\nfill = \"#12345\"\n").is_err());
        assert!(parse("[redact]\npadding = 1000\n").is_err());
        assert!(parse("[ocr]\nmodel_dir = \"/x\"\n").is_err());
        assert!(parse("[output]\nsave_dir = \"Pictures\"\n").is_err());
        assert!(parse("[capture]\ninclude_cursor = \"yes\"\n").is_err());
        assert!(parse("not toml at all [").is_err());
    }

    #[test]
    fn config_of_the_old_name_moves_once_and_is_read() {
        let base =
            std::env::temp_dir().join(format!("telamon-screenshot-cfgmv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join(legacy::OLD_NAME);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "[overlay]\naccent = \"#123456\"\n").unwrap();

        let (cfg, warn) = load_migrating(&base);
        assert!(warn.is_none(), "{warn:?}");
        assert_eq!(cfg.overlay.accent, "#123456");
        assert!(!old.exists(), "the old folder is gone");
        assert!(path_in(&base, legacy::NAME).is_file());
        // Once: a later run reads the new place, and a recreated old folder is left alone.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "[overlay]\naccent = \"#654321\"\n").unwrap();
        let (cfg, _) = load_migrating(&base);
        assert_eq!(cfg.overlay.accent, "#123456");
        assert!(old.join("config.toml").is_file());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_file_is_silent_defaults() {
        let (cfg, warn) = load_from(Path::new("/nonexistent/telamon-screenshot/config.toml"));
        assert_eq!(cfg, Config::default());
        assert!(warn.is_none());
    }

    #[test]
    fn unreadable_or_bad_file_warns_once() {
        let dir =
            std::env::temp_dir().join(format!("telamon-screenshot-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the file should be.
        let (cfg, warn) = load_from(&dir);
        assert_eq!(cfg, Config::default());
        assert!(warn.unwrap().contains("not a regular file"));

        let bad = dir.join("config.toml");
        std::fs::write(&bad, "[overlay]\ndim = 3.0\n").unwrap();
        let (cfg, warn) = load_from(&bad);
        assert_eq!(cfg, Config::default());
        assert!(warn.unwrap().contains("overlay.dim"));

        std::fs::write(&bad, vec![b'#'; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        assert!(load_from(&bad).1.unwrap().contains("64 KiB"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex_color("#8A7AF4"), Some([0x8A, 0x7A, 0xF4]));
        assert_eq!(parse_hex_color("8A7AF4"), None);
        assert_eq!(parse_hex_color("#8A7AFZ"), None);
        assert_eq!(parse_hex_color("#+A7AF4"), None);
    }
}
