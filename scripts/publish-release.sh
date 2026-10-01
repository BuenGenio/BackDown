#!/usr/bin/env bash
# Publish a tagged CLI build to the public BuenGenio/backdown-releases repo and point the site's download
# cards at it. The `release` workflow for the tag must have finished (or, with --local, scripts/build-local.sh
# must have built the tag on this machine); either builds and checksums everything, so this only moves
# verified files. Deploying the site is a separate step (marketing/README.md).
#
#   scripts/publish-release.sh v0.2.0-alpha.1
#   scripts/publish-release.sh v0.2.0-alpha.5 --local   # publish what scripts/build-local.sh built instead
#   scripts/publish-release.sh v1.0.0-beta.1 --local --with-packages   # plus scripts/package-local.sh's installers
#
# Needs gh (logged in with access to both repos) and python3.
set -euo pipefail
tag=${1:?usage: scripts/publish-release.sh vX.Y.Z [--local] [--with-packages]}
local_build= with_packages=
for opt in "${@:2}"; do
  case $opt in --local) local_build=1 ;; --with-packages) with_packages=1 ;; *) echo "unknown option $opt" >&2; exit 1 ;; esac
done
src=BuenGenio/BackDown
dst=BuenGenio/backdown-releases
root=$(cd "$(dirname "$0")/.." && pwd)

dist="$root/target/dist/$tag"
if [ -n "$local_build" ]; then
  # built on this machine by scripts/build-local.sh (Actions unavailable): publish exactly that folder
  want=("backdown-$tag-linux-x86_64.tar.gz" "backdown-$tag-linux-arm64.tar.gz" "backdown-$tag-macos-universal.tar.gz"
        "backdown-$tag-windows-x86_64.zip" SHA256SUMS.txt)
  [ "$(cd "$dist" 2>/dev/null && ls | sort)" = "$(printf '%s\n' "${want[@]}" | sort)" ] ||
    { echo "$dist should hold exactly ${want[*]}: run scripts/build-local.sh $tag" >&2; exit 1; }
  (cd "$dist" && sha256sum --quiet -c SHA256SUMS.txt) || { echo "checksum mismatch in $dist" >&2; exit 1; }
  built=$(cat "$dist.built-from" 2>/dev/null || echo unknown)
  tagged=$(git -C "$root" rev-parse -q --verify "refs/tags/$tag^{commit}" || echo "missing here")
  [ "$built" = "$tagged" ] ||
    { echo "$dist was built from $built, but tag $tag is $tagged: tag the built commit, or rebuild at the tag" >&2; exit 1; }
  echo "publishing the local build of $tag (${tagged:0:12}) in $dist"
else
  rm -rf "$dist" && mkdir -p "$dist"
  run=$(gh run list --repo "$src" --workflow release.yml --branch "$tag" --status success --limit 1 --json databaseId --jq '.[0].databaseId // empty')
  if [ -n "$run" ]; then
    gh run download "$run" --repo "$src" --name release --dir "$dist"
    (cd "$dist" && sha256sum --quiet -c SHA256SUMS.txt) || { echo "checksum mismatch in $dist" >&2; exit 1; }
  else
    # every build job passed but the bundle job didn't run (e.g. Actions was out of minutes): do its one
    # step here, from the per-platform artifacts
    run=$(gh run list --repo "$src" --workflow release.yml --branch "$tag" --limit 1 --json databaseId --jq '.[0].databaseId // empty')
    [ -n "$run" ] || { echo "no release workflow run for $tag yet (Actions down? build it here: scripts/build-local.sh)" >&2; exit 1; }
    platforms=(linux-x86_64 linux-arm64 macos-universal windows-x86_64)
    for p in "${platforms[@]}"; do
      gh run download "$run" --repo "$src" --name "$p" --dir "$dist" \
        || { echo "run $run has no finished $p build; wait for it or rerun it" >&2; exit 1; }
    done
    (cd "$dist" && sha256sum -- * > SHA256SUMS.txt)
    echo "bundled run $run's ${#platforms[@]} builds locally"
  fi
fi

