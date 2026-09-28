#!/usr/bin/env bash
# Build the CLI release on this machine instead of GitHub Actions: the same four archives and SHA256SUMS.txt
# as the `release` artifact of .github/workflows/release.yml, in the folder scripts/publish-release.sh
# publishes from. Use it when Actions can't run (billing, outage) or to try a release build before tagging.
# It tests, builds, checks and packages; it never tags, pushes, publishes or uploads anything.
#
#   scripts/build-local.sh                   # the version in Cargo.toml, named v<version>
#   scripts/build-local.sh v0.2.0-alpha.5    # must match Cargo.toml (the binary's --version comes from it)
#   scripts/build-local.sh --no-test         # skip `cargo test` (builds, checks and smoke tests still run)
#
# Output, rebuilt from scratch on every run (cargo's cache keeps reruns quick):
#   target/dist/<tag>/backdown-<tag>-{linux-x86_64,linux-arm64,macos-universal}.tar.gz,
#   target/dist/<tag>/backdown-<tag>-windows-x86_64.zip and SHA256SUMS.txt
# Publish them, once the built commit is tagged <tag>, with: scripts/publish-release.sh <tag> --local
#
# Runs on Linux (ARM64 or x86-64), no root needed. Needs, user-local:
#   rustup + cargo (the five targets are added here), zig 0.16 (ziglang.org tarball, e.g. in ~/.local/opt with
#   ~/.local/bin/zig linked to it, or `pip install ziglang` as CI does), cargo-zigbuild (`cargo install --locked
#   cargo-zigbuild`), rcodesign (apple-codesign, a prebuilt binary from github.com/indygreg/apple-platform-rs;
#   ad-hoc signs the macOS binary), python3, GNU tar, 7z or zip. Optional: qemu-x86_64 (qemu-user) to run the
#   x86-64 Linux build on an ARM host.
#
# How the result differs from CI's:
#   - Linux: nothing; the same static musl build through cargo-zigbuild and zig.
#   - macOS: linked by zig instead of Apple's ld, merged into one universal binary by cargo-zigbuild instead of
#     lipo, and ad-hoc signed by rcodesign instead of codesign. The minimum macOS stays 11.0 as in CI (zig
#     alone would stamp 13.0, hence the two small linker wrappers below).
#   - Windows: target x86_64-pc-windows-gnu (mingw-w64 via zig, on the Universal CRT) instead of
#     x86_64-pc-windows-msvc, which needs Microsoft's toolchain. Same Rust code and Win32 calls; unlike the
#     MSVC build it doesn't need VCRUNTIME140.dll, and its main thread reserves 16 MB of stack, not 1 MB.
#   - Tests run on this machine's platform only (CI tests on Linux x86-64, macOS and Windows), but the Linux
#     binaries are smoke tested here on both architectures (ARM64 natively, x86-64 under qemu).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for d in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
  [ -d "$d" ] && case ":$PATH:" in *":$d:"*) ;; *) PATH="$d:$PATH" ;; esac
done
export PATH

die() { echo "build-local: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

run_tests=1 want=
for a in "$@"; do
  case $a in
    --no-test) run_tests= ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option $a (see --help)" ;;
    *) [ -z "$want" ] || die "one version at most"; want=$a ;;
  esac
done

need() { command -v "$1" >/dev/null || die "$1 not found: $2"; }
need cargo "install Rust with rustup; cargo lives in ~/.cargo/bin"
need rustup "install Rust with rustup"
need zig "unpack https://ziglang.org/download/ into ~/.local/opt and link ~/.local/bin/zig to it"
need cargo-zigbuild "cargo install --locked cargo-zigbuild"
need rcodesign "get apple-codesign from https://github.com/indygreg/apple-platform-rs/releases (or cargo install apple-codesign)"
need python3 "install python3"
command -v 7z >/dev/null || command -v zip >/dev/null || die "7z or zip is needed for the Windows archive"
tar --version 2>/dev/null | grep -q 'GNU tar' || die "GNU tar is needed"

# the version CI would ship: the tag is v<Cargo.toml version>, and the binary reports that version
version=$(cargo metadata --no-deps --format-version 1 --locked |
  python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "backdown-cli"))')
