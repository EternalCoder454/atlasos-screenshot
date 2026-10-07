# Telamon Screenshot: design

## What it is

The screenshot tool of Telamon OS, a replacement for Spectacle without screen
recording. A single-shot CLI: one command per capture, no tray, no daemon, no
toolkit. It captures (the whole desktop, a screen, a window, or a region
dragged on the frozen desktop), saves a PNG, puts the picture on the clipboard
and shows a notification with buttons. The one window is the annotation
editor, a separate Qt program (`editor/`), so the lean path never loads Qt.

Shortcuts (Spectacle's, with its desktop-action ids so a shortcut a user
changed there can move over by name): Print runs the default mode, Shift+Print
`--full`, Meta+Print `--active-window`, Meta+Shift+Print and Meta+Shift+S
`--region`, Meta+Ctrl+Print `--window`.

Print takes a region because that is what Spectacle's does: its Print key
runs a bare `spectacle`, and Spectacle 6.7's `launchAction` defaults to
`TakeRectangularScreenshot` (spectacle.kcfg, v6.7.2). Two defaults differ on
purpose: Spectacle saves and copies only on request (`autoSaveImage=false`,
`clipboardGroup=PostScreenshotDoNothing`) and opens its window; this tool
saves to the same folder (`Pictures/Screenshots`, `Screenshot_<date>_<time>`)
and copies at once, then notifies, and has no window until Edit.

## Data flow

```
main ── cli::parse_args ── config::load
     ── single_instance lock (not for --region X,Y,WxH)
     ── countdown::wait(--delay)          layer-shell badge; destroyed and answered before the capture
     ── capture, by kind:
     │    region   Session::connect ── capture_workspace (KWin ScreenShot2, else ext-image-copy-capture
     │             / wlr-screencopy per output, composed) ── overlay::select ── Frame::crop
     │    full / screen / active-window / window
     │             capture::grab ── KWin CaptureWorkspace / CaptureActiveScreen / CaptureActiveWindow /
     │             CaptureInteractive(window); full also works through the Wayland protocols
     ── mode: Image -> PNG | Text (Ctrl, --mode text): models::ensure -> ocrs -> text
     │       | Redact (Alt): ocrs lines -> regex -> opaque boxes -> PNG
     ── store::save_png    (Image and Redact; not with --no-save or output.save = false)
     ── clipboard::copy    (prepare, then fork a detached server)
     ── post::finish       (fork a detached child: the notification and its buttons, or the editor)
```

For a region the frame is taken once, before the overlay maps, and every mode
crops from it, so "what you saw is what you get" holds for every backend.
Screens and windows are KWin's own pictures at native resolution, with the
cursor, window frame and window shadow chosen by `--cursor`, `--no-frame`,
`--no-shadow` (config: `capture.*`); the pointer's screen and windows need
KWin, and on other compositors `--screen` is every screen and `--window`
reports that it needs KWin.

## Threading

Single-threaded, except where libraries add threads: zbus (its connection is
dropped right after use) and rayon inside ocrs/rten (parked workers after
OCR). A watchdog thread bounds the waits on the compositor that have no
timeout of their own (connect, capture, overlay setup and teardown). It
notifies the user and exits on expiry, and it is joined before the fork.

The clipboard fork happens after all of that, when the Wayland and D-Bus
connections are closed. The child:
- closes every descriptor that was open before the copy was prepared;
- runs wl-clipboard-rs' serve loop on its own connection, then `_exit`s;
- reports a failure to the parent through a pipe, which the parent waits
  up to 100 ms for, so a rejected selection is an error and not a silent
  empty clipboard. The clipboard is live during that wait; only the
  process exit is later.

Only one capture runs at a time (the delay and the window picker included).
It holds a `flock` on `$XDG_RUNTIME_DIR/telamon-screenshot.lock`, so a second
hotkey press only notifies that one is in progress; `--region X,Y,WxH` skips
the lock. The lock is released before the clipboard and notification
children are forked, and they close every inherited descriptor.

