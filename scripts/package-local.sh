#!/usr/bin/env bash
# Build BackDown's installers on this machine from scripts/build-local.sh's archives, without root, and check
# each one. It never tags, pushes, publishes or uploads anything.
#
#   scripts/package-local.sh                  # the version in Cargo.toml (target/dist/v<version>/ must exist)
#   scripts/package-local.sh v0.2.0-alpha.5
#   scripts/package-local.sh --fetch-tools    # first get the packaging tools (below) into ~/.local/opt
#
# From target/dist/<tag>/ (read only; publish-release.sh publishes exactly that folder) into target/packages/<tag>/:
#   Linux    backdown-io_<version>_amd64.deb, backdown-io_<version>_arm64.deb (named 1.0.0-beta.2, versioned
#            1.0.0~beta.2 inside; "backdown" is taken: Debian and
#            Ubuntu ship an unrelated duplicate remover by that name, so ours conflicts with and replaces it,
#            and with our own packages from before the rename): /usr/bin/backdown, the applications-menu
#            entry and icons, AppStream metadata, and a systemd *user* unit anyone can turn on
#            (systemctl --user enable --now backdown); nothing is enabled for anyone by installing. No maintainer
#            scripts: dpkg's triggers refresh the menu and icon caches.
#            BackDown-<version>-x86_64.AppImage, -aarch64.AppImage (with appimagetool): run with no arguments it
#            opens BackDown's window; with arguments it's the backdown command.
#   macOS    BackDown-<version>.dmg: BackDown.app (the universal binary as the bundle's executable, Info.plist,
#            .icns; ad-hoc signed by rcodesign) beside an Applications link, as a compressed HFS+ disk image made
#            with mkfs.hfsplus and libdmg-hfsplus. Also BackDown-<version>-macos.zip (the same .app).
#            Not notarised: that needs an Apple Developer ID. The first open needs right-click > Open.
#   Windows  BackDown-<version>-x64.msi (with msitools' wixl): per user, no administrator rights; installs to
#            %LOCALAPPDATA%\Programs\BackDown with a Start menu entry and a PATH entry.
#            BackDown-<version>-windows-x86_64-setup.zip: the same, by double-clicking "Install BackDown.cmd"
#            (and "Uninstall BackDown.cmd"). Not code-signed: SmartScreen may ask once.
# plus SHA256SUMS.txt. The desktop files, icons, Info.plist, installer scripts and MSI source are written by the
# Linux build itself (`backdown desktop files`, unit-tested in desktop.rs), so every platform's come from one place.
#
# Needs: bash, python3, GNU tar, dpkg-deb (no root: --root-owner-group), zip, and rcodesign (as build-local.sh).
# Optional, each format skipped with a note when missing (--fetch-tools gets them, user-locally, into
# $BACKDOWN_PACKAGING_TOOLS, default ~/.local/opt/backdown-packaging; nothing system-wide):
#   appimagetool (github.com/AppImage/appimagetool) and the type-2 runtimes for both architectures
#   wixl and msiinfo (Debian's msitools and wixl packages, unpacked with dpkg-deb -x)
#   mkfs.hfsplus (hfsprogs), and dmg + hfsplus built from github.com/mozilla/libdmg-hfsplus
#   qemu-x86_64 or qemu-aarch64 to run the other architecture's binaries in the checks
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for d in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
  [ -d "$d" ] && case ":$PATH:" in *":$d:"*) ;; *) PATH="$d:$PATH" ;; esac
done
export PATH
tools=${BACKDOWN_PACKAGING_TOOLS:-$HOME/.local/opt/backdown-packaging}
maintainer=${BACKDOWN_MAINTAINER:-backdown.io <orders@backdown.io>}

die() { echo "package-local: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

fetch=0 want=
for a in "$@"; do
  case $a in
    --fetch-tools) fetch=1 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option $a (see --help)" ;;
    *) [ -z "$want" ] || die "one version at most"; want=$a ;;
  esac
done

multiarch=$(dpkg-architecture -qDEB_HOST_MULTIARCH 2>/dev/null || echo "$(uname -m)-linux-gnu")
host_arch=$(uname -m)   # aarch64 or x86_64

