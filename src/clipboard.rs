//! Putting the result on the Wayland clipboard.
//!
//! On Wayland the copying client must stay alive to hand the data to whoever
//! pastes. Like `wl-copy`, we prepare the copy in the CLI (so errors still
//! reach the caller), then fork: the CLI exits, and the child serves pastes
//! until another client takes the clipboard (Klipper usually does at once),
//! then exits. The child is detached (own session, stdio on /dev/null), so it
//! never holds a terminal or a `$(...)` capture open.

use wl_clipboard_rs::copy::{MimeType, Options, Source};

pub enum Payload {
    Png(Vec<u8>),
    Text(String),
}

/// Must be called with no other Wayland connection, D-Bus connection or
/// worker thread in use: the forked child inherits none of the threads and
/// all of the file descriptors (it closes the ones it didn't open).
pub fn copy(payload: Payload) -> Result<(), String> {
    let (source, mime) = match payload {
        Payload::Png(bytes) => (
            Source::Bytes(bytes.into_boxed_slice()),
            MimeType::Specific("image/png".into()),
        ),
        Payload::Text(text) => (
            Source::Bytes(text.into_bytes().into_boxed_slice()),
            MimeType::Text,
        ),
    };
    // Everything open now is the caller's; the child keeps only what the
    // clipboard opens next (its Wayland socket) and the status pipe.
    let inherited = open_fds();
    let mut opts = Options::new();
    opts.foreground(true);
    let prepared = opts
        .prepare_copy(source, mime)
        .map_err(|e| format!("can't use the clipboard: {e}"))?;
    let (status_r, status_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
        .map_err(|e| format!("can't start the clipboard server: {e}"))?;

    // SAFETY: the child only runs the clipboard loop on state it owns (the
    // prepared copy has its own connection and no threads), then `_exit`s.
    match unsafe { libc::fork() } {
        -1 => Err(format!(
            "can't start the clipboard server: {}",
            std::io::Error::last_os_error()
        )),
        0 => {
            // A panic here must not reach the parent's hook (D-Bus from a
            // forked process).
            std::panic::set_hook(Box::new(|_| {}));
            detach(&inherited);
            drop(status_r);
            let code = match prepared.serve() {
                Ok(()) => 0,
                Err(e) => {
                    let msg = e.to_string();
                    let _ = rustix::io::write(&status_w, &msg.as_bytes()[..msg.len().min(512)]);
                    1
                }
            };
            // SAFETY: ends the child without running the parent's atexit
            // handlers or flushing its stdio buffers a second time.
            unsafe { libc::_exit(code) }
        }
        pid => {
            // The child owns the connection now. Dropping our copy is
            // harmless (it only closes our fd), but forgetting it guarantees
            // we never flush the requests the child is about to send.
            std::mem::forget(prepared);
            drop(status_w);
            check_child(pid, status_r)
        }
    }
}

/// Setting the selection fails, if at all, on the child's first roundtrip.
/// Wait briefly (100 ms; the parent only, the clipboard is live already) for that: an early exit with a message is an error the
/// user must see; silence (still serving) or a clean exit is success.
fn check_child(pid: libc::pid_t, status: rustix::fd::OwnedFd) -> Result<(), String> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    let mut fds = [PollFd::new(&status, PollFlags::IN)];
    if !matches!(poll(&mut fds, Some(&ts)), Ok(n) if n > 0) {
        return Ok(());
    }
    let mut buf = [0u8; 512];
    let n = rustix::io::read(&status, &mut buf).unwrap_or(0);
    let mut wstatus = 0;
    // SAFETY: reaps our own child; it has closed the pipe, so it is exiting.
    let reaped = unsafe { libc::waitpid(pid, &mut wstatus, 0) } == pid;
    if n == 0 {
        // A clean exit is fine (something took the clipboard already).
        let clean = !reaped || (libc::WIFEXITED(wstatus) && libc::WEXITSTATUS(wstatus) == 0);
        return if clean {
            Ok(())
        } else {
            Err("the clipboard server stopped; nothing was copied".into())
        };
    }
    Err(format!(
        "the clipboard didn't take the copy: {}",
        String::from_utf8_lossy(&buf[..n])
    ))
}

/// Open descriptors above stderr, found by probing (no allocation or
/// directory fd that could be confused with a later one).
pub fn open_fds() -> Vec<libc::c_int> {
    let max = rustix::process::getrlimit(rustix::process::Resource::Nofile)
        .current
        .unwrap_or(65536)
        .min(65536) as libc::c_int;
    // SAFETY: F_GETFD only reads the descriptor flags.
    (3..max)
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1)
        .collect()
}

/// New session, stdio to /dev/null, cwd to /, and the caller's other
/// descriptors closed, so the server holds nothing of the caller's open.
pub fn detach(inherited: &[libc::c_int]) {
    // SAFETY: plain syscalls on descriptors the child doesn't use, in a
    // single-threaded child.
    unsafe {
        for &fd in inherited {
            libc::close(fd);
        }
        libc::setsid();
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
        if null >= 0 {
            for fd in 0..3 {
                libc::dup2(null, fd);
            }
            if null > 2 {
                libc::close(null);
            }
        }
        libc::chdir(c"/".as_ptr());
    }
}
