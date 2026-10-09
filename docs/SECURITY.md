# Telamon Screenshot: threat model

What the program protects, from whom, what it does about it, and what it
accepts. `DESIGN.md` has the data flow; this file has the reasoning behind
the safety rules in `CLAUDE.md`. The code and tests it refers to are named so
that a change to one shows up against the other.

To report a vulnerability, open a private security advisory on the GitHub
repository (Security, Report a vulnerability) rather than a public issue.

## What is being protected

1. **Screen contents.** A screenshot shows mail, chat, passwords being typed
   into a visible field, one-time codes, other users' data. Pixels, OCR text
   and redaction results are all sensitive. They live in four places: process
   memory, the clipboard, the saved PNG, and (as a 320 px thumbnail, only when
   nothing is saved) the notification.
2. **The capture right.** On Telamon OS the right to read the screen is not
   something every program has: KWin grants `org.kde.KWin.ScreenShot2` to
   the binaries named by an installed `.desktop` file with
   `X-KDE-DBUS-Restricted-Interfaces`. Whoever can run, or ask, the granted
   binary has the right.
3. **The user's files and session.** Saving, opening and launching must not
   write outside the chosen folder, replace what is there, or run anything
   the user did not choose.

## Who is who

| Actor | Trusted? | Notes |
|---|---|---|
| The user at the keyboard (hotkey, launcher, notification buttons) | yes | The only source of *consent*. |
| KWin, the compositor, the session bus daemon | yes, but their data is checked | Metadata (sizes, strides, formats) is range-checked before it sizes an allocation. |
| The notification server (`org.freedesktop.Notifications`) | partly | It receives the title, the body and a thumbnail; its signals are believed only from the connection that answered our `Notify`. |
| Other programs of the same user on the session bus | **no** | Any of them may call `org.kde.Spectacle`, emit look-alike signals, own a name first. Sandboxed (Flatpak) programs reach only what their bus filter allows. |
| Other programs of the same user, running the binary or in its memory | **out of scope** (see "Accepted") | They are already inside the user's trust boundary. |
| Other users of the machine | no | The session bus is per user; files are created 0600 and in folders others cannot write to. |
| Files the user opens in the editor, the config file, stdin of the helpers | **no** | Treated as hostile input: size-capped, parsed strictly, fuzzed. |
| The network (only the OCR model download) | no | Fixed HTTPS URLs, no redirects, pinned SHA-256. |

## Capture consent

A screenshot is made with the user's consent only when the user asked for it.
How each way in checks that:

| Way in | Gesture | What the user sees | Notes |
|---|---|---|---|
| Hotkey or launcher (`.desktop` actions) | Pressing it | The overlay (region, window picker) or nothing before the capture (whole desktop, screen, active window); then a notification | The user's own action. `output.notify` and `--no-notify` are the user's to set. |
| Command line | Running it | As above, as configured | A program that runs the binary is the user's own program (see "Accepted"). |
| `--delay N` | Starting it | A countdown badge that is never in the picture | Bounded to 600 s. |
| `org.kde.Spectacle` over D-Bus | **None**: any program on the bus | A one second countdown badge first (whole desktop, screen, active window) or the overlay (region, window picker); **always** a notification that names the calling program, whatever `output.notify` says | Requests come at most once every 3 seconds. See below. |
| Notification buttons | Clicking them | The result | The buttons are a fixed set. |

### What a caller of `org.kde.Spectacle` gets

The service (`src/dbus.rs`, started on demand by the compat package's D-Bus
service file, exits after 15 idle seconds) mirrors Spectacle's interface so
programs that ask Spectacle keep working. What a caller can and cannot do:

- **It cannot get pixels.** The reply to a method is empty. The result is
  the path of a file in the `ScreenshotTaken(path)` signal, a string
  from the program that took the picture, never from the caller. The picture
  is also on the clipboard. A caller therefore learns nothing about the screen
  unless it can read the user's Pictures folder (a normal program of the user
  can; a Flatpak app without that permission cannot) or the clipboard.
- **It cannot choose where or how.** The methods' arguments (`-1/0/1`
  integers) pick among fixed flags (`--cursor`, `--no-frame`, ...). No path,
  string, or number reaches the command line otherwise. The file name is
  made by the program (`Screenshot_YYYYMMDD_HHMMSS.png`), in the configured
  folder. `argv()` is tested to always parse and to never produce `--edit`,
  `--no-save` or a region without the overlay.
