# AtlasOS Screenshot

Freeze the screen, drag a region, and it's on the clipboard. No window, no
toolkit, no files: just `atlasos-screenshot`, bound to **Meta+Shift+S** on
AtlasOS.

| Hold when you release the drag | You get                                                  |
|--------------------------------|----------------------------------------------------------|
| nothing                        | the region as a PNG (`image/png`)                        |
| **Ctrl**                       | the text in the region (`text/plain`), read by OCR       |
| **Alt**                        | the PNG with e-mail, IPv4, IPv6 and MAC addresses covered |

A click without dragging takes the whole screen under the pointer. Escape or
the right mouse button cancels.

```
atlasos-screenshot [--mode image|text|redact] [--region X,Y,WxH]
```

`--region` skips the overlay (for scripts); `--mode` sets what a plain drag
copies. Exit status: 0 copied, 1 cancelled or failed, 2 bad arguments.

## How it works

- **Capture.** One capture of the whole desktop, before the overlay appears:
  KWin's `org.kde.KWin.ScreenShot2` on Plasma (granted to `/usr/bin/atlasos-screenshot`
  by its `.desktop` file), otherwise `ext-image-copy-capture-v1` or
  `wlr-screencopy` on wlroots compositors. No portal, no `grim`/`slurp`.
- **Overlay.** A `wlr-layer-shell` surface per screen shows the frozen frame
  dimmed, and the selection in a rounded border in the Atlas accent.
- **Clipboard.** Like `wl-copy`, a small background copy of the tool serves
  the clipboard until something else is copied (Klipper usually takes over
  at once), then exits.

## Redaction is best-effort

Alt+drag covers what OCR **reads**, line by line, with solid boxes (never a
blur, which can be reversed). Text OCR misreads, text in images, or an address
split across lines can slip through. Check the result before sharing it.

## Disk use

Nothing is written to disk unless you set `output.save_dir` in the config
(see `config.example.toml`), with one exception approved for AtlasOS: the
first Ctrl or Alt drag downloads the OCR models (12 MB) into
`~/.local/share/atlasos-screenshot/models` (`$XDG_DATA_HOME`). They come only
from the fixed upstream HTTPS URLs, are checked against pinned SHA-256 hashes
before use and again on every load, and are downloaded again if damaged.
Offline with no models, plain drags still work; Ctrl and Alt drags show a
notification and copy nothing.

## Credits and licences

- AtlasOS Screenshot: MIT (see `LICENSE`).
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
