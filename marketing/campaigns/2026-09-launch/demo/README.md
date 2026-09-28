# BackDown: a five-file demo archive

This tiny, invented archive lets you try indexing and content comparisons without pointing BackDown at personal files. It contains five harmless UTF-8 text files in two folders named `drive-a` and `drive-b`.

These folders simulate drives. They may live on the same physical disk, so this is not a demonstration of independent backup copies.

## What to look for

- `drive-a/shared-note.txt` and `drive-b/renamed-note.txt` have identical contents under different names: one duplicate group, with two 76-byte files.
- Both folders contain `status.txt`. One says GREEN and the other AMBER. Their names and 19-byte sizes match, but their contents differ. They must not appear as a duplicate pair.
- `drive-a/unique-note.txt` exists only in `drive-a`.

The two status files are also unmatched content. The single-file example illustrates a missing counterpart; the status pair illustrates why a matching name and size do not establish matching contents.

## First run

Use an existing BackDown executable. Public alpha downloads and setup notes are at [backdown.io/download](https://backdown.io/download/); no email signup is required. This archive includes no executable.

Open a terminal inside this `demo` folder, where you can see `drive-a` and `drive-b`. The following commands assume `backdown` is on your PATH.

Create the isolated working folder in a macOS or Linux shell:

```sh
mkdir -p .backdown
```

In Windows PowerShell, create it with:

```powershell
New-Item -ItemType Directory -Force .backdown | Out-Null
```

Then run these inspection commands one at a time:

```sh
backdown --local --db .backdown/index.db index drive-a drive-b
backdown --local --db .backdown/index.db hash
backdown --local --db .backdown/index.db dupes --min-size 1 --json
backdown --local --db .backdown/index.db check drive-a --list --json
```

If the Windows executable is in your current folder rather than on PATH, use `./backdown.exe` in place of `backdown`.

`--db` keeps the demo index inside `.backdown/`. `--local` keeps these commands from handing work to a running BackDown daemon. Index only the two named folders: the surrounding README and generated index are not source data. The included `.gitignore` excludes `.backdown/` from version control.

`--min-size 1` includes these tiny text files; the usual default filters out files smaller than 1 MB. `dupes` and `check` also run the needed hashing, so the explicit `hash` command is included to make the stages visible.

Expected results:

- Index: three files in `drive-a`, two in `drive-b`.
- Duplicates: one group containing `shared-note.txt` and `renamed-note.txt`. The duplicate group's reported reclaimable amount is 76 bytes.
- Check: `safe` is `false`, with one verified file and two unique files in `drive-a`. The unique list contains `status.txt` and `unique-note.txt`.
- The check command exits with code **2**. That is the expected “files must be copied first” result, not a failed demo. In a script that stops on any nonzero exit, run this check separately.

A matching hash establishes evidence about compared contents. It is not permission to delete a file or drive. The reported reclaimable amount describes the duplicate group, not a recommendation to remove a copy. No deletion, copying, service installation or drive-mounting commands are part of this demo.

## Optional local browser

```sh
backdown --local --db .backdown/index.db serve --no-open --port 0
```

Open the one-run loopback link printed in the terminal. Port `0` asks the operating system for an available local port. Inspect the two demo folders and text previews. Stop this foreground browser with Ctrl-C when finished; it is not installed as a service.

## Validation and package contents

Validated on Linux on 2026-09-28 using the already-built local `target/release/backdown`, which reported version `0.2.0-alpha.4`. The CLI flags were checked against that executable's help and the repository CLI source. Index, hash, duplicate reporting and the expected check result were run against only these five demo files, with `.backdown/index.db` explicitly selected. The optional local browser started on an automatically selected port and returned HTTP 200. No existing personal index or source drives were used.

This verifies the local build and this small example; macOS and Windows execution were not tested. It does not claim that an independently downloaded release was tested.

`demo-data.zip` contains only this README, `.gitignore` and the five invented source files. The generated `.backdown` index, machine-specific paths and executable are excluded. You can unpack the ZIP to a fresh location and repeat the example with a new isolated index.
