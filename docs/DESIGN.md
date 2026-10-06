# AtlasOS Screenshot: design

## What it is

A single-shot CLI, bound to Meta+Shift+S. It freezes the screen, the user
drags a region, and the result goes to the clipboard. There is no window,
tray, daemon or toolkit. Spectacle stays for Print, recording and annotation.

## Data flow

```
main ── Session::connect (Wayland, xdg-output layout)
     ── capture_workspace ── KWin ScreenShot2 (D-Bus, pipe fd)
     │                     └ else ext-image-copy-capture / wlr-screencopy per output, composed
     │   => Frame { RGBA image of the whole layout, bounds, scale = max output scale }
     ── overlay::select (layer-shell per output; dimmed frame + selection subsurface)
     │   => (Rect in logical coords, Mods at release) | cancel
     ── drop Wayland connection
     ── Frame::crop
     ── mode: Image -> PNG | Ctrl: models::ensure -> ocrs -> text | Alt: ocrs lines -> regex -> opaque boxes -> PNG
     ── optional save (only with output.save_dir)
     ── clipboard::copy (prepare, then fork a detached server)
```

The frame is taken once, before the overlay maps, and every mode crops from
it, so "what you saw is what you get" holds for every backend.

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

Only one overlay runs at a time. It holds a `flock` on
`$XDG_RUNTIME_DIR/atlasos-screenshot.lock`, so a second hotkey press only
notifies that one is in progress; `--region` skips the lock.

## Privilege and attack surface

- **Privilege.** No root and no polkit. KWin grants ScreenShot2 to
  `/usr/bin/atlasos-screenshot` because the packaged `.desktop` file names it
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
  markup.
- **Network.** Only for the models: fixed HTTPS URLs on one host, no
  redirects, rustls with bundled roots. Timeouts are 10 s to connect and
  60 s per file, the body is capped at the expected size + 64 KiB, and there
  is one retry with backoff.
- **Model files.** The folder is 0700, ours, not a symlink, and writable by
  nobody else. Files are opened with `O_NOFOLLOW`, written through a temp file
  with `O_EXCL` + fsync + rename, and the SHA-256 is checked on every load, on
  the same bytes that are then parsed.
- **Saved screenshots** (opt-in). Refused in a folder other users can write
  to: world-writable, or group-writable by a group other than the user's
  own (unless it has the sticky bit). The file is written unnamed
  (`O_TMPFILE`, 0600) and linked in under its final name with `linkat`,
  which never replaces a file. Without `O_TMPFILE` (or /proc), a random temp
  name is renamed with `RENAME_NOREPLACE`. Names use local time. A failed save is
  reported, and the copy still happens.
- **Redaction** is an opaque fill, never a blur. It is best-effort and
  documented as such.

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

Every error goes to stderr and to a desktop notification. The app runs from
a hotkey, so stderr alone would be invisible.

## Performance budget

- **Hotkey to overlay:** under 150 ms at 4K + 1080p.
- **Release to clipboard (image):** under 300 ms for a full 4K frame (fast
  PNG compression).
- **Selection redraw:** follows frame callbacks, and only the selection's
  pixels are redrawn.
- **Idle CPU:** 0. It blocks in `poll` while waiting for input.
- **RSS:** about 3× the frame size during selection (frame + dimmed base +
  selection buffer). OCR adds about 60 MB.
- **Binary:** release, LTO, stripped.
