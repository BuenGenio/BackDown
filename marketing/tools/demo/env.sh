# The docs demo's own BackDown: its own data folder, runtime folder and PATH shims, never the user's index.
# Source it: `. tools/demo/env.sh`. DEMO_HOME is where the invented drives and both indexes live.
DEMO_TOOLS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export DEMO_TOOLS
export D="${DEMO_HOME:-/tmp/backdown-demo}"
export BD="${BACKDOWN_BIN:-$(cd "$DEMO_TOOLS/../../.." && pwd)/target/release/backdown}"
export DEMO_RUN="${DEMO_RUN:-/tmp/bd-demo-run}"   # Unix sockets need a short path
export XDG_DATA_HOME="$D/xdg/data"
export XDG_RUNTIME_DIR="$DEMO_RUN"
export BACKDOWN_NO_UPDATE_CHECK=1
export BACKDOWN_FAKE_HEALTH="$D/health-desktop.json"
export BACKDOWN_FAKE_VOLUMES="$D/volumes-desktop.json"
export BACKDOWN_HOSTNAME=alex-desktop
export PATH="$DEMO_TOOLS/shim:$PATH"
mkdir -p "$D/xdg/data" "$D/nas-xdg/data" "$D/log" "$DEMO_RUN/nas"
