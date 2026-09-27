#!/usr/bin/env bash
# Publish a tagged CLI build to the public BuenGenio/backdown-releases repo and point the site's download
# cards at it. The `release` workflow for the tag must have finished; it builds and checksums everything,
# so this only moves verified files. Deploying the site is a separate step (marketing/README.md).
#
#   scripts/publish-release.sh v0.2.0-alpha.1
#
# Needs gh (logged in with access to both repos) and python3.
set -euo pipefail
tag=${1:?usage: scripts/publish-release.sh vX.Y.Z}
src=BuenGenio/BackDown
dst=BuenGenio/backdown-releases
root=$(cd "$(dirname "$0")/.." && pwd)

run=$(gh run list --repo "$src" --workflow release.yml --branch "$tag" --status success --limit 1 --json databaseId --jq '.[0].databaseId // empty')
[ -n "$run" ] || { echo "no successful release workflow run for $tag yet" >&2; exit 1; }

dist="$root/target/dist/$tag"
rm -rf "$dist" && mkdir -p "$dist"
gh run download "$run" --repo "$src" --name release --dir "$dist"
(cd "$dist" && sha256sum --quiet -c SHA256SUMS.txt) || { echo "checksum mismatch in $dist" >&2; exit 1; }

pre=(); [[ $tag == *-* ]] && pre=(--prerelease)
gh release create "$tag" "$dist"/* --repo "$dst" --title "BackDown $tag" "${pre[@]}" --notes "$(cat <<NOTES
Command-line preview of BackDown: find out what's on your drives, what's duplicated, and which drives are safe to wipe.

| Platform | Download |
|---|---|
| Linux x86-64 (static) | \`backdown-$tag-linux-x86_64.tar.gz\` |
| Linux ARM64 (static) | \`backdown-$tag-linux-arm64.tar.gz\` |
| macOS, Apple silicon + Intel | \`backdown-$tag-macos-universal.tar.gz\` |
| Windows x86-64 | \`backdown-$tag-windows-x86_64.zip\` |

\`\`\`sh
backdown index /media/old-drive /media/archive
backdown check /media/old-drive        # exit 0 = safe to wipe, 2 = copy first
backdown dupes --min-size 10M
\`\`\`

Verify downloads against \`SHA256SUMS.txt\`. Builds aren't signed yet: see the README for the one-time macOS and Windows steps. Provided as-is, without warranty, for evaluation.
NOTES
)"

python3 - "$tag" "$dist" "$root/marketing/downloads.json" <<'PY'
import json, sys
from pathlib import Path
tag, dist, path = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
sums = {name: digest for digest, name in (l.split() for l in (dist / 'SHA256SUMS.txt').read_text().splitlines())}
base = f'https://github.com/BuenGenio/backdown-releases/releases/download/{tag}/'
files = {'linux': [('x86-64', 'linux-x86_64.tar.gz'), ('ARM64', 'linux-arm64.tar.gz')],
         'macos': [('Apple silicon + Intel', 'macos-universal.tar.gz')],
         'windows': [('x86-64', 'windows-x86_64.zip')]}
data = json.loads(path.read_text())
for p in data['platforms']:
    names = [(label, f'backdown-{tag}-{suffix}') for label, suffix in files[p['id']]]
    p.update(status='available', version=tag, files=[{'label': l, 'url': base + n, 'sha256': sums[n]} for l, n in names])
path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')
print(f'{path}: {sum(len(p["files"]) for p in data["platforms"])} downloads for {tag}')
PY
python3 "$root/marketing/build.py" | tail -1
echo "published https://github.com/$dst/releases/tag/$tag; now deploy the site (cd marketing && npx wrangler deploy)"