# with --with-packages: scripts/package-local.sh's installers go up beside the archives, under one
# SHA256SUMS.txt that covers everything (`backdown update` picks archives by exact name, so extra files
# can't confuse it)
upload=$dist
if [ -n "$with_packages" ]; then
  pkgs="$root/target/packages/$tag"
  [ -f "$pkgs/SHA256SUMS.txt" ] || { echo "no installers in $pkgs: run scripts/package-local.sh $tag" >&2; exit 1; }
  (cd "$pkgs" && sha256sum --quiet -c SHA256SUMS.txt) || { echo "checksum mismatch in $pkgs" >&2; exit 1; }
  upload="$root/target/release-upload/$tag"
  rm -rf "$upload" && mkdir -p "$upload"
  for f in "$dist"/* "$pkgs"/*; do [ "$(basename "$f")" = SHA256SUMS.txt ] || ln "$f" "$upload/" 2>/dev/null || cp "$f" "$upload/"; done
  cat "$dist/SHA256SUMS.txt" "$pkgs/SHA256SUMS.txt" > "$upload/SHA256SUMS.txt"
  (cd "$upload" && sha256sum --quiet -c SHA256SUMS.txt) || { echo "combined checksums don't match" >&2; exit 1; }
fi
# GitHub rewrites other characters in asset names ("~" becomes "."), which would break the site's
# links and SHA256SUMS.txt: refuse such a name rather than publish it
for f in "$upload"/*; do
  n=$(basename "$f")
  [[ $n =~ ^[A-Za-z0-9._-]+$ ]] || { echo "$n: GitHub would rename it (only letters, digits, . _ - survive); rename it first" >&2; exit 1; }
done
notes="$root/scripts/release-notes/$tag.md"
[ -f "$notes" ] || { echo "write the release notes first: $notes" >&2; exit 1; }

pre=(); [[ $tag == *-* ]] && pre=(--prerelease)
gh release create "$tag" "$upload"/* --repo "$dst" --title "BackDown $tag" "${pre[@]}" --notes-file "$notes"

python3 - "$tag" "$upload" "$root/marketing/downloads.json" <<'PY'
import fnmatch, json, sys
from pathlib import Path
tag, dist, path = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
sums = {name: digest for digest, name in (l.split() for l in (dist / 'SHA256SUMS.txt').read_text().splitlines())}
base = f'https://github.com/BuenGenio/backdown-releases/releases/download/{tag}/'
# installers first (for everyone), then the command-line archives (kind "cli": smaller buttons)
files = {'linux': [('.deb · x86-64', 'backdown-io_*_amd64.deb', None), ('.deb · ARM64', 'backdown-io_*_arm64.deb', None),
                   ('AppImage · x86-64', 'BackDown-*-x86_64.AppImage', None), ('AppImage · ARM64', 'BackDown-*-aarch64.AppImage', None),
                   ('CLI · x86-64', f'backdown-{tag}-linux-x86_64.tar.gz', 'cli'), ('CLI · ARM64', f'backdown-{tag}-linux-arm64.tar.gz', 'cli')],
         'macos': [('.dmg', 'BackDown-*.dmg', None), ('.app · .zip', 'BackDown-*-macos.zip', None),
                   ('CLI', f'backdown-{tag}-macos-universal.tar.gz', 'cli')],
         'windows': [('.msi', 'BackDown-*-x64.msi', None), ('setup · .zip', 'BackDown-*-windows-x86_64-setup.zip', None),
                     ('CLI', f'backdown-{tag}-windows-x86_64.zip', 'cli')]}
formats = {'linux': '.deb · AppImage · CLI', 'macos': '.dmg · CLI · universal', 'windows': '.msi · setup · CLI'}
data = json.loads(path.read_text())
for p in data['platforms']:
    out = []
    for label, pattern, kind in files[p['id']]:
        match = sorted(n for n in sums if fnmatch.fnmatch(n, pattern))
        if not match:
            continue  # an installer that wasn't built this time
        f = {'label': label, 'url': base + match[0], 'sha256': sums[match[0]]}
        if kind:
            f['kind'] = kind
        out.append(f)
    if any('kind' not in f for f in out):
        p['formats'] = formats[p['id']]
    p.update(status='available', version=tag, files=out)
path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')
print(f'{path}: {sum(len(p["files"]) for p in data["platforms"])} downloads for {tag}')
PY
python3 "$root/marketing/build.py" | tail -1
echo "published https://github.com/$dst/releases/tag/$tag; now deploy the site (cd marketing && npx wrangler deploy)"
