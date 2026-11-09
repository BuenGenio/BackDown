#!/bin/bash
# Build the demo's two BackDown indexes from the generated drives, end to end. Repeatable: wipes the demo's own
# data folders first (never the user's; see env.sh). Needs the drives from gen.py.
set -u
. "/usr/bin/env.sh"
for f in volumes-desktop volumes-nas health-desktop health-nas; do sed "s#@D@##g" "/.json.in" > "/.json"; done
DR="$D/drives"
rm -f $D/log/manifest-old-laptop.json; rm -rf "$D/xdg/data/backdown" "$D/nas-xdg/data/backdown" "$DR/Archive"/* "$DR/Mirror"/*
mkdir -p "$D/xdg/data" "$D/nas-xdg/data"
step() { echo; echo "== $*"; }

step NAS: index, hash, pool
( export XDG_DATA_HOME="/nas-xdg/data" XDG_RUNTIME_DIR="/nas" BACKDOWN_FAKE_VOLUMES="/volumes-nas.json" BACKDOWN_HOSTNAME=nas BACKDOWN_FAKE_HEALTH="/health-nas.json"
  cd "$D/nas" && $BD index --local volume1 volume2 2>&1 | grep -v 'full walk' && $BD hash --local --jobs 4 2>&1 | tail -1
  $BD entities --local >/dev/null 2>&1; $BD pool add --local "$D/nas/volume1/Pool" --reserve 10% 2>&1 | tail -1 )

step desktop: first scans
cd "$DR" && $BD index --local "Old Laptop" "Backup 2021" "Camera cards" 2>&1 | grep -v 'full walk'
$BD hash --local --jobs 4 2>&1 | tail -1

step a second scan of the laptop, with changes, so folders have versions
cd "$DR/Old Laptop/home/alex" && python3 - <<'PY'
import os, random, time
r = random.Random(7)
docs = sorted(p for p in os.listdir('Documents') if p.endswith('.txt'))[:40]
for p in docs[:6]:  # edited
    with open(os.path.join('Documents', p), 'a') as f: f.write('\nAdded a paragraph before the second backup.\n')
    t = time.time() - 86400 * 400; os.utime(os.path.join('Documents', p), (t, t))
for p in docs[6:10]: os.remove(os.path.join('Documents', p))  # deleted: visible at the earlier scan only
os.makedirs('Documents/Camera club/2020', exist_ok=True)
for i in range(5):
    with open(f'Documents/Camera club/2020/Minutes 2020-0{i+1}.txt', 'w') as f: f.write('Minutes\n\nPresent: Alex, Maria, Jonas.\n')
PY
cd "$DR" && $BD index --local "Old Laptop" 2>&1 | grep -v 'full walk'; $BD hash --local --jobs 4 2>&1 | tail -1

step recognise, people, dates and places
$BD entities --local 2>&1 | grep -E '^\s{2}[A-Z]' | sort | uniq -c | sort -rn | head -20
$BD tag exif --geo --local "$DR/Old Laptop/home/alex/Pictures" "$DR/Backup 2021/Laptop copy 2020/Pictures" "$DR/Camera cards" 2>&1 | tail -1

step manifests of two drives
$BD manifest create --local "$DR/Old Laptop" --out $D/log/manifest-old-laptop.json 2>&1 | tail -1
$BD manifest create --local "$DR/Camera cards" 2>&1 >/dev/null | tail -1

step the NAS as another machine
$BD remote add --local nas ssh://alex@nas.local 2>&1 | tail -2
$BD remote sync --local 2>&1 | tail -2

step plan and apply into the Archive
$BD plan --local --to "$DR/Archive" --from "$DR/Old Laptop" --from "$DR/Backup 2021" --from "$DR/Camera cards" --merge-mail 2>&1 | tail -3
time $BD apply --local 1 --go 2>&1 | tail -6
cd "$DR" && $BD index --local Archive 2>&1 | grep -v 'full walk'; $BD hash --local --jobs 4 2>&1 | tail -1

step "a mirror of the archive's media"
mkdir -p "$DR/Mirror"
$BD mirror add --local "$DR/Archive/Media" "$DR/Mirror/Media" 2>&1 | tail -2
$BD mirror sync --local 2>&1 | tail -2; $BD mirror verify --local 2>&1 | tail -2
cd "$DR" && $BD index --local Mirror 2>&1 | grep -v 'full walk'; $BD hash --local --jobs 4 2>&1 | tail -1
$BD entities --local >/dev/null 2>&1

step custody and status
$BD custody check --local 2>&1 | tail -2
$BD status --local 2>&1 | head -12
