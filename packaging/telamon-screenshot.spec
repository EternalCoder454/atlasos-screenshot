# Telamon Screenshot, for Telamon OS: the replacement for KDE Spectacle (without
# screen recording). Two programs: the Rust CLI (this crate) and the Qt Quick
# annotation editor in editor/, built with its own CMake tree.

%global debug_package %{nil}
%global app_id net.eterneon.telamon.screenshot

Name:           telamon-screenshot
Version:        0.3.0
Release:        1%{?dist}
Summary:        Screenshots for Telamon OS: screen, window or region, copied, saved and annotated
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
# The editor
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  ninja-build
BuildRequires:  cmake(Qt6Core)
BuildRequires:  cmake(Qt6Gui)
BuildRequires:  cmake(Qt6Widgets)
BuildRequires:  cmake(Qt6Qml)
BuildRequires:  cmake(Qt6Quick)
BuildRequires:  cmake(Qt6QuickControls2)
BuildRequires:  cmake(Qt6Test)
BuildRequires:  cmake(Qt6QuickTest)
BuildRequires:  qt6-qtbase-devel
BuildRequires:  qt6-qtdeclarative-devel
# QML modules qmlcachegen resolves at build time (not linked), and the tests
# in %%check load. telamon-ui comes from telamon-framework, which is in no
# repository: install its RPMs first (build-rpm.sh does, given
# TELAMON_LOCAL_RPMS).
BuildRequires:  telamon-ui >= 2.0.0
BuildRequires:  kf6-kirigami
BuildRequires:  kf6-qqc2-desktop-style
BuildRequires:  qt6-qtsvg

# The keyboard state of the selection overlay
Requires:       libxkbcommon
# The notification's Open button (and Show in Folder without a file manager).
Requires:       xdg-utils
# The editor: Qt Quick on Telamon.Ui (Kirigami and the Plasma style give it
# the desktop's colours and fonts; the icons are SVG).
Requires:       telamon-ui >= 2.0.0
Requires:       kf6-kirigami
Requires:       kf6-qqc2-desktop-style
Requires:       qt6-qtsvg
Requires:       hicolor-icon-theme
# Only KWin's ScreenShot2 (granted by the .desktop file) or a wlroots
# compositor can capture: no portal fallback.
Recommends:     kwin

%description
Telamon Screenshot takes the whole desktop, a screen, a window or a region
dragged on the frozen desktop; saves it to Pictures/Screenshots, copies it to
the clipboard and notifies with Open, Show in Folder, Edit and Copy. A region
can also be copied as recognised text (Ctrl) or as a PNG with e-mail
addresses, IP addresses and MAC addresses covered (Alt). The annotation editor
(telamon-screenshot-editor) draws arrows, boxes, text and numbers, crops and
redacts. It replaces KDE Spectacle, without screen recording, and uses
Spectacle's shortcuts.

%package spectacle-compat
Summary:        Answers KDE Spectacle's D-Bus service with Telamon Screenshot
Requires:       %{name} = %{version}-%{release}
# Stands in for the spectacle package: the two can't be installed together
# (both own org.kde.Spectacle).
Provides:       spectacle
Conflicts:      spectacle

%description spectacle-compat
Programs that ask org.kde.Spectacle for a screenshot (the Plasma panel's
screenshot entries, scripts using its D-Bus interface) get Telamon
Screenshot's answer. Ships the D-Bus service file. It conflicts with the
spectacle package, which has to be removed first (dnf install --allowerasing
does it): the two can't own org.kde.Spectacle and Print together.

%prep
%autosetup -n telamon-screenshot-%{version}

%build
# NETWORK: cargo fetches crates.io during %%build (works in podman, not in an
# offline mock/Koji build). CARGO_HOME from the environment keeps a crate cache.
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
export RUSTFLAGS="%{build_rustflags} --remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=cargo"
cargo build --release --locked

# The editor, in its own build tree (the cargo one is target/). Its tests are
# built too, and run in %%check; they are not installed.
export CFLAGS="%{build_cflags} -ffile-prefix-map=$PWD=."
export CXXFLAGS="%{build_cxxflags} -ffile-prefix-map=$PWD=."
%global _vpath_srcdir editor
%cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DTELAMON_EDITOR_TESTS=ON
%cmake_build

%install
install -Dpm0755 target/release/telamon-screenshot %{buildroot}%{_bindir}/telamon-screenshot
# The old name keeps working in a shortcut or a script (this release only). It is
# a link: KWin grants ScreenShot2 by the real path of the running binary, which
# is the one the .desktop file names.
ln -s telamon-screenshot %{buildroot}%{_bindir}/atlasos-screenshot
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/applications/%{app_id}.desktop
# kglobalaccel reads launch shortcuts (X-KDE-Shortcuts) from here.
install -Dpm0644 data/%{app_id}.desktop %{buildroot}%{_datadir}/kglobalaccel/%{app_id}.desktop
%cmake_install
install -Dpm0644 data/org.kde.Spectacle.service %{buildroot}%{_datadir}/dbus-1/services/org.kde.Spectacle.service
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
grep -qx 'Exec=%{_bindir}/telamon-screenshot --dbus' %{buildroot}%{_datadir}/dbus-1/services/org.kde.Spectacle.service
grep -qx 'Name=org.kde.Spectacle' %{buildroot}%{_datadir}/dbus-1/services/org.kde.Spectacle.service
test -x %{buildroot}%{_bindir}/telamon-screenshot-editor
grep -qx 'Exec=%{_bindir}/telamon-screenshot-editor %%f' %{buildroot}%{_datadir}/applications/%{app_id}.editor.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.editor.desktop
grep -qx 'X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2' %{buildroot}%{_datadir}/applications/%{app_id}.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
cargo test --release --locked
# The editor's tests: the C++ backend and the Qt Quick ones, on the offscreen
# platform (the tests' environment sets it) with the real Telamon.Ui.
%ctest

%files
%license LICENSE
%doc README.md
%{_docdir}/%{name}/config.example.toml
%{_bindir}/telamon-screenshot
%{_bindir}/atlasos-screenshot
%{_bindir}/telamon-screenshot-editor
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/applications/%{app_id}.editor.desktop
%{_datadir}/kglobalaccel/%{app_id}.desktop

%files spectacle-compat
%{_datadir}/dbus-1/services/org.kde.Spectacle.service

%changelog
* Wed Oct 07 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.3.0-1
- Replaces KDE Spectacle (without screen recording): whole desktop, screen, active window, a
  window you click, or a region; saved to Pictures/Screenshots, copied, and a notification with
  Open, Show in Folder, Edit and Copy; --delay with a countdown; Spectacle's shortcuts and
  desktop actions, so shortcuts can move over by name
- telamon-screenshot-editor: the annotation editor (arrows, boxes, text, numbers, crop,
  redact), Qt Quick on telamon-ui 2.0.0; opened by --edit, Shift at the end of a drag and
  the notification's Edit button
- telamon-screenshot-spectacle-compat: answers org.kde.Spectacle; provides and conflicts
  with spectacle
- The overlay no longer shows KWin's open and close animation on the frozen frame, and
  shows each screen at its own scale

* Wed Oct 07 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.2.0-1
- Renamed to Telamon Screenshot: package, binary, app ID and folders are telamon-screenshot and
  net.eterneon.telamon.screenshot. Obsoletes/Provides atlasos-screenshot; /usr/bin/atlasos-screenshot
  stays as a link for this release; config and models move to the new folders on the first run.

* Mon Oct 05 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.1.0-1
- First package