tag=v$version
[ -z "$want" ] || [ "${want#v}" = "$version" ] ||
  die "Cargo.toml says $version, not ${want#v}: bump [workspace.package] version (the binary reports it) and commit first"

# what source this is; warnings, not errors, so a trial build of an untagged commit still works
warnings=()
source_desc="$root (not a git checkout)"
built_from=unknown   # recorded next to the output; publish-release.sh --local checks it against the tag
epoch=${SOURCE_DATE_EPOCH:-}
if git -C "$root" rev-parse --git-dir >/dev/null 2>&1; then
  head=$(git -C "$root" rev-parse HEAD)
  built_from=$head
  source_desc="commit ${head:0:12} ($(git -C "$root" log -1 --format=%s | cut -c1-60))"
  epoch=${epoch:-$(git -C "$root" log -1 --format=%ct)}
  if tagged=$(git -C "$root" rev-parse -q --verify "refs/tags/$tag^{commit}"); then
    [ "$tagged" = "$head" ] ||
      warnings+=("tag $tag is at ${tagged:0:12}, not this commit: these files are NOT $tag's source; don't publish them as $tag")
  else
    warnings+=("no tag $tag here yet: tag the built commit before publishing")
  fi
  if [ -n "$(git -C "$root" status --porcelain -- crates Cargo.toml Cargo.lock .cargo 'rust-toolchain*')" ]; then
    warnings+=("uncommitted changes in crates/ or Cargo.* are built in: no commit reproduces these files")
    built_from=$head-dirty
  fi
fi
epoch=${epoch:-$(date +%s)}
show_warnings() { local w; for w in "${warnings[@]}"; do echo "WARNING: $w" >&2; done; }

host=$(rustc -vV | sed -n 's/^host: //p')
echo "backdown $version ($tag) from $source_desc"
echo "host $host; $(rustc -V); zig $(zig version); $(cargo-zigbuild --version); $(rcodesign --version)"
show_warnings

step "rustup targets"
targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-gnu)
rustup target add "${targets[@]}" 2>&1 | { grep -v 'is up to date' || true; }
echo "${targets[*]}"

if [ -n "$run_tests" ]; then
  step "cargo test --locked --release ($host)"
  cargo test --locked --release
fi

work=$root/target/build-local/$tag   # one folder per platform, like CI's dist/
dist=$root/target/dist/$tag          # what publish-release.sh publishes
rm -rf "$work" "$dist" "$dist.built-from"
mkdir -p "$work" "$dist"
start=$(date +%s)

build() { # rust target
  step "cargo zigbuild --locked --release --target $1"
  cargo zigbuild --locked --release --target "$1"
}
stage() { # platform binary
  mkdir -p "$work/$1" && chmod 755 "$work/$1"
  install -m 755 "$2" "$work/$1/"
}

build x86_64-unknown-linux-musl
stage linux-x86_64 target/x86_64-unknown-linux-musl/release/backdown
build aarch64-unknown-linux-musl
stage linux-arm64 target/aarch64-unknown-linux-musl/release/backdown

