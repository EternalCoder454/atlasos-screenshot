# AtlasOS Screenshot: region screenshots to the clipboard (image, OCR text or
# redacted image), for AtlasOS.

%global debug_package %{nil}
%global app_id net.eterneon.atlas.screenshot

Name:           atlasos-screenshot
Version:        0.1.0
Release:        1%{?dist}
Summary:        Region screenshots to the clipboard as image, text or redacted image
License:        MIT
URL:            https://github.com/EternalCoder454/atlasos-screenshot
Source0:        atlasos-screenshot-%{version}.tar.gz
# No OCR models: the app downloads them (sha256-pinned) on the first Ctrl or
# Alt drag, into ~/.local/share/atlasos-screenshot/models.

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  rust-srpm-macros
BuildRequires:  gcc
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  desktop-file-utils

# The keyboard state of the selection overlay
Requires:       libxkbcommon
# Only KWin's ScreenShot2 (granted by the .desktop file) or a wlroots
# compositor can capture: no portal fallback.
Recommends:     kwin

%description
AtlasOS Screenshot freezes the screen and copies the region you drag to the
clipboard: as a PNG, as recognised text when Ctrl is held, or as a PNG with
e-mail addresses, IP addresses and MAC addresses covered when Alt is held.
It has no window and writes nothing to disk unless configured to.

%prep
%autosetup -n atlasos-screenshot-%{version}

%build
# NETWORK: cargo fetches crates.io during %%build (works in podman, not in an
# offline mock/Koji build). CARGO_HOME from the environment keeps a crate cache.
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
export RUSTFLAGS="%{build_rustflags} --remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=cargo"
cargo build --release --locked

%install
install -Dpm0755 target/release/atlasos-screenshot %{buildroot}%{_bindir}/atlasos-screenshot
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/applications/%{app_id}.desktop
# kglobalaccel reads launch shortcuts (X-KDE-Shortcuts) from here.
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/kglobalaccel/%{app_id}.desktop
install -Dpm0644 data/config.example.toml %{buildroot}%{_docdir}/%{name}/config.example.toml

%check
rc=0
grep -qF "%{_builddir}" %{buildroot}%{_bindir}/atlasos-screenshot || rc=$?
if [ "$rc" != 1 ]; then
    echo "atlasos-screenshot holds the build path %{_builddir} (grep status $rc)" >&2
    exit 1
fi
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
cargo test --release --locked

%files
%license LICENSE
%doc README.md
%{_docdir}/%{name}/config.example.toml
%{_bindir}/atlasos-screenshot
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/kglobalaccel/%{app_id}.desktop

%changelog
* Mon Oct 05 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.1.0-1
- First package