- **It cannot do it silently.** The child is started with `--notify` (a
  notification even when `output.notify = false`) and `--requested-by NAME`,
  where NAME is the `comm` of the process that owns the calling connection
  (asked from the bus daemon, `GetConnectionCredentials`, then `/proc`),
  defanged to letters, digits and a few marks. A Flatpak app is named "a
  sandboxed app" (its bus connection is its proxy's). The captures that need
  no gesture show the countdown badge for one second first. `Record*`
  answers `RecordingFailed`.
- **It cannot flood.** One capture at a time, and a new request is refused
  (`ScreenshotFailed`) within 3 seconds of the last accepted one. A refused
  request starts nothing and shows nothing.
- **It can still** make a capture happen, land a PNG in `Pictures/Screenshots`
  and replace the clipboard, with a visible notification. That is what
  Spectacle's own service allows and what KDE Connect and scripts rely on;
  removing the `telamon-screenshot-spectacle-compat` package removes the
  service.
- `ScreenshotTaken` is a broadcast signal, as Spectacle's is, so any program
  on the session bus can see the path. The path is a file name in the user's
  own folder; the content is not on the bus.

Not done, on purpose: asking the user to confirm each D-Bus request. Callers
(KDE Connect's remote screenshot, scripts) cannot answer a dialog, and a
dialog that the user clicks through out of habit protects less than a
notification that names the program.

## The capture right (ScreenShot2)

KWin identifies the caller by the real path of `/proc/<pid>/exe` and compares
it with the `Exec=` of a desktop file that asks for the interface. What follows:

- The grant is asked for by **one** desktop file, `net.eterneon.telamon.screenshot.desktop`
  (and its kglobalaccel copy), for **one** interface. The editor's desktop file
  asks for none: the editor parses untrusted images and has no capture right.
  The spec's `%check` fails the build if either changes.
- The binary is `/usr/bin/telamon-screenshot`, root-owned; `atlasos-screenshot`
  is a link to it (KWin resolves the real path). The helper modes of the same
  binary (`--post`, `--dbus`, `--save-png`, `--copy-png`) never call ScreenShot2
  themselves.
- The binary has **no mode that returns pixels to its caller**: stdout has the
  saved file's path, the clipboard server serves the clipboard, nothing writes
  the picture to a pipe or socket the caller names.
- `prctl(PR_SET_DUMPABLE, 0)` is deliberately **not** used. It would stop other
  processes of the user from reading this process's memory, but also makes
  `/proc/<pid>/exe` unreadable to them, and KWin reads exactly that to decide
  who may capture. Core dumps are switched off with `RLIMIT_CORE = 0` instead
  (`src/harden.rs`, `editor/src/harden.cpp`).

## Sensitive data in motion and at rest

| Where | Protection |
|---|---|
| Process memory | Single short-lived process; the pixels are dropped before the clipboard server and the notification helper are forked (they inherit nothing but what they serve). `RLIMIT_CORE = 0` in the CLI, the editor, and everything they start, so a crash writes no core to disk or to systemd-coredump. A crash shows a notification ("something went wrong"), not a dump. |
| Wayland buffers | Anonymous sealed-size `memfd`s (`MFD_CLOEXEC`), never named files. |
| Saved PNG | `Pictures/Screenshots` or `output.save_dir`; created `0600`; see "Saving". `output.save = false` / `--no-save` keeps nothing on disk. |
| Clipboard | The picture (or the OCR text) is put on the Wayland clipboard by a detached server that exits when another client takes the selection. Clipboard managers (Klipper) may keep a history: that is the user's setting, outside this program. |
| Notification | Title, escaped body, and either the file URI or (only if nothing was saved) a thumbnail of at most 320 px. Both go to whoever owns `org.freedesktop.Notifications`, which is the shell on Telamon OS. |
| Temp files | **None by design.** The saved file is written unnamed (`O_TMPFILE`) and linked in. Where `O_TMPFILE` is unavailable, a random-named 0600 sibling in the target folder is renamed with `RENAME_NOREPLACE` and removed if anything fails. Nothing is written to `/tmp`, and nothing is spooled to disk between the CLI, the notification helper and the editor: they talk over pipes. The only other files are the OCR models (0700 folder, 0600 temp, renamed) and the lock file in `$XDG_RUNTIME_DIR` (0600, `O_NOFOLLOW`). |
| OCR models | See "Network". |

## Saving (`src/store.rs`)

- The folder is the configured `output.save_dir` (absolute), else
  `<Pictures>/Screenshots` read from `user-dirs.dirs` without running a script
  (relative values, `..` and NUL are ignored). Only the default folder is
  created.
- The file name is made by the program from the time: `Screenshot_YYYYMMDD_HHMMSS.png`
  or `-1` ... `-99`. No part of it comes from a caller, the config, the image or
  the window title, so there is no traversal or injection through it. The
  stamp is clamped so the shape holds for any clock value.
- The file is opened through a directory descriptor (`openat`) and linked with
  `linkat`, which never replaces a file and does not follow a symlink at the
  target name: a file, symlink or dangling symlink already there is skipped
  and the next name is used. The fallback uses `O_EXCL | O_NOFOLLOW` and
  `RENAME_NOREPLACE`.
- A folder other users can write to (world-writable, or group-writable by a
  group other than the user's own) is refused unless it has the sticky bit.
- The editor's Save As (the user types the path) writes with `QSaveFile`
  (temp file next to the target, renamed over it), new files `0600`.

## Process launching (`src/actions.rs`, `src/post.rs`)

- No shell anywhere. The notification buttons run argument vectors:
  `/usr/bin/xdg-open <file>` (absolute path: `PATH` is not consulted), the
  editor next to the running binary or in `/usr/bin`, this program with
  `--copy-png`, and the file manager's `ShowItems` with a percent-escaped
  `file://` URI. A hostile file name is one argument or one escaped URI.
- A button key from the server that is not one of `open`, `folder`, `edit`,
  `copy`, `default` does nothing.
- **Signals are believed only from the server that answered `Notify`.** The
  bus stamps every message with its sender's unique name, which cannot be
  forged; `next_event` compares it with the sender of the `Notify` reply
  (`is_from`), and the match rule is restricted to the notification service's
  name so another program cannot crowd the queue with look-alikes. The e2e
  test forges `ActionInvoked` for every button from another connection and
  checks that nothing runs.
- The `--post` helper reads its job from stdin: length-prefixed fields, at
  most 320 MiB, checked arithmetic, relative paths and mis-sized thumbnails
  dropped. Anyone can run the helper, with any job; that is the same power as
  running `xdg-open`.
- Notification text is escaped (`& < >`) because the server may read it as
  markup. The notification body is made from fixed words, the saved file's own
  generated name, config and system error messages, and the sanitized caller
  name.

## Parsers of untrusted input

| Input | Parser | Limits |
|---|---|---|
| KWin's ScreenShot2 reply and pixel pipe | `capture/kwin.rs` | Width and height 1 to 16384, stride at least 4 x width, total bytes capped before allocating, 10 s timeout, only known `QImage` formats. |
| Compositor buffers (ext-image-copy-capture, wlr-screencopy) | `capture/wl.rs` | Same size and format checks, sealed memfd sizes. |
| Config file | `config.rs` (TOML) | 64 KiB, regular files only (non-blocking open, no FIFO hang), `deny_unknown_fields`, every value range-checked, bad file gives the defaults plus one warning. |
| `--post` job on stdin | `post::decode` | 320 MiB, checked offsets. |
| PNG on stdin of `--save-png` / `--copy-png` | `read_png` | 256 MiB, signature and header decoded, 1 to 16384 pixels a side. Bytes are stored as they are; nothing is decoded fully. |
| Command line | `cli::parse_args` | Fixed grammar; delay 0-600; region values bounded; names for `--requested-by` printable and at most 64 characters. |
| `user-dirs.dirs` | `store::user_dir_from` | 64 KiB; absolute, no `..`, no NUL. |
| Images and files in the editor | `editor/src/backend.cpp` | Opened once and checked on the open descriptor (regular file only; a FIFO swapped in cannot block); at most 256 MiB; the format must be PNG, JPEG, WebP, BMP or GIF by content (SVG, PDF and the like are refused); at most 16384 pixels a side and 1 GiB of pixels, checked from the header before decoding, plus `QImageReader::setAllocationLimit`. The editor never gets the capture right, and its core dumps are off. |
| OCR models | `models.rs` | Pinned SHA-256 and size, re-checked on every load on the bytes that are then parsed. |

Property tests (`cargo test`, `src/proptests.rs`) and libFuzzer targets
(`fuzz/`, run for a short time by `.github/workflows/fuzz.yml`) check the
file-name builder and the parsers above for panics and for their invariants.

## Network

Only the OCR model download, the first time a text or redact capture is made.
Fixed HTTPS URLs on one host, no redirects, no proxy from the environment,
10 s connect and 60 s total timeouts, body cap of expected size plus 64 KiB,
`rustls` with bundled roots (no OpenSSL; `deny.toml` bans it), SHA-256
and size pinned in the source. A model on the server that no longer matches
is refused, not used.

## Build and supply chain

- **Compiler and linker.** The RPM builds with Fedora's flags (`%{build_rustflags}`
  for Rust, `%{build_cflags}` for the C that cargo builds, `%{build_cxxflags}` and
  `%{build_ldflags}` for the editor). `%check` runs `scripts/check-hardening.sh`
  on both programs: position independent, `PT_GNU_RELRO`, `BIND_NOW`, no
  executable stack, no text relocations, no RPATH/RUNPATH, stack protector in the C++
  editor (a notice for Rust, which has none on stable). It also fails on a
  build path in the binary, a setuid/setgid/world-writable file, a second
  restricted interface, or the editor asking for one. Release builds check
  integer overflow (`overflow-checks = true`, `panic = "abort"`).
- **Dependencies.** `cargo deny` (advisories, bans, licences, sources) in
  `.github/workflows/audit.yml`, on every dependency change and weekly; `Cargo.lock`
  is committed and every build uses `--locked`. No git dependencies; crates
  from crates.io only. Dependabot proposes updates weekly.
- **CI.** `.github/workflows/ci.yml`: `cargo fmt --check`, `clippy -D warnings`,
  `cargo test`, and the RPM build (with `%check`, the editor's tests and the
  hardening check on the packages produced). Workflows have `contents: read`
  and nothing else, run no code from forks with secrets (there are none), pin
  every action by commit SHA, and check out with `persist-credentials: false`.
  `CODEOWNERS` puts `.github/`, `packaging/`, `data/`, `src/dbus.rs`,
  `deny.toml` and the lock files under owner review.
- **Dev containers** run with `--ulimit core=0`, so a crashing test cannot leave
  a core with screen pixels in the host's crash store.

## Accepted risks

1. **Code running as the user can capture.** KWin's grant is by path, not by
   the user's intent. A process of the same user can run
   `telamon-screenshot --full --no-notify` (silent, to the clipboard),
   `LD_PRELOAD` its own code into the granted binary, or read the files in
   `Pictures/Screenshots`. This is the same as `spectacle --background` on
   stock Plasma and as any Wayland screenshot tool; the defence is that
   Wayland isolates applications from each other and the capture right is
   KWin's to give. A user who runs hostile code as themselves has lost more
   than the screen. Sandboxed apps get none of this.
2. **The Spectacle service lets a program ask for a picture.** Mitigated as
   above (notification naming it, countdown, rate limit, no pixels returned).
   Removing `telamon-screenshot-spectacle-compat` removes it.
3. **Whoever owns `org.freedesktop.Notifications` sees the thumbnail** of an
   unsaved capture. On Telamon OS that is Plasma's shell; a program that took
   the name before the shell would also see it.
4. **Clipboard history** (Klipper) keeps what the user copies, screenshots
   included.
5. **Redaction is best-effort** (OCR, line by line). Documented in the README.
6. **Core dumps are off**, so a crash of the CLI leaves no backtrace in
   coredumpctl. The panic notification is the report. (A later Reliable-phase
   pass may add a crash reporter that never includes memory.)
7. **Editor Save As follows a symlink at the path the user typed**, and its
   "file exists" prompt follows it too.
8. **Unit tests use predictable temp folders under `$TMPDIR`**, created fresh;
   they are not part of the shipped program.

## What would change this

A new way in (a portal, a second D-Bus name, a URL handler), a new place the
picture is sent or kept (upload, history, cloud), a new parser, or a new
program in the package that is granted a restricted interface. Each needs an
entry here and a test before it ships.
