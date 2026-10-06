//! The OCR models, downloaded on the first Ctrl or Alt drag.
//!
//! This is the one place the tool writes to disk without being configured to
//! (approved for AtlasOS): `$XDG_DATA_HOME/atlasos-screenshot/models`.
//! - Fixed HTTPS URLs, no redirects, no plain HTTP; nothing comes from config.
//! - Timeouts (10 s connect, 60 s per file), a size cap, one retry.
//! - The folder is 0700 and must be ours; files go to a temp name, are
//!   checked against the pinned SHA-256, then renamed into place.
//! - Every load re-checks the hash on the bytes that are then used, so a
//!   damaged file is downloaded again and a swapped one is never run.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct ModelSpec {
    pub file: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Upstream: https://github.com/robertknight/ocrs-models (trained on
/// HierText, CC-BY-SA 4.0). The pins are those of the 2024-01-01 release.
pub const BASE_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/";
pub const DETECTION: ModelSpec = ModelSpec {
    file: "text-detection.rten",
    sha256: "f15cfb56bd02c4bf478a20343986504a1f01e1665c2b3a0ad66340f054b1b5ca",
    size: 2_510_284,
};
pub const RECOGNITION: ModelSpec = ModelSpec {
    file: "text-recognition.rten",
    sha256: "e484866d4cce403175bd8d00b128feb08ab42e208de30e42cd9889d8f1735a6e",
    size: 9_716_568,
};

pub const DOWNLOADING: &str = "Downloading text recognition models (12 MB)…";

/// The verified model bytes.
pub struct Models {
    pub detection: Vec<u8>,
    pub recognition: Vec<u8>,
}

/// `$XDG_DATA_HOME/atlasos-screenshot/models`, else `~/.local/share/...`.
pub fn dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(base.join("atlasos-screenshot").join("models"))
}

/// The models, downloading whichever is missing or damaged. `on_download` is
/// called once, before the first download starts.
pub fn ensure(on_download: &dyn Fn()) -> Result<Models, String> {
    let dir = dir()?;
    let mut have = [
        read_verified(&dir, &DETECTION)?,
        read_verified(&dir, &RECOGNITION)?,
    ];
    if have.iter().any(Option::is_none) {
        on_download();
        prepare_dir(&dir)?;
        for (slot, spec) in have.iter_mut().zip([&DETECTION, &RECOGNITION]) {
            if slot.is_none() {
                let bytes = download_with_retry(spec)?;
                store(&dir, spec, &bytes)?;
                *slot = Some(bytes);
            }
        }
    }
    let [Some(detection), Some(recognition)] = have else {
        unreachable!()
    };
    Ok(Models {
        detection,
        recognition,
    })
}

/// `Ok(None)` when the file is missing or fails its check (it is then
/// replaced). `Err` only for a folder we must not touch.
fn read_verified(dir: &Path, spec: &ModelSpec) -> Result<Option<Vec<u8>>, String> {
    if !dir.exists() {
        return Ok(None);
    }
    check_dir(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(spec.file));
    let Ok(file) = file else { return Ok(None) };
    let mut bytes = Vec::with_capacity(spec.size as usize);
    if file.take(spec.size + 1).read_to_end(&mut bytes).is_err() {
        return Ok(None);
    }
    Ok(verify(spec, &bytes).then_some(bytes))
}

fn verify(spec: &ModelSpec, bytes: &[u8]) -> bool {
    bytes.len() as u64 == spec.size && sha256_hex(bytes) == spec.sha256
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, bytes);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Creates the folder 0700 (parents too, as XDG asks), then checks it.
fn prepare_dir(dir: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    check_dir(dir)
}

