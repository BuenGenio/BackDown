#!/bin/bash
# The whole pipeline: invented drives -> two BackDown indexes -> the demo daemon -> a recording of its UI's API
# -> public/demo/. Needs: a release build with the demo hooks (cargo build --release -p backdown-cli), Firefox,
# python3 with Pillow, LibreOffice (soffice) for the office documents, node with puppeteer-core (npm install here).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/env.sh"
echo "== drives -> $D"; python3 -I "$HERE/gen.py" "$D"
echo "== indexes"; "$HERE/seed.sh" 2>&1 | tail -20
echo "== daemon"; "$HERE/daemon.sh" start
TOKEN="$("$HERE/daemon.sh" token)"; BASE="$("$HERE/daemon.sh" url)"
echo "== recording"; rm -rf "$D/rec"; node "$HERE/record.mjs" "$BASE" "$TOKEN" "$D/rec"
echo "== bundle"; node "$HERE/bundle.mjs" "$BASE" "$TOKEN" "$D/rec"
"$HERE/daemon.sh" stop
echo "== site"; (cd "$HERE/../.." && python3 build.py >/dev/null && python3 tools/check-links.py | tail -1)