# macOS 11 minimum, as CI's MACOSX_DEPLOYMENT_TARGET: rustc takes it from the environment, but cargo-zigbuild
# hands zig a bare `<arch>-macos-none` target and zig 0.16 then stamps 13.0 on the binary; these wrappers
# give zig the versioned target instead (cargo-zigbuild keeps a C compiler or linker set in the environment).
# When linking they also leave header room for the code signature, which zig writes for arm64 only; without
# it rcodesign can't sign the x86-64 slice ("insufficient room to write code signature load command").
macos_min=11.0
zigbuild=$(command -v cargo-zigbuild)
(
  rm -f "$root"/target/build-local/zigcc-*
  for arch in aarch64 x86_64; do
    wrapper=$(cat <<EOF
#!/bin/sh
# zig cc for $arch macOS $macos_min+, written by scripts/build-local.sh
case " \$* " in *" -c "*) ;; *) set -- -Wl,-headerpad_max_install_names "\$@" ;; esac
exec "$zigbuild" zig cc -- -g -fno-sanitize=all -target $arch-macos.$macos_min-none "\$@"
EOF
)
    # named after its content: cargo relinks when the linker's path changes, not when the file does
    w=$root/target/build-local/zigcc-$arch-macos-$(printf '%s' "$wrapper" | sha256sum | cut -c1-12).sh
    printf '%s\n' "$wrapper" > "$w"
    chmod +x "$w"
    export "CC_${arch}_apple_darwin=$w" "CARGO_TARGET_${arch^^}_APPLE_DARWIN_LINKER=$w"
  done
  export MACOSX_DEPLOYMENT_TARGET=$macos_min
  build universal2-apple-darwin   # both Apple targets, merged into one fat binary by cargo-zigbuild
)
stage macos-universal target/universal2-apple-darwin/release/backdown
step "rcodesign sign (ad-hoc, both slices; like CI's codesign --force --sign -)"
# required to run on Apple silicon at all; not a Developer ID signature
rcodesign sign --binary-identifier backdown "$work/macos-universal/backdown" > "$work/rcodesign.log" 2>&1 ||
  { cat "$work/rcodesign.log" >&2; die "rcodesign failed"; }
echo "signed $work/macos-universal/backdown"

build x86_64-pc-windows-gnu
stage windows-x86_64 target/x86_64-pc-windows-gnu/release/backdown.exe
built=$(date +%s)

step "check the binaries (format, architecture, static linking, macOS minimum and signature, Windows DLLs)"
python3 - "$work" "$macos_min" <<'PY'
import hashlib, struct, sys
from pathlib import Path
work, macos_min = Path(sys.argv[1]), sys.argv[2]
failed = []

def fail(msg):
    failed.append(msg)
    print('  FAIL: ' + msg)

def elf(path, machine):
    b = path.read_bytes()
    if b[:4] != b'\x7fELF' or b[4] != 2:
        return fail(f'{path}: not a 64-bit ELF file')
    e_machine = struct.unpack_from('<H', b, 18)[0]
    phoff, = struct.unpack_from('<Q', b, 32)
    phentsize, phnum = struct.unpack_from('<HH', b, 54)
    types = [struct.unpack_from('<I', b, phoff + i * phentsize)[0] for i in range(phnum)]
    arch = {62: 'x86-64', 183: 'arm64'}.get(e_machine, hex(e_machine))
    static = 3 not in types and 2 not in types   # no PT_INTERP, no PT_DYNAMIC
    print(f'  {path.parent.name}: ELF {arch}, {"static" if static else "DYNAMIC"}, {len(b):,} bytes')
    if arch != machine: fail(f'{path}: {arch}, expected {machine}')
    if not static: fail(f'{path}: dynamically linked')

def version(v):
    return f'{v >> 16}.{(v >> 8) & 0xff}' + (f'.{v & 0xff}' if v & 0xff else '')