/// The folder must be a real directory (not a symlink), owned by us, and
/// writable by nobody else; otherwise another user could swap the models.
fn check_dir(dir: &Path) -> Result<(), String> {
    for d in [dir, dir.parent().unwrap_or(dir)] {
        let meta =
            fs::symlink_metadata(d).map_err(|e| format!("can't check {}: {e}", d.display()))?;
        let unsafe_dir = !meta.file_type().is_dir()
            || meta.uid() != rustix::process::getuid().as_raw()
            || meta.permissions().mode() & 0o022 != 0;
        if unsafe_dir {
            return Err(format!(
                "{} isn't a private folder of yours, so the OCR models won't be kept there",
                d.display()
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
enum FetchError {
    /// Worth one more try.
    Transient(String),
    /// Retrying won't help.
    Fatal(String),
}

fn download_with_retry(spec: &ModelSpec) -> Result<Vec<u8>, String> {
    match download(spec) {
        Ok(b) => Ok(b),
        Err(FetchError::Fatal(e)) => Err(e),
        Err(FetchError::Transient(_)) => {
            std::thread::sleep(Duration::from_secs(3));
            download(spec).map_err(|e| match e {
                FetchError::Transient(e) | FetchError::Fatal(e) => e,
            })
        }
    }
}

fn download(spec: &ModelSpec) -> Result<Vec<u8>, FetchError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        // The one fixed host, directly: no proxy from the environment.
        .proxy(None)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("atlasos-screenshot/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let url = format!("{BASE_URL}{}", spec.file);
    let mut resp = agent.get(&url).call().map_err(plain)?;
    // A little over the real size: a longer body is the wrong file anyway,
    // and the cap stops a hostile server from filling memory.
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(spec.size + 64 * 1024)
        .read_to_vec()
        .map_err(plain)?;
    if !verify(spec, &bytes) {
        // TLS rules out damage in transit: a full-size file with the wrong
        // hash means upstream replaced it, and retrying won't help.
        return Err(if bytes.len() as u64 == spec.size {
            FetchError::Fatal(
                "the model on the server has changed; atlasos-screenshot needs an update".into(),
            )
        } else {
            FetchError::Transient("the download was incomplete".into())
        });
    }
    Ok(bytes)
}

/// No route, no network or no name lookup (getaddrinfo's failure has no
/// error kind of its own, only its message).
fn offline(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::NetworkUnreachable | K::NetworkDown | K::HostUnreachable | K::AddrNotAvailable
    ) || e.to_string().contains("lookup address")
}

/// ureq's errors in plain words.
fn plain(e: ureq::Error) -> FetchError {
    use ureq::Error as E;
    match e {
        E::HostNotFound | E::ConnectionFailed => {
            FetchError::Transient("no internet connection".into())
        }
        E::Timeout(_) => FetchError::Transient("the download timed out".into()),
        E::Io(e) if offline(&e) => FetchError::Transient("no internet connection".into()),
        E::Io(e) => FetchError::Transient(format!("the connection failed ({e})")),
        E::StatusCode(code) if code >= 500 || code == 429 => {
            FetchError::Transient(format!("the server is having trouble (HTTP {code})"))
        }
        E::StatusCode(code) => {
            FetchError::Fatal(format!("the server refused the download (HTTP {code})"))
        }
        E::BodyExceedsLimit(_) => {
            FetchError::Fatal("the server sent a file larger than expected".into())
        }
        E::Tls(_) | E::Rustls(_) => {
            FetchError::Fatal("the server's secure connection couldn't be verified".into())
        }
        e => FetchError::Transient(format!("the download failed ({e})")),
    }
}

/// Writes `bytes` (already verified) to a temp file in `dir`, syncs it, and
/// renames it into place, so a crash never leaves a half file under the real
/// name.
fn store(dir: &Path, spec: &ModelSpec, bytes: &[u8]) -> Result<(), String> {
    remove_stale_temps(dir, spec);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{}.{}.{nanos}.tmp", spec.file, std::process::id()));
    let fail = |what: &str, e: std::io::Error| {
        format!(
            "can't save the OCR models in {} ({what}: {e})",
            dir.display()
        )
    };
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|e| fail("create", e))?;
        f.write_all(bytes).map_err(|e| fail("write", e))?;
        f.sync_all().map_err(|e| fail("sync", e))?;
        fs::rename(&tmp, dir.join(spec.file)).map_err(|e| fail("rename", e))?;
        // Best effort: the model is in place either way; this only makes
        // the rename itself survive a crash.
        let _ = File::open(dir).and_then(|d| d.sync_all());
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Temp files left by a run that crashed mid-download (older than 10 min,
/// so a download running in parallel keeps its own).
fn remove_stale_temps(dir: &Path, spec: &ModelSpec) {
    let prefix = format!(".{}.", spec.file);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(&prefix) && name.ends_with(".tmp")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(600));
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_errors_read_as_no_internet() {
        use std::io::{Error, ErrorKind};
        assert!(offline(&Error::other(
            "failed to lookup address information: Temporary failure in name resolution"
        )));
        assert!(offline(&Error::from(ErrorKind::NetworkUnreachable)));
        assert!(!offline(&Error::from(ErrorKind::ConnectionReset)));
    }

    const TINY: ModelSpec = ModelSpec {
        file: "tiny.rten",
        // sha256("abc")
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        size: 3,
    };

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "atlasos-screenshot-models-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn hash_and_size_checks() {
        assert!(verify(&TINY, b"abc"));
        assert!(!verify(&TINY, b"abd"));
        assert!(!verify(&TINY, b"abcd"));
    }

    #[test]
    fn store_then_read_back_and_detect_damage() {
        let d = tmpdir("store").join("atlasos-screenshot").join("models");
        prepare_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(read_verified(&d, &TINY).unwrap().is_none());
        store(&d, &TINY, b"abc").unwrap();
        assert_eq!(read_verified(&d, &TINY).unwrap().unwrap(), b"abc");
        assert_eq!(
            fs::metadata(d.join("tiny.rten"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // No temp file left behind.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        // A damaged file reads as missing, so it gets downloaded again.
        fs::write(d.join("tiny.rten"), b"abX").unwrap();
        assert!(read_verified(&d, &TINY).unwrap().is_none());
        fs::remove_dir_all(d.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn refuses_shared_or_symlinked_folders() {
        let root = tmpdir("perm");
        let d = root.join("atlasos-screenshot").join("models");
        prepare_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            read_verified(&d, &TINY)
                .unwrap_err()
                .contains("private folder")
        );
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();

        let link = root.join("atlasos-screenshot").join("linked");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(check_dir(&link).is_err());

        // A symlink in place of the model file is not followed.
        fs::write(root.join("abc"), b"abc").unwrap();
        std::os::unix::fs::symlink(root.join("abc"), d.join("tiny.rten")).unwrap();
        assert!(read_verified(&d, &TINY).unwrap().is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stale_temps_are_cleaned_but_fresh_ones_kept() {
        let root = tmpdir("stale");
        let d = root.join("atlasos-screenshot").join("models");
        prepare_dir(&d).unwrap();
        let fresh = d.join(".tiny.rten.1.2.tmp");
        fs::write(&fresh, b"x").unwrap();
        remove_stale_temps(&d, &TINY);
        assert!(fresh.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pins_are_well_formed() {
        for s in [&DETECTION, &RECOGNITION] {
            assert_eq!(s.sha256.len(), 64);
            assert!(
                s.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
        assert!(BASE_URL.starts_with("https://"));
    }
}
