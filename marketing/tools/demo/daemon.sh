#!/bin/bash
# start | stop | token | url | log  — the demo's own BackDown daemon, isolated from the user's (see env.sh)
. "/usr/bin/env.sh"
PORT=8798
UI=$XDG_DATA_HOME/backdown/ui.json
case "$1" in
  start)
    pkill -f "backdown daemon --port $PORT" 2>/dev/null; sleep 1
    nohup $BD daemon --port $PORT --rescan-every never --local > $D/log/daemon.log 2>&1 &
    echo "daemon pid $!"
    for i in $(seq 1 40); do curl -s -o /dev/null http://127.0.0.1:$PORT/ && break; sleep 0.5; done
    curl -s -o /dev/null -w 'GET / -> %{http_code}\n' http://127.0.0.1:$PORT/
    ;;
  stop) pkill -f "backdown daemon --port $PORT"; echo stopped ;;
  token) python3 -c "import json,sys; print(json.load(open('$UI'))['token'])" ;;
  url) echo "http://127.0.0.1:$PORT" ;;
  log) tail -30 $D/log/daemon.log ;;
esac