`post::finish` forks one more detached child (like the clipboard server, after
all connections and threads of the parent are gone). It opens its own D-Bus
connection, subscribes to the notification server's signals, sends the
notification, and serves `ActionInvoked` until the notification is closed
other than by expiring (an expired one is still in Plasma's history), or five
minutes pass. With `--edit` it only starts the editor. The parent returns
as soon as the clipboard is set, so `$(telamon-screenshot ...)` gets the saved
path at once.

## Privilege and attack surface

- **Privilege.** No root and no polkit. KWin grants ScreenShot2 to
  `/usr/bin/telamon-screenshot` because the packaged `.desktop` file names it
  with `X-KDE-DBUS-Restricted-Interfaces`.
- **Input from KWin.** Image metadata is range-checked (16384² max, stride ≥
  4·width) before it sizes an allocation. Pipe reads are capped to that size,
  with a 10 s timeout.
- **Input from the compositor.** Buffer sizes and formats are checked the
  same way. Dispatching times out after 5 s during capture.
- **Config.** Capped at 64 KiB, regular files only, `deny_unknown_fields`,
  every value range-checked. Problems give the defaults plus one warning.
- **Arguments.** Fixed grammar; region values are bounded. `--region` skips
  the overlay, so any process of the user's can take a screenshot without a
  gesture by running the binary. That matches `spectacle --background` on
  stock Plasma, and KWin's grant is by path, so the binary in root-owned
  `/usr/bin` can't be swapped. Accepted as parity.
- **Notifications.** The body is escaped, because the server may read it as
  markup. The buttons are a fixed set (`open`, `folder`, `edit`, `copy`, and
  the body click as `default`); a key the server sends that is not one of them
  does nothing. They run argument vectors, never a shell: `xdg-open <file>`,
  `<editor> <file>` (or `-` with the PNG on stdin when nothing was saved),
  this program with `--copy-png` (PNG on stdin), and the file manager's
  `ShowItems` with a percent-escaped `file://` URI (else `xdg-open <folder>`).
  A file name is one argument and one escaped URI, so a hostile name stays a
  name. Tested in `actions.rs`.
- **The editor and `org.kde.Spectacle`.** `--save-png` and `--copy-png` read a
  PNG on stdin (at most 256 MiB, signature checked, at most 16384 pixels a
  side, decoded header only) so the editor needs no file or clipboard code.
  `--dbus` owns `org.kde.Spectacle` and answers each method by running this
  program with fixed flags (the `-1/0/1` arguments choose among them); it
  takes no path or text from the caller. Any process of the user's could ask
  for a screenshot over D-Bus; that is what Spectacle's service allows too.
- **Network.** Only for the models: fixed HTTPS URLs on one host, no
  redirects, rustls with bundled roots. Timeouts are 10 s to connect and
  60 s per file, the body is capped at the expected size + 64 KiB, and there
  is one retry with backoff.
- **Model files.** The folder is 0700, ours, not a symlink, and writable by
  nobody else. Files are opened with `O_NOFOLLOW`, written through a temp file
  with `O_EXCL` + fsync + rename, and the SHA-256 is checked on every load, on
  the same bytes that are then parsed.
- **Saved screenshots.** Saved by default in `Pictures/Screenshots` (found
  from `user-dirs.dirs` without running `xdg-user-dir`, made if missing), or
  `output.save_dir`. Refused in a folder other users can write
  to: world-writable, or group-writable by a group other than the user's
  own (unless it has the sticky bit). The file is written unnamed
  (`O_TMPFILE`, 0600) and linked in under its final name with `linkat`,
  which never replaces a file. Without `O_TMPFILE` (or /proc), a random temp
  name is renamed with `RENAME_NOREPLACE`. Names use local time. A failed save is
  reported, and the copy still happens.
- **Redaction** is an opaque fill, never a blur. It is best-effort and
  documented as such.

## Overlay entry and exit (KWin animations)

KWin's Scale effect animates every window it maps, and unmaps: a fade and a
zoom of 160 ms at the default animation speed. On the frozen frame that
looked like a second, smaller copy of the screen fading in over the live one
as the screen dimmed ("duplicated background"), and the same in reverse when
the overlay closed. The overlay now:

- maps each screen as one transparent pixel stretched over it, and swaps in
  the dimmed frame `hold` after the compositor's first frame callback for
  it (the sign the animation started), or a fixed time after the configure
  if none comes. The animation then plays out on nothing. `hold` is 180 ms
  scaled by Plasma's animation speed (`AnimationDurationFactor`), and zero
  when KWin isn't running or has neither the `scale` nor the `fade` effect
  loaded. `TELAMON_HOLD_MS` overrides it (for tests).
- swaps the transparent pixel back in before it closes, so the close
  animation has nothing to show either.
- shows each screen 1:1: a screen drawn at a lower scale than the frame
  (frame pixels per logical unit is the highest scale of all outputs) is
  shown from its own `CaptureScreen`, not from the resampled frame, and the
  frame's scale snaps to a multiple of 1/120 (wp-fractional-scale), so the
  frozen frame lines up with what KWin drew.

The cost: the dim appears about 180 ms after the screen was frozen (the
frozen frame is what the selection crops, so nothing is lost; the user sees
the live screen for that long). Measured in KWin's virtual backend, capturing
the output while the overlay starts and quits: before, the fade showed a
residual of 4-7 gray levels (mean) against a plain live-to-dim blend, over
about 100 ms; after, the screen steps from live to dim in one frame, with
zero residual, on entry and on exit.

## Failure modes

| Failure | Behaviour |
|---|---|
| Not Wayland / no compositor | Plain error, exit 1 |
| No capture backend | Error naming what was tried |
| KWin refuses (binary not at /usr/bin) | Error explaining the install path |
| Output unplugged / layer closed during selection | Treated as a cancel |
| Esc / right click | Cancel, silent, exit 1 |
| Models missing and offline | Notification "Couldn't download the text recognition models: no internet connection", copy nothing |
| Model damaged | Re-downloaded |
| OCR finds no text | Notification, copy nothing |
| Alt finds nothing to cover | Copies the image, notifies that nothing was redacted |
| Model on the server replaced (hash differs) | No retry; says an update is needed |
| Compositor stops answering | Watchdog notifies and exits 1 |
| Bad config | Defaults, plus one warning (stderr and notification) |
| Panic | Notification, exit |
| Clipboard unavailable | Error, exit 1 |
| No active window | Error "there is no active window to capture", exit 1 |
| Window picker cancelled (Escape, right click) | Silent, exit 1 |
| Window or active screen without KWin | Error saying KWin is needed |
| No layer-shell for the countdown | The delay still holds, silently |
| Save fails (read-only, shared folder, no space) | The notification says so, the clipboard still has the picture |
| No notification server | Nothing is shown; the captured path is still printed |
| Editor missing | The Edit button does nothing; `--edit` leaves the picture saved |
| `org.kde.Spectacle` already owned | `--dbus` exits with that error |

Every error goes to stderr and to a desktop notification. The app runs from
a hotkey, so stderr alone would be invisible.

## Performance budget

- **Hotkey to overlay:** the frame is frozen under 150 ms at 4K + 1080p;
  under KWin's open animation the dim follows after the hold (see above).
- **Release to clipboard (image):** under 300 ms for a full 4K frame (fast
  PNG compression).
- **Selection redraw:** at most one per frame callback, however fast the
  pointer moves. Each screen reuses two SHM buffers (a third only if the
  compositor holds both) and repaints, from a dimmed copy of the frame made
  once, only the strips the selection's edges crossed since that buffer was
  last shown; only what changed since the last commit is damaged, and a
  screen the change doesn't touch isn't committed. A full-screen drag at
  3840x2160 costs about 0.6 ms of CPU per frame (`redraw_bench`, below),
  where repainting the whole selection every frame cost 50-70 ms.
- **Idle CPU:** 0. It blocks in `poll` while waiting for input.
- **RSS:** about 4× the frame size during selection (frame, and per screen
  its dimmed copy and two buffers). OCR adds about 60 MB.
- **Binary:** release, LTO, stripped.

The redraw benchmark replays a two-second full-screen drag at 3840x2160
(scale 1.7) with the old and the new painting:
`scripts/dev.sh cargo test --release --locked redraw_bench -- --ignored --nocapture`.

## Names before the rename (0.2.0)

The app was `atlasos-screenshot` (`net.eterneon.atlas.screenshot`) until 0.2.0.
For that release only, and to be removed after the image has moved:

- the package `Obsoletes`/`Provides` `atlasos-screenshot`, and
  `/usr/bin/atlasos-screenshot` is a link to `telamon-screenshot`. KWin
  resolves the caller to its real path (`/proc/<pid>/exe`), so the link runs
  with the grant of the new `.desktop` file (checked against KWin in
  `scripts/e2e-kwin.sh`);
- the lock is taken under `telamon-screenshot.lock` and
  `atlasos-screenshot.lock`, so a running instance of either name keeps a
  second overlay out;
- `src/legacy.rs` moves `$XDG_CONFIG_HOME/atlasos-screenshot` and
  `$XDG_DATA_HOME/atlasos-screenshot` to the new names, once, with one
  `renameat2(RENAME_NOREPLACE)` (atomic, never replaces). If the move fails
  the config is read in place, and the models are downloaded again.

The old `.desktop` file is not kept: two files with the same `X-KDE-Shortcuts`
would fight over Meta+Shift+S. A shortcut the user changed is stored by
kglobalaccel under the old file name and is not carried over.