def macho_slice(b, name):
    magic, cputype, _, _, ncmds, _, _, _ = struct.unpack_from('<8I', b, 0)
    if magic != 0xfeedfacf: return fail(f'{name}: not a 64-bit Mach-O slice')
    minos, sig = None, None
    off = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', b, off)
        if cmd == 0x32: minos = version(struct.unpack_from('<I', b, off + 12)[0])     # LC_BUILD_VERSION
        if cmd == 0x24: minos = version(struct.unpack_from('<I', b, off + 8)[0])      # LC_VERSION_MIN_MACOSX
        if cmd == 0x1d: sig = struct.unpack_from('<II', b, off + 8)                   # LC_CODE_SIGNATURE
        off += size
    if minos != macos_min: fail(f'{name}: minimum macOS {minos}, expected {macos_min}')
    if not sig: return fail(f'{name}: no code signature (Apple silicon kills unsigned arm64 binaries)')
    # check the signature the way the kernel does on page-in: every page's hash must match the CodeDirectory
    dataoff, _ = sig
    magic, _, count = struct.unpack_from('>III', b, dataoff)
    if magic != 0xfade0cc0: return fail(f'{name}: code signature is not an embedded SuperBlob')
    notes = []
    for i in range(count):
        slot, boff = struct.unpack_from('>II', b, dataoff + 12 + 8 * i)
        cd = dataoff + boff
        if struct.unpack_from('>I', b, cd)[0] != 0xfade0c02: continue                # not a CodeDirectory
        _, _, _, flags, hash_off, ident_off, _, nslots, limit = struct.unpack_from('>9I', b, cd)
        hash_size, hash_type, _, page_log = struct.unpack_from('>4B', b, cd + 36)
        algo = {1: 'sha1', 2: 'sha256', 3: 'sha256', 4: 'sha384'}[hash_type]
        page = 1 << page_log if page_log else limit
        bad = sum(hashlib.new(algo, b[p * page:min((p + 1) * page, limit)]).digest()[:hash_size]
                  != b[cd + hash_off + p * hash_size:cd + hash_off + (p + 1) * hash_size] for p in range(nslots))
        ident = b[cd + ident_off:b.index(b'\0', cd + ident_off)].decode()
        kind = ('ad-hoc' if flags & 0x2 else 'signed') + (', linker-signed' if flags & 0x20000 else '')
        notes.append(f'{kind} "{ident}" {algo}: {nslots - bad}/{nslots} pages match')
        if bad: fail(f'{name}: {bad} of {nslots} pages don\'t match their {algo} hash')
        if not flags & 0x2: fail(f'{name}: not an ad-hoc signature')
    if not notes: fail(f'{name}: code signature has no CodeDirectory')
    print(f'    {name}: macOS {minos}+, ' + '; '.join(notes))

def macho(path):
    b = path.read_bytes()
    magic, n = struct.unpack_from('>II', b, 0)
    if magic != 0xcafebabe: return fail(f'{path}: not a universal (fat) Mach-O binary')
    arches = {}
    for i in range(n):
        cputype, _, off, size, _ = struct.unpack_from('>5I', b, 8 + 20 * i)
        arches[{0x01000007: 'x86_64', 0x0100000c: 'arm64'}.get(cputype, hex(cputype))] = b[off:off + size]
    print(f'  {path.parent.name}: Mach-O universal [{" ".join(arches)}], {len(b):,} bytes')
    if sorted(arches) != ['arm64', 'x86_64']: fail(f'{path}: slices {sorted(arches)}, expected arm64 and x86_64')
    for name, s in arches.items(): macho_slice(s, name)

def pe(path):
    b = path.read_bytes()
    pe_off, = struct.unpack_from('<I', b, 0x3c)
    if b[pe_off:pe_off + 4] != b'PE\0\0': return fail(f'{path}: not a PE file')
    machine, nsect, _, _, _, opt_size, _ = struct.unpack_from('<HHIIIHH', b, pe_off + 4)
    opt = pe_off + 24
    if struct.unpack_from('<H', b, opt)[0] != 0x20b: return fail(f'{path}: not PE32+')
    subsystem, = struct.unpack_from('<H', b, opt + 68)
    stack, = struct.unpack_from('<Q', b, opt + 72)
    imp_rva, _ = struct.unpack_from('<II', b, opt + 112 + 8)
    sections = [struct.unpack_from('<IIII', b, opt + opt_size + 40 * i + 8) for i in range(nsect)]
    def at(rva):
        for vsize, va, rawsize, raw in sections:
            if va <= rva < va + max(vsize, rawsize): return rva - va + raw
        raise ValueError(hex(rva))
    dlls, d = [], at(imp_rva)
    while any(struct.unpack_from('<5I', b, d)):
        name = at(struct.unpack_from('<5I', b, d)[3])
        dlls.append(b[name:b.index(b'\0', name)].decode())
        d += 20
    dlls = sorted({x.lower() for x in dlls})
    arch = {0x8664: 'x86-64'}.get(machine, hex(machine))
    kind = {3: 'console'}.get(subsystem, f'subsystem {subsystem}')
    print(f'  {path.parent.name}: PE32+ {arch} {kind}, {stack >> 20} MB stack, {len(b):,} bytes, imports: {" ".join(dlls)}')
    if arch != 'x86-64' or subsystem != 3: fail(f'{path}: expected an x86-64 console program')
    # everything must ship with Windows 10+: no MinGW runtime DLLs (libgcc_s, libwinpthread) and no VC++ ones
    extra = [x for x in dlls if x.startswith(('lib', 'vcruntime', 'msvcp', 'msvcr')) or x.startswith('ucrtbase')]
    if extra: fail(f'{path}: needs DLLs Windows doesn\'t ship: {" ".join(extra)}')

