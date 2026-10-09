# Telamon Screenshot

Screenshot tool for Telamon OS (Fedora Kinoite 44 + Plasma 6, KWin 6.7), the
replacement for Spectacle without screen recording: whole desktop, screen,
window or region, saved to Pictures/Screenshots, copied, with a notification
(Open, Show in Folder, Edit, Copy); a region can also be copied as OCR text
(Ctrl) or redacted PNG (Alt). Read `docs/DESIGN.md` first. Print and Spectacle's
other shortcuts come from its `.desktop` file. Two programs: the Rust CLI (this
crate) and `editor/`, the Qt/QML annotation editor (own CMake build).

## Hard rules

- **The CLI has no toolkit, no Qt.** An approved exception to the Telamon
  stack: it has no windows. The overlay and the countdown are layer-shell +
  SHM, drawn by hand, with the Telamon.Ui accent, radius and border copied as
  constants (`src/overlay.rs`, `src/countdown.rs`, `src/config.rs`). The one
  window is the editor in `editor/` (Qt Quick on Telamon.Ui, Telamon.Ui
  controls only), a separate executable the CLI only starts.
- **Spawned processes are argv-only, never a shell**, and only the fixed set
  in `src/actions.rs`: `xdg-open`, the editor, this program (`--copy-png`).
  No portal. Capture is KWin ScreenShot2, then (whole desktop and regions)
  ext-image-copy-capture, then wlr-screencopy (`src/capture*`). Forks: the
  clipboard server (`src/clipboard.rs`) and the notification/editor child
  (`src/post.rs`).
- **Disk writes:** the PNG in `Pictures/Screenshots` or `output.save_dir`
  (`src/store.rs`: O_TMPFILE + linkat, never overwrites, refuses shared
  folders; `output.save = false` turns it off), and the OCR model download
  into `$XDG_DATA_HOME/telamon-screenshot/models` (`src/models.rs`): fixed
  HTTPS URLs, pinned SHA-256, 0700 folder, temp file + rename.
- **`org.kde.Spectacle`** (`src/dbus.rs`) mirrors Spectacle's interface so
  programs that ask it keep working; the action ids in the `.desktop` file are
  Spectacle's, so users' shortcuts can move by name. Don't rename them. A
  capture asked for over D-Bus is never silent (`--notify`, `--requested-by`,
  the countdown, the rate limit) and nothing but the file's path goes back.
- **Security rules** are in `docs/SECURITY.md`: update it with any new way in,
  place the picture goes, parser, or restricted-interface grant. The editor
  never gets a restricted interface. No `PR_SET_DUMPABLE` (KWin needs
  `/proc/<pid>/exe`); core dumps are off with `RLIMIT_CORE`. Programs started
  from a button are absolute paths. Dev containers run with `--ulimit core=0`.
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
| End-to-end (KWin) | `scripts/e2e-kwin.sh` (in `localhost/telamonos:telamon-test`; three parts: online, offline, modes) |
| Editor | `scripts/dev-editor.sh editor/build.sh` (builds and runs its tests) |
| Dependency audit | `cargo deny --locked check advisories bans licenses sources` (CI: `audit.yml`) |
| Hardening of a build | `scripts/check-hardening.sh [--rust] <elf>` (the spec's `%check` runs it) |
| Fuzz | see `fuzz/run.sh` (nightly, cargo-fuzz; CI: `fuzz.yml`) |
| RPM | `podman run --rm --init --security-opt label=disable --ulimit core=0 -v "$PWD":/src:ro -v <out>:/out -v telamon-cargo:/root/.cargo/registry -e CARGO_HOME=/root/.cargo registry.fedoraproject.org/fedora:44 /src/packaging/build-rpm.sh /out` |
