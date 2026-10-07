# Telamon Screenshot: region screenshots to the clipboard (image, OCR text or
# redacted image), for Telamon OS.

%global debug_package %{nil}
%global app_id net.eterneon.telamon.screenshot

Name:           telamon-screenshot
Version:        0.2.0
Release:        1%{?dist}
Summary:        Region screenshots to the clipboard as image, text or redacted image
License:        MIT
URL:            https://github.com/EternalCoder454/atlasos-screenshot
Source0:        telamon-screenshot-%{version}.tar.gz
# Renamed from atlasos-screenshot in 0.2.0.
Obsoletes:      atlasos-screenshot < 0.2.0
Provides:       atlasos-screenshot = %{version}-%{release}
# No OCR models: the app downloads them (sha256-pinned) on the first Ctrl or
# Alt drag, into ~/.local/share/telamon-screenshot/models.

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
Telamon Screenshot freezes the screen and copies the region you drag to the
clipboard: as a PNG, as recognised text when Ctrl is held, or as a PNG with
e-mail addresses, IP addresses and MAC addresses covered when Alt is held.
It has no window and writes nothing to disk unless configured to.

%prep
%autosetup -n telamon-screenshot-%{version}

%build
# NETWORK: cargo fetches crates.io during %%build (works in podman, not in an
# offline mock/Koji build). CARGO_HOME from the environment keeps a crate cache.
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
export RUSTFLAGS="%{build_rustflags} --remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=cargo"
cargo build --release --locked

%install
install -Dpm0755 target/release/telamon-screenshot %{buildroot}%{_bindir}/telamon-screenshot
# The old name keeps working in a shortcut or a script (this release only). It is
# a link: KWin grants ScreenShot2 by the real path of the running binary, which
# is the one the .desktop file names.
ln -s telamon-screenshot %{buildroot}%{_bindir}/atlasos-screenshot
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/applications/%{app_id}.desktop
# kglobalaccel reads launch shortcuts (X-KDE-Shortcuts) from here.
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/kglobalaccel/%{app_id}.desktop
install -Dpm0644 data/config.example.toml %{buildroot}%{_docdir}/%{name}/config.example.toml

%check
rc=0
grep -qF "%{_builddir}" %{buildroot}%{_bindir}/telamon-screenshot || rc=$?
if [ "$rc" != 1 ]; then
    echo "telamon-screenshot holds the build path %{_builddir} (grep status $rc)" >&2
    exit 1
fi
test "$(readlink %{buildroot}%{_bindir}/atlasos-screenshot)" = telamon-screenshot
grep -qx 'Exec=%{_bindir}/telamon-screenshot' %{buildroot}%{_datadir}/applications/%{app_id}.desktop
grep -qx 'X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2' %{buildroot}%{_datadir}/applications/%{app_id}.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
cargo test --release --locked

%files
%license LICENSE
%doc README.md
%{_docdir}/%{name}/config.example.toml
%{_bindir}/telamon-screenshot
%{_bindir}/atlasos-screenshot
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/kglobalaccel/%{app_id}.desktop

%changelog
* Wed Oct 07 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.2.0-1
- Renamed to Telamon Screenshot: package, binary, app ID and folders are telamon-screenshot and
  net.eterneon.telamon.screenshot. Obsoletes/Provides atlasos-screenshot; /usr/bin/atlasos-screenshot
  stays as a link for this release; config and models move to the new folders on the first run.

* Mon Oct 05 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.1.0-1
- First package
