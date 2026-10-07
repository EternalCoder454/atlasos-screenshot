# Telamon Screenshot

Rust CLI for Telamon OS (Fedora Kinoite 44 + Plasma 6, KWin 6.7): freeze the
screen, drag a region, copy it as PNG / OCR text (Ctrl) / redacted PNG (Alt).
Read `docs/DESIGN.md` first. Ships next to Spectacle (which keeps Print,
recording and annotation); bound to Meta+Shift+S through its `.desktop` file.

## Hard rules

- **No toolkit, no Qt.** An approved exception to the Telamon stack: the app
  has no windows. The overlay is layer-shell + SHM, drawn by hand, with the
  Telamon.Ui accent, radius and border copied as constants (`src/overlay.rs`,
  `src/config.rs`).
- **No spawned processes, no portal.** Capture is KWin ScreenShot2, then
  ext-image-copy-capture, then wlr-screencopy (`src/capture*`). The only
  fork is the clipboard server (`src/clipboard.rs`).
- **No disk writes** except `output.save_dir` (opt-in) and the OCR model
  download into `$XDG_DATA_HOME/telamon-screenshot/models` (`src/models.rs`):
  fixed HTTPS URLs, pinned SHA-256, 0700 folder, temp file + rename.
- **Redaction is an opaque fill, never a blur**, and documented as best-effort.
- The config file is untrusted: bad or unknown values give the defaults plus
  one warning, never a panic.
- Mocks only in tests; the shipped binary captures, OCRs and copies for real.
- **Builds run in the fedora:44 container** (`scripts/dev.sh <cmd>`), output
  under `/work` = `~/.cache/claude-builds/telamon-screenshot`, one
  `CARGO_TARGET_DIR` per agent. Intensive jobs go through
  `~/.claude/heavy/run.sh`; final RPM builds and UI runs go to the "AtlasOS"
  coordinator session.
- Commits are authored as
  `EternalHell <77252745+EternalCoder454@users.noreply.github.com>`.
  Licence MIT, app ID `net.eterneon.telamon.screenshot`.

## Commands

| Task | Command |
|---|---|
| Format | `scripts/dev.sh cargo fmt --check` |
| Lint | `scripts/dev.sh cargo clippy --all-targets --locked -- -D warnings` |
| Tests | `scripts/dev.sh cargo test --locked` |
| Release build | `scripts/dev.sh cargo build --release --locked` |
| Redraw benchmark | `scripts/dev.sh cargo test --release --locked redraw_bench -- --ignored --nocapture` |
| End-to-end (KWin) | `scripts/e2e-kwin.sh` (in `localhost/atlasos:mon7`, see the script) |
| RPM | `podman run --rm --init --security-opt label=disable -v "$PWD":/src:ro -v <out>:/out -v telamon-cargo:/root/.cargo/registry -e CARGO_HOME=/root/.cargo registry.fedoraproject.org/fedora:44 /src/packaging/build-rpm.sh /out` |