elf(work / 'linux-x86_64' / 'backdown', 'x86-64')
elf(work / 'linux-arm64' / 'backdown', 'arm64')
macho(work / 'macos-universal' / 'backdown')
pe(work / 'windows-x86_64' / 'backdown.exe')
sys.exit(1 if failed else 0)
PY

# CI's smoke test, on whichever binaries this machine can run
smoke() { # platform, then the command to run it with (empty: natively)
  local p=$1; shift
  local bin=$work/$p/backdown dir=$work/smoke-$p out
  [ -f "$bin" ] || bin=$bin.exe
  mkdir -p "$dir/a" "$dir/b" && echo same > "$dir/a/x" && echo same > "$dir/b/x"
  out=$("$@" "$bin" --version) || die "$p: --version failed"
  [ "$out" = "backdown $version" ] || die "$p: --version says '$out', expected 'backdown $version'"
  (cd "$dir" && "$@" "$bin" --db smoke.db index a b && "$@" "$bin" --db smoke.db check a) > "$dir.log" 2>&1 ||
    { cat "$dir.log" >&2; die "$p: smoke test (index, check) failed"; }
  echo "  $p: $out; index + check OK${1:+ (under $1)}"
  rm -rf "$dir" "$dir.log"
}
step "smoke test (--version, index, check)"
arch=$(uname -m)
case $arch in
  aarch64|arm64)
    smoke linux-arm64
    if command -v qemu-x86_64 >/dev/null; then smoke linux-x86_64 qemu-x86_64
    else echo "  linux-x86_64: not run (no qemu-x86_64; inspected only)"; fi ;;
  x86_64)
    smoke linux-x86_64
    if command -v qemu-aarch64 >/dev/null; then smoke linux-arm64 qemu-aarch64
    else echo "  linux-arm64: not run (no qemu-aarch64; inspected only)"; fi
    if command -v wine >/dev/null; then smoke windows-x86_64 wine
    else echo "  windows-x86_64: not run (no wine; inspected only)"; fi ;;
esac
echo "  macos-universal: can't run on Linux (inspected only)"
[ "$arch" = x86_64 ] || echo "  windows-x86_64: can't run on this $arch host (inspected only)"

step "package into $dist"
for p in linux-x86_64 linux-arm64 macos-universal windows-x86_64; do
  install -m 644 crates/backdown-cli/README-release.txt "$work/$p/README.txt"
  install -m 644 crates/backdown-cli/THIRD-PARTY-NOTICES.txt "$work/$p/"
  base=backdown-$tag-$p
  if [ "$p" = windows-x86_64 ]; then
    # CI's Windows checkout turns the text files' line endings into CRLF; match it
    sed -i 's/\r*$/\r/' "$work/$p/README.txt" "$work/$p/THIRD-PARTY-NOTICES.txt"
    touch -d "@$epoch" "$work/$p"/*
    if command -v 7z >/dev/null; then
      (cd "$work/$p" && 7z a -tzip -mtc=off "$dist/$base.zip" . >/dev/null)
    else
      (cd "$work/$p" && zip -q -X -r "$dist/$base.zip" .)
    fi
  else
    # same layout as CI's `tar -C dist -czf`; fixed owner, order and times so a rebuild gives the same file
    tar -C "$work/$p" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -cf - . | gzip -n > "$dist/$base.tar.gz"
  fi
done
(cd "$dist" && sha256sum -- * > SHA256SUMS.txt)
echo "$built_from" > "$dist.built-from"   # beside the folder, not in it: everything in it gets published
done_at=$(date +%s)

step "done: $dist ($((built - start))s to build, $((done_at - start))s in all)"
(cd "$dist" && ls -l --time-style=+ -- * | awk '{printf "  %12s  %s\n", $5, $NF}' && cat SHA256SUMS.txt)
show_warnings
echo
echo "Nothing was published. After tagging the built commit $tag: scripts/publish-release.sh $tag --local"