fetch_tools() {
  step "fetching packaging tools into $tools (user-local, no root)"
  mkdir -p "$tools/debs" "$tools/bin"
  local deb
  ( cd "$tools/debs" && apt-get download msitools wixl wixl-data libmsi-1.0-0 libgcab-1.0-0 hfsprogs libbz2-dev )
  for deb in "$tools"/debs/*.deb; do
    case $deb in
      */libbz2-dev_*) dpkg-deb -x "$deb" "$tools/bz2" ;;
      */hfsprogs_*) dpkg-deb -x "$deb" "$tools/hfsprogs" ;;
      *) dpkg-deb -x "$deb" "$tools/msitools" ;;
    esac
  done
  rm -f "$tools/bz2/usr/lib/$multiarch/libbz2.so"   # a dangling link here: link the static archive
  curl -fsSL -o "$tools/appimagetool.AppImage" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$host_arch.AppImage"
  chmod +x "$tools/appimagetool.AppImage"
  rm -rf "$tools/appimagetool" && (cd "$tools" && ./appimagetool.AppImage --appimage-extract >/dev/null && mv squashfs-root appimagetool)
  for a in x86_64 aarch64; do
    curl -fsSL -o "$tools/runtime-$a" "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$a"
  done
  if [ ! -d "$tools/libdmg-hfsplus-src" ]; then
    git clone -q --depth 1 https://github.com/mozilla/libdmg-hfsplus.git "$tools/libdmg-hfsplus-src"
  fi
  cmake -S "$tools/libdmg-hfsplus-src" -B "$tools/libdmg-build" -DCMAKE_BUILD_TYPE=Release \
    -DBZIP2_INCLUDE_DIR="$tools/bz2/usr/include" -DBZIP2_LIBRARIES="$tools/bz2/usr/lib/$multiarch/libbz2.a" \
    -DCMAKE_EXE_LINKER_FLAGS="-L$tools/bz2/usr/lib/$multiarch" >/dev/null
  make -s -C "$tools/libdmg-build" -j4 dmg-bin hfsplus
  install -m 755 "$tools/libdmg-build/dmg/dmg" "$tools/libdmg-build/hfs/hfsplus" "$tools/bin/"
  echo "tools in $tools"
}
[ "$fetch" = 1 ] && fetch_tools

command -v python3 >/dev/null || die "python3 is needed"
command -v dpkg-deb >/dev/null || die "dpkg-deb is needed (it comes with dpkg)"
command -v rcodesign >/dev/null || die "rcodesign is needed to sign BackDown.app (see scripts/build-local.sh)"
command -v zip >/dev/null || die "zip is needed"

version=$(cargo metadata --no-deps --format-version 1 --locked |
  python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "backdown-cli"))')
tag=v$version
if [ -n "$want" ]; then tag=v${want#v}; version=${tag#v}; fi
dist=$root/target/dist/$tag
[ -d "$dist" ] || die "no $dist: build it first with scripts/build-local.sh${want:+ $want}"
(cd "$dist" && sha256sum --quiet -c SHA256SUMS.txt) || die "$dist doesn't match its SHA256SUMS.txt"

work=$root/target/package-local/$tag
out=$root/target/packages/$tag
rm -rf "$work" "$out"
mkdir -p "$work" "$out"
built=() skipped=()

# ---- the archives, unpacked
step "unpacking $dist"
for p in linux-x86_64 linux-arm64 macos-universal; do
  mkdir -p "$work/$p" && tar -xzf "$dist/backdown-$tag-$p.tar.gz" -C "$work/$p"
done
mkdir -p "$work/windows-x86_64" && (cd "$work/windows-x86_64" && python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(".")' "$dist/backdown-$tag-windows-x86_64.zip")
case $host_arch in
  aarch64|arm64) host=$work/linux-arm64/backdown; other_qemu=qemu-x86_64 ;;
  x86_64) host=$work/linux-x86_64/backdown; other_qemu=qemu-aarch64 ;;
  *) die "run this on Linux x86-64 or ARM64" ;;
