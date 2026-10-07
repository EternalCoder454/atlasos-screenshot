# Telamon Screenshot

The screenshot tool of Telamon OS, and the replacement for Spectacle (all of
it except screen recording). It takes the whole desktop, one screen, a window
or a region you drag on the frozen desktop; saves the picture in
`Pictures/Screenshots`; copies it to the clipboard; and tells you with a
notification you can click to Open, Show in Folder, Edit or Copy. The region
mode can also copy the text in the picture (OCR) or cover addresses in it.
No window, no toolkit for any of that; the annotation editor is the one
window, a separate program.

## Shortcuts

The same as Spectacle's, shipped in the `.desktop` file so KDE picks them up:

| Key               | What                                                          |
|-------------------|---------------------------------------------------------------|
| Print             | what `capture.default_mode` says (a region, by default, as Spectacle's Print) |
| Shift+Print       | the whole desktop (every screen), as one picture              |
| Meta+Print        | the active window                                             |
| Meta+Shift+Print  | a region (also **Meta+Shift+S**, the original snip key)       |
| Meta+Ctrl+Print   | a window you click (KWin's own picker)                        |

"Capture Current Monitor" (the screen the pointer is on) and "Open the
Editor" are in the launcher's right-click menu and in Shortcuts settings.

## Command line

```
telamon-screenshot [WHAT] [OPTIONS]

  -r, --region [X,Y,WxH]  drag a region (or give it, to skip the overlay)
  -f, --full              every screen        -m, --screen   the screen the pointer is on
  -a, --active-window     the focused window  -u, --window   a window you click

  --delay N               wait N seconds (0-600) with a countdown that is never in the picture
  --mode image|text|redact    what the capture becomes (default image)
  --no-save  --no-notify  --edit  --cursor/--no-cursor  --no-frame  --no-shadow
```

Exit status: 0 done, 1 cancelled or failed, 2 bad arguments. The path of the
saved file is printed on stdout.

While dragging a region: release for the picture; hold **Ctrl** when you
release for the text in it (OCR), **Alt** for the picture with e-mail, IPv4,
IPv6 and MAC addresses covered, **Shift** to open the editor with it. A click
without dragging takes the whole screen under the pointer. Escape or the right
mouse button cancels.

## Where it goes

- **A file**, `Screenshot_YYYYMMDD_HHMMSS.png` in `$(xdg-user-dir PICTURES)/Screenshots`
  (made if missing), unless `output.save = false` or `--no-save`. Text from
  OCR is never saved. A file is never overwritten, and a folder other users
  can write to is refused (the copy still happens).
- **The clipboard**, always. Like `wl-copy`, a small background copy of the
  tool serves it until something else is copied (Klipper usually takes over at
  once), then exits.
- **A notification** (`output.notify`) with the picture and the buttons Open,
  Show in Folder, Edit and Copy. The buttons run `xdg-open`, the file
  manager's D-Bus `ShowItems`, the editor and this program, never a shell. A
  detached helper waits for a click for up to five minutes.

## The editor

`telamon-screenshot-editor` (package `telamon-screenshot-editor`) is a small
Telamon.Ui window: crop, arrow, rectangle, pen, highlighter, text, numbered
markers, redact (a solid box, never a blur), undo and redo, colour and stroke
size, Copy, Save and Save As, and a New Screenshot menu with a delay. It opens
from `--edit`, Shift at the end of a drag, the notification's Edit, or the
launcher's "Open the Editor".

## Programs that ask Spectacle

`telamon-screenshot --dbus` answers `org.kde.Spectacle` (`FullScreen`,
`CurrentScreen`, `ActiveWindow`, `WindowUnderCursor`, `RectangularRegion`,
`StartAgent`, `OpenWithoutScreenshot`, with `ScreenshotTaken` and
`ScreenshotFailed`; the `Record*` methods answer `RecordingFailed`). The
`telamon-screenshot-spectacle-compat` package starts it on demand and conflicts
with `spectacle`.

## How it works

- **Capture.** One capture of the whole desktop, before the overlay appears:
  KWin's `org.kde.KWin.ScreenShot2` on Plasma (granted to `/usr/bin/telamon-screenshot`
  by its `.desktop` file), otherwise `ext-image-copy-capture-v1` or
  `wlr-screencopy` on wlroots compositors. No portal, no `grim`/`slurp`.
- **Overlay.** A `wlr-layer-shell` surface per screen shows the frozen frame
  dimmed, and the selection in a rounded border in the Telamon accent.
- **Clipboard.** Like `wl-copy`, a small background copy of the tool serves
  the clipboard until something else is copied (Klipper usually takes over
  at once), then exits.

## Redaction is best-effort

Alt+drag covers what OCR **reads**, line by line, with solid boxes (never a
blur, which can be reversed). Text OCR misreads, text in images, or an address
split across lines can slip through. Check the result before sharing it.

## Disk use

A capture is written only as the PNG in the Screenshots folder (turn that off
with `output.save = false`), plus one more thing approved for Telamon OS: the
first Ctrl or Alt capture downloads the OCR models (12 MB) into
`~/.local/share/telamon-screenshot/models` (`$XDG_DATA_HOME`). They come only
from the fixed upstream HTTPS URLs, are checked against pinned SHA-256 hashes
before use and again on every load, and are downloaded again if damaged.
Offline with no models, plain drags still work; Ctrl and Alt drags show a
notification and copy nothing.

## Renamed from AtlasOS Screenshot

Until 0.1.0 this was `atlasos-screenshot` (`net.eterneon.atlas.screenshot`).
The package obsoletes and provides the old name. For this release
`/usr/bin/atlasos-screenshot` stays as a link to `telamon-screenshot` (KWin
grants ScreenShot2 by the real path, which is the new binary the new
`.desktop` file names), and the single-instance lock is held under both
`telamon-screenshot.lock` and `atlasos-screenshot.lock`. On the first run the
folders `~/.config/atlasos-screenshot` (the config) and
`~/.local/share/atlasos-screenshot` (the OCR models) are moved, once and
atomically, to `telamon-screenshot`; a new folder that exists wins and the old
one is left alone.

## Credits and licences

- Telamon Screenshot: MIT (see `LICENSE`).
- OCR: [ocrs](https://github.com/robertknight/ocrs) and
  [RTen](https://github.com/robertknight/rten), MIT OR Apache-2.0.
- OCR models: [ocrs-models](https://github.com/robertknight/ocrs-models),
  trained on [HierText](https://github.com/google-research-datasets/hiertext)
  (CC-BY-SA 4.0). They are downloaded from upstream on first use, not
  redistributed with this package.

## Building

Builds run in a `fedora:44` container: `scripts/dev.sh cargo build --release`.
The RPM: `packaging/build-rpm.sh <out dir>` inside `fedora:44` (works from a
copy of the tree without `.git`).