esac
[ "$("$host" --version)" = "backdown $version" ] || die "$host says $("$host" --version), not backdown $version"
"$host" desktop files --help >/dev/null 2>&1 ||
  die "the $tag build predates \`backdown desktop files\`: rebuild with scripts/build-local.sh from this source"
files() { "$host" desktop files --platform "$1" --out "$2" --version "$version" "${@:3}" >/dev/null; }
# run a binary for either Linux architecture (the other one under qemu, if it's here)
runs() { # arch binary args...
  local arch=$1; shift
  case "$arch:$host_arch" in
    x86_64:x86_64|aarch64:aarch64|aarch64:arm64) "$@" ;;
    *) command -v "$other_qemu" >/dev/null && "$other_qemu" "$@" || return 127 ;;
  esac
}
docs() { # into a usr/share/doc/backdown folder
  install -d -m 755 "$1"
  install -m 644 "$work/linux-x86_64/README.txt" "$work/linux-x86_64/THIRD-PARTY-NOTICES.txt" "$1/"
  printf '%s\n' "BackDown $version, (c) backdown.io. https://backdown.io" \
    "This is a pre-release build, provided as-is, without warranty, for evaluation." \
    "The third-party components it contains and their licences are listed in THIRD-PARTY-NOTICES.txt." > "$1/copyright"
  chmod 644 "$1/copyright"
}

# ---- Linux: .deb
debversion=${version//-/\~}   # 0.2.0~alpha.4 sorts before 0.2.0, as a pre-release should
for pair in x86_64:amd64 aarch64:arm64; do
  arch=${pair%:*} debarch=${pair#*:}
  platform=linux-$([ "$arch" = x86_64 ] && echo x86_64 || echo arm64)
  step ".deb for $debarch"
  r=$work/deb-$debarch
  install -D -m 755 "$work/$platform/backdown" "$r/usr/bin/backdown"
  files linux "$r" --exe /usr/bin/backdown
  docs "$r/usr/share/doc/backdown-io"
  # Tab completion for every user, where bash-completion, zsh and fish look for packages' own
  install -d "$r/usr/share/bash-completion/completions" "$r/usr/share/zsh/vendor-completions" "$r/usr/share/fish/vendor_completions.d"
  "$host" completions bash > "$r/usr/share/bash-completion/completions/backdown"
  "$host" completions zsh > "$r/usr/share/zsh/vendor-completions/_backdown"
  "$host" completions fish > "$r/usr/share/fish/vendor_completions.d/backdown.fish"
  chmod 644 "$r"/usr/share/bash-completion/completions/backdown "$r"/usr/share/zsh/vendor-completions/_backdown "$r"/usr/share/fish/vendor_completions.d/backdown.fish
  find "$r" -type d -exec chmod 755 {} +
  install -d -m 755 "$r/DEBIAN"
  cat > "$r/DEBIAN/control" <<EOF
Package: backdown-io
Version: $debversion
Architecture: $debarch
Maintainer: $maintainer
Installed-Size: $(du -sk --apparent-size "$r/usr" | cut -f1)
Section: utils
Priority: optional
Homepage: https://backdown.io
Suggests: chromium | google-chrome-stable | microsoft-edge-stable | brave-browser
Conflicts: backdown
Replaces: backdown
Description: see what's on your drives, what's duplicated, and what's safe to wipe
 BackDown indexes your drives, finds what is duplicated with a SHA-256 hash
 behind every claim, recognises what things are (mail stores, backups, phones,
 systems, repositories, photo libraries) and tells you which drives are safe
 to wipe. Scanning only ever reads your drives; it writes only when you ask it
 to copy, free up a drive or save what a rescue found.
 .
 This is BackDown from backdown.io. It isn't the "backdown" duplicate remover
 in Debian and Ubuntu: both install /usr/bin/backdown, so this one replaces it.
 .
 Open it from the applications menu: it starts its background service and shows
 its window. The command line is \`backdown\`. To keep the service running
 whenever you're logged in: systemctl --user enable --now backdown
EOF
  # the file is named with the tag's version: GitHub rewrites "~" in uploaded names (to "."), which
  # would break the download links and SHA256SUMS.txt; the version inside keeps "~" so apt orders it
  deb=$out/backdown-io_${version}_$debarch.deb
  dpkg-deb --root-owner-group -Zxz --build "$r" "$deb" >/dev/null
  # check: what dpkg says, what's inside, and that it installs (into a throwaway root) and runs
  dpkg-deb --info "$deb" | sed -n 's/^ \(Package\|Version\|Architecture\|Installed-Size\): /  \1: /p'
  dpkg-deb --contents "$deb" | awk '{print "  " $1, $2, $6}' | grep -E 'usr/bin/backdown$|\.desktop$|backdown\.service$|256x256|metainfo|completions/backdown$' || true
  [ "$(dpkg-deb --contents "$deb" | awk '{print $2}' | sort -u)" = "root/root" ] || die "$deb: files not owned by root"
  x=$work/deb-check-$debarch
  dpkg -x "$deb" "$x"
  if v=$(runs "$arch" "$x/usr/bin/backdown" --version 2>/dev/null); then
    [ "$v" = "backdown $version" ] || die "$deb: the installed binary says $v"
    echo "  installed into a throwaway root: $v"
  else
    echo "  installed into a throwaway root (not run: no $other_qemu)"
  fi
  if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$x/usr/share/applications/io.backdown.BackDown.desktop" || die "$deb: the desktop entry doesn't validate"
    echo "  desktop-file-validate: OK"
  fi
  grep -q '^ExecStart=/usr/bin/backdown daemon$' "$x/usr/lib/systemd/user/backdown.service" || die "$deb: the user unit doesn't run /usr/bin/backdown"
  built+=("$(basename "$deb")")
done

# ---- Linux: AppImage
appimagetool=$tools/appimagetool/AppRun
if [ -x "$appimagetool" ] && [ -f "$tools/runtime-x86_64" ] && [ -f "$tools/runtime-aarch64" ]; then
  for arch in x86_64 aarch64; do
    platform=linux-$([ "$arch" = x86_64 ] && echo x86_64 || echo arm64)
    step "AppImage for $arch"
    d=$work/AppDir-$arch
    files appimage "$d"
    install -D -m 755 "$work/$platform/backdown" "$d/usr/bin/backdown"
    docs "$d/usr/share/doc/backdown"
    img=$out/BackDown-$version-$arch.AppImage
    ARCH=$arch VERSION=$version "$appimagetool" --no-appstream --runtime-file "$tools/runtime-$arch" "$d" "$img" \
      > "$work/appimagetool-$arch.log" 2>&1 || { cat "$work/appimagetool-$arch.log" >&2; die "appimagetool failed"; }
    chmod 755 "$img"
    # check: the runtime is for $arch, and the squashfs inside holds the app and runs
    python3 - "$img" "$arch" <<'PY'
import struct, sys
b = open(sys.argv[1], 'rb').read(64)
m = {62: 'x86_64', 183: 'aarch64'}.get(struct.unpack_from('<H', b, 18)[0])
assert b[:4] == b'\x7fELF' and b[8:11] == b'AI\x02', 'not a type-2 AppImage'
assert m == sys.argv[2], f'runtime is {m}'
print(f'  type-2 AppImage, {m} runtime')
PY
    offset=$(runs "$arch" "$img" --appimage-offset 2>/dev/null || true)
    if [ -n "$offset" ] && command -v unsquashfs >/dev/null; then
      x=$work/appimage-check-$arch
      unsquashfs -q -n -o "$offset" -d "$x" "$img" >/dev/null
      for f in AppRun io.backdown.BackDown.desktop io.backdown.BackDown.png usr/bin/backdown; do
        [ -e "$x/$f" ] || die "$img: no $f inside"
      done
      v=$(runs "$arch" "$x/usr/bin/backdown" --version)
      [ "$v" = "backdown $version" ] || die "$img: says $v"
      echo "  squashfs at offset $offset: AppRun, desktop entry, icon, usr/bin/backdown ($v)"
    else
      echo "  contents not checked (the $arch runtime can't run here, or no unsquashfs)"
    fi
    built+=("$(basename "$img")")
  done
else
  skipped+=("AppImage: no appimagetool and runtimes in $tools (scripts/package-local.sh --fetch-tools)")
fi

# ---- macOS: BackDown.app, as .dmg and .zip
step "BackDown.app"
m=$work/mac
files macos "$m"
app=$m/BackDown.app
install -D -m 755 "$work/macos-universal/backdown" "$app/Contents/MacOS/backdown"
install -D -m 644 "$work/macos-universal/README.txt" "$work/macos-universal/THIRD-PARTY-NOTICES.txt" "$app/Contents/Resources/"
# ad-hoc (no Developer ID): Apple silicon runs only signed code; the bundle's seal covers Info.plist and Resources
rcodesign sign --binary-identifier io.backdown.BackDown "$app" > "$work/rcodesign-app.log" 2>&1 ||
  { cat "$work/rcodesign-app.log" >&2; die "rcodesign couldn't sign BackDown.app"; }
[ -f "$app/Contents/_CodeSignature/CodeResources" ] || die "BackDown.app has no CodeResources seal"
echo "  ad-hoc signed; sealed: $(grep -c '<key>' "$app/Contents/_CodeSignature/CodeResources") entries"
(cd "$m" && zip -qry "$out/BackDown-$version-macos.zip" BackDown.app)
zipinfo "$out/BackDown-$version-macos.zip" | awk '/MacOS\/backdown$|Info.plist$|BackDown.icns$/ {print "  " $1, $NF}'
built+=("BackDown-$version-macos.zip")

mkfs_hfsplus=$(command -v mkfs.hfsplus || command -v /usr/sbin/mkfs.hfsplus || true)
[ -n "$mkfs_hfsplus" ] || { [ -x "$tools/hfsprogs/usr/sbin/mkfs.hfsplus" ] && mkfs_hfsplus=$tools/hfsprogs/usr/sbin/mkfs.hfsplus; } || true
if [ -n "$mkfs_hfsplus" ] && [ -x "$tools/bin/hfsplus" ] && [ -x "$tools/bin/dmg" ]; then
  step ".dmg"
  s=$work/dmg-root
  mkdir -p "$s"
  cp -a "$app" "$s/"
  ln -s /Applications "$s/Applications"
  cat > "$s/If BackDown won't open.txt" <<'EOF'
Drag BackDown onto Applications, then open it from there.

This build isn't notarised by Apple yet, so the first time, macOS may say it can't check it:
right-click (or Control-click) BackDown in Applications, choose Open, then Open again.
On macOS 15 and later: System Settings > Privacy & Security > "Open Anyway".
After that it opens like any other app. https://backdown.io
EOF
  img=$work/BackDown.hfs
  size=$(( $(du -sm "$s" | cut -f1) + 16 ))
  truncate -s "${size}M" "$img"
  "$mkfs_hfsplus" -v BackDown "$img" >/dev/null
  "$tools/bin/hfsplus" "$img" -s clone_link addall "$s" >/dev/null
  "$tools/bin/hfsplus" "$img" chmod 755 /BackDown.app/Contents/MacOS/backdown >/dev/null
  dmg=$out/BackDown-$version.dmg
  "$tools/bin/dmg" build "$img" "$dmg" >/dev/null
  # check: a UDIF image ("koly" trailer) that opens back up to the same, signed binary
  python3 - "$dmg" <<'PY'
import sys
b = open(sys.argv[1], 'rb').read()
assert b[-512:-508] == b'koly', 'no UDIF trailer'
print(f'  UDIF disk image, {len(b):,} bytes')
PY
  x=$work/dmg-check
  mkdir -p "$x"
  "$tools/bin/dmg" extract "$dmg" "$x/back.hfs" >/dev/null
  "$tools/bin/hfsplus" "$x/back.hfs" ls / | awk 'NF {print "  / " $0}'
  "$tools/bin/hfsplus" "$x/back.hfs" ls /BackDown.app/Contents/MacOS | awk 'NF {print "  MacOS/ " $0}'
  (cd "$x" && "$tools/bin/hfsplus" back.hfs extract /BackDown.app/Contents/MacOS/backdown backdown >/dev/null)
  cmp -s "$x/backdown" "$app/Contents/MacOS/backdown" || die "$dmg: the binary inside differs from the signed one"
  echo "  the binary inside is the signed one"
  built+=("$(basename "$dmg")")
else
  skipped+=(".dmg: needs mkfs.hfsplus and libdmg-hfsplus's dmg and hfsplus (scripts/package-local.sh --fetch-tools); the .zip has the same app")
fi

# ---- Windows: the setup zip, and the .msi
step "Windows setup zip"
w=$work/win
mkdir -p "$w"
install -m 644 "$work/windows-x86_64/backdown.exe" "$work/windows-x86_64/README.txt" "$work/windows-x86_64/THIRD-PARTY-NOTICES.txt" "$w/"
files windows "$w"
python3 - "$w" <<'PY'
import sys
from pathlib import Path
w = Path(sys.argv[1])
for f in ['install.ps1', 'uninstall.ps1', 'Install BackDown.cmd', 'Uninstall BackDown.cmd']:
    b = (w / f).read_bytes()
    assert b.isascii(), f'{f} is not ASCII (Windows PowerShell would misread it)'
    assert b.count(b'\n') == b.count(b'\r\n'), f'{f} is not CRLF'
print('  scripts: ASCII, CRLF')
PY
setup=$out/BackDown-$version-windows-x86_64-setup.zip
(cd "$w" && zip -q -X "$setup" backdown.exe README.txt THIRD-PARTY-NOTICES.txt backdown.ico install.ps1 uninstall.ps1 "Install BackDown.cmd" "Uninstall BackDown.cmd")
zipinfo -1 "$setup" | sed 's/^/  /'
built+=("$(basename "$setup")")

wixl=$tools/msitools/usr/bin/wixl
if [ -x "$wixl" ]; then
  step ".msi"
  export LD_LIBRARY_PATH=$tools/msitools/usr/lib/$multiarch${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
  msi=$out/BackDown-$version-x64.msi
  (cd "$w" && "$wixl" --arch x64 -o "$msi" backdown.wxs) || die "wixl failed"
  info=$tools/msitools/usr/bin/msiinfo
  # check: the tables Windows Installer reads, and that the program inside is the built one
  "$info" suminfo "$msi" | sed -n 's/^\(Title\|Subject\|Author\|Template\|Word count\): /  \1: /p'
  prop() { "$info" export "$msi" Property | awk -F'\t' -v k="$1" '$1 == k {print $2}'; }
  echo "  ProductVersion $(prop ProductVersion), ALLUSERS='$(prop ALLUSERS)', MSIINSTALLPERUSER='$(prop MSIINSTALLPERUSER)', UpgradeCode $(prop UpgradeCode)"
  "$info" export "$msi" Directory | awk -F'\t' 'NR > 3 {print "  Directory " $1 " <- " $2 " (" $3 ")"}'
  "$info" export "$msi" Shortcut | awk -F'\t' 'NR > 3 {print "  Shortcut " $3 ": " $5 " " $6}'
  "$info" export "$msi" Environment | awk -F'\t' 'NR > 3 {print "  Environment " $2 " = " $3}'
  "$info" export "$msi" Registry | awk -F'\t' 'NR > 3 {print "  Registry HKCU\\" $3 " " $4}'
  x=$work/msi-check
  mkdir -p "$x"
  "$tools/msitools/usr/bin/msiextract" -C "$x" "$msi" >/dev/null
  found=$(find "$x" -name backdown.exe | head -1)
  [ -n "$found" ] && cmp -s "$found" "$w/backdown.exe" || die "$msi: backdown.exe inside differs from the built one"
  echo "  backdown.exe inside is the built one (${found#$x/})"
  built+=("$(basename "$msi")")
else
  skipped+=(".msi: no wixl in $tools (scripts/package-local.sh --fetch-tools); the setup zip does the same install")
fi

(cd "$out" && sha256sum -- * > SHA256SUMS.txt)
step "done: $out"
(cd "$out" && ls -l --time-style=+ -- * | awk '{printf "  %12s  %s\n", $5, $NF}')
for s in "${skipped[@]}"; do echo "skipped: $s"; done
echo
echo "Built ${#built[@]} packages. Nothing was published: these are for trying out, or for attaching to a release by hand."
