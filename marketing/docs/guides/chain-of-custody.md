---
title: Chain of custody
description: Every copy, move, deletion, link and merge BackDown makes, in a hash-chained log sealed with this computer's key. Export it for evidence handling, and let anyone verify it, with BackDown or without.
order: 9.5
---

Whenever BackDown changes something about your files (it copies a file into the archive, moves one, deletes one whose copy it has just checked, links two identical ones, merges a mailbox), it records it in a log. Each entry is chained to the one before it by its hash, and the log is sealed with a key that belongs to this installation of BackDown. You can export the log as one signed file and hand it to an auditor, a court or a client. They can check it with `backdown custody verify`, or with a few lines of Python and no BackDown at all.

BackDown keeps the log in every edition, from the day you install it. Exporting, verifying and checking the log are part of **BackDown Forensic**; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)).

> [!NOTE]
> The log starts on the day this BackDown was installed, or updated to a version that keeps it. Anything BackDown did before that is in its own records (plans, wipes), but not in the chain, and BackDown doesn't add it afterwards. Every export says when the log starts.

## What is recorded, and when

Each event is written in the same database transaction as the job's own journal entry, so an event exists exactly when the action it describes was done. (Mirrors and Rescue keep no journal of their own: their events are written as each file is done.) Nothing else is recorded: not reading, scanning or hashing, and not a file that was already where it was meant to go.

| Action | What happened | Done by |
|---|---|---|
| `copy` | `src` was copied to `dest`, and the copy was read back and matched | [applying a plan](/docs/guides/file-into-an-archive/), [a mirror](/docs/guides/mirrors/) sync |
| `move` | the file at `src` is now at `dest`, and no longer at `src` | [freeing up](/docs/guides/free-up-a-drive/) with `--move-to` (copied, read back, then deleted); a mirror setting aside the old copy of a changed file (renamed, not read) |
| `delete` | `src` was deleted; `dest` is the identical copy that was kept | freeing up a drive |
| `link` | `src` now shares its storage with the identical `dest`, as a hardlink or a clone | [tidying the archive](/docs/guides/file-into-an-archive/#tidy-the-archive) |
| `merge` | a mail message from `src` was written to `dest` | [merging mailboxes](/docs/guides/merge-mailboxes/) |
| `rescue-copy` | a file was read out of a filesystem on a drive or disk image, and copied to `dest` | [Rescue](/docs/guides/recover-deleted-files/)'s "copy these files out" |

Each event also has:

- `ref`: the job, as `plan:3`, `wipe:5`, `dedup:2`, `merge:4`, `rescue:1` or `mirror:2`. A mailbox merged as part of a plan is recorded under its merge.
- `src_sha256` and `dest_sha256`: the content's SHA-256, where BackDown read it or had it confirmed. `dest_sha256` is left out when the copy wasn't read just then: a wipe with `--trust-index` relies on the index for the copy, and a mirror's set-aside copy isn't read. For a message taken from an mbox file, `src` is the mbox and the hash is the message's.
- `size` in bytes, and `at`, when it was done (Unix seconds, from this computer's clock).

A deletion's `dest` is the copy that made it safe: a path on this computer, or `ssh://…` for a copy [on another machine](/docs/guides/several-machines/) (an empty file needs no copy, and has no `dest`). A rescued file's `src` names the drive or image, where its filesystem starts (in bytes) and the path inside: `/dev/sdb@1048576:/DCIM/IMG_0001.JPG`.

Not in the log: exporting a Maildir as an mbox file, Rescue's disk images and the files it finds by their content (they have no source file to name), and anything done to your files by other programs.

## Seals

A seal is a signed statement of the log's last event: its number, its hash and the time. BackDown signs it with this installation's own key, an Ed25519 key made the first time one is needed and kept in BackDown's data folder (`signing.key`), readable by your account alone.

- The background service seals the log at the end of every job that added events.
- A command run in the terminal without the service seals when it finishes.
- An export seals first.

Before it seals, BackDown checks the log since the last seal. A log that doesn't check out isn't sealed: the service's log says why, and so does `backdown custody check`.

## What it proves, and what it doesn't

The chain proves that each event is exactly as BackDown recorded it, in that order, with nothing taken out or slipped in between. Changing any field of an event, removing one, adding one or swapping two breaks the chain from there on, and verifying says which event.

A seal proves that this installation's key saw the log, as it was then, up to that event. Rewriting the log before a seal no longer matches it, even if every hash after the change is recomputed. The export's own signature proves the file wasn't changed after it was made, and says which key made it.

What it can't prove:

- **That nothing happened outside BackDown.** The log is a record of what BackDown did. A file changed by another program, a copy made by hand, or what a drive held before BackDown first saw it isn't in it.
- **Who held the key.** BackDown has to be able to read its key to sign, so anyone with access to your account can read it too, and could make up a log and sign it. A seal is as trustworthy as that key: note its fingerprint (each export prints it, as in `3ea4-8535-4fa4-c62d`) somewhere others can see it, such as in a report or with an auditor, so a later export can be compared with it.
- **The events after the last seal.** Until the next seal (the end of the running job), the newest events could be removed without a trace.
- **The time.** `at` is this computer's clock.

## Export

```sh
backdown custody export --out custody.json
```

```text
Wrote custody.json: 1043 events.
Sealed up to event 1043; signed with this installation's key 3ea4-8535-4fa4-c62d.
The log starts on 3 Oct 2026, when this BackDown was installed (or updated to a version that keeps it): what happened before isn't in it.
Check it with: backdown custody verify custody.json
```

An export is never written over an existing file (`--out -` writes it to the terminal). It's one JSON file, with one event per line.

To hand over only part of the log, filter it:

- `--since 2026-10-01` (or `2026-10-01T14:30`): what was done from that day on, in UTC;
- `--ref wipe:5`: one job's events. Activity shows each job with its number ("Apply plan 3", "Free up (wipe 5)"), and `backdown plans` lists plans;
- `--path /media/old-drive`: what was done to files in that folder, as `src` or `dest`. Whole names only: `/media/old` doesn't match `/media/old-drive`.

With a filter, the events that don't match are still in the export, *elided*: only their number, their digest and their hash are there, so the chain can still be checked from the first event to the last, but nothing about those files (no path, no hash of a file, no job) is shown. The export says which filter was used.

```sh
backdown custody export --out old-drive.json --path /media/old-drive
```

```text
Wrote old-drive.json: 1043 events, 212 shown and 831 elided (only events in /media/old-drive are shown; the others keep their number, digest and hash, not their paths).
```

In the browser, **Activity** has a **Chain of custody** button by "Recent" (when the background service runs), and so does the list of past wipes in **Free up**. Both download the whole log as an export. They only read: if the log isn't sealed up to its last event yet, the download carries a seal made for it, which isn't kept. It works from other devices too, which can only look.

## Verify an export

```sh
backdown custody verify custody.json
```

```text
custody.json checks out.
  1043 events, done 3 Oct 2026 to 9 Oct 2026
  sealed up to event 1043 (12 seals), by key 3ea4-8535-4fa4-c62d
  exported 2026-10-09 14:00 UTC, signed by key 3ea4-8535-4fa4-c62d
  the log starts on 3 Oct 2026, when this BackDown was installed (or updated to a version that keeps it): what happened before isn't in it.
```

It checks every event against its hash and the one before it, every seal's signature against the event it names, that the seals reach the last event, and the export's own signature. When something doesn't check out, it says exactly what and exits with 1:

```text
custody.json DOESN'T CHECK OUT:
  - the export itself was changed after it was made: its signature doesn't match
  - event 1043 was changed after it was recorded
  - events after seq 1200 aren't covered by a seal
```

`--json` gives the same as JSON. Verifying needs nothing but the file, and BackDown doesn't need to be the one that made it.

## Check the log in the index

```sh
backdown custody check
```

checks the log kept in BackDown's index the same way. It's how you find out whether anyone changed the database behind BackDown's back. Events after the last seal are listed as not sealed yet: they're sealed when the job adding them ends.

## Verify without BackDown

Everything needed to check an export is in it, and the method is standard: SHA-256, Ed25519 and JSON.

**The export** is a JSON object: `v` (1), `kind` (`"backdown-custody"`), `created` and `log_starts` (Unix seconds), `key` (the Ed25519 public key that signed it, 64 hex digits), `filter` (when one was used), `note` (the same in words), `events`, `seals` and `sig`.

**An event** has `seq` (1, 2, 3… in the order recorded), `at`, `action`, `ref`, `src`, `size`, and when they apply `src_sha256`, `dest` and `dest_sha256` (fields that don't apply are left out, not null), then `prev` and `hash`. Hashes are lowercase hex. A path is a JSON string when it's valid UTF-8. Otherwise it's `src_base64` or `dest_base64` instead: the path's raw bytes in standard base64 (on Windows, its UTF-16LE bytes), so no name is lost. An elided event is `{seq, digest, hash, elided: true}`.

**The canonical form** of a JSON value: object keys sorted (by their bytes), no spaces or line breaks, strings with only `"`, `\` and control characters escaped (non-ASCII characters as they are, in UTF-8), and integers as plain digits. In Python: `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()`.

**The chain:**

1. An event's `digest` is the SHA-256 of the canonical form of the event without its `prev` and `hash`. An elided event gives its `digest` instead.
2. Its `hash` is the SHA-256 of its `prev` and its `digest` joined: the two as 64 hex digits each, one after the other, 128 characters. (`printf '%s%s' "$prev" "$digest" | sha256sum`)
3. Its `prev` is the `hash` of the event before; the first event's is 64 zeros.

**Signatures** (the export's and each seal's): Ed25519 (RFC 8032) by `key`, over the canonical form of the whole object without its `sig`. `sig` is the 64-byte signature in base64url without padding. **A seal** is `{kind: "backdown-custody-seal", seq, hash, at, key, sig}`. It checks out when its signature does and the event numbered `seq` has that `hash`.

The whole check, in Python, with the `cryptography` package:

```python
import base64, hashlib, json, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

def canonical(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()

def sha256(b):
    return hashlib.sha256(b).hexdigest()

def signed_by(doc):  # raises InvalidSignature if doc was changed
    unsigned = {k: v for k, v in doc.items() if k != "sig"}
    sig = base64.urlsafe_b64decode(doc["sig"] + "=" * (-len(doc["sig"]) % 4))
    Ed25519PublicKey.from_public_bytes(bytes.fromhex(doc["key"])).verify(sig, canonical(unsigned))
    return doc["key"]

bundle = json.load(open(sys.argv[1], encoding="utf-8"))
key = signed_by(bundle)
prev, hashes = "0" * 64, {}
for n, e in enumerate(bundle["events"], start=1):
    assert e["seq"] == n, f"event {n} is missing"
    if e.get("elided"):
        digest = e["digest"]
    else:
        assert e["prev"] == prev, f"event {n} doesn't follow the one before"
        digest = sha256(canonical({k: v for k, v in e.items() if k not in ("prev", "hash")}))
    assert sha256((prev + digest).encode()) == e["hash"], f"event {n} was changed"
    prev = hashes[n] = e["hash"]
sealed = 0
for s in bundle["seals"]:
    signed_by(s)
    assert s["kind"] == "backdown-custody-seal" and hashes.get(s["seq"]) == s["hash"], s
    sealed = max(sealed, s["seq"])
assert sealed == len(bundle["events"]), f"events after {sealed} aren't sealed"
print(f"ok: {len(bundle['events'])} events, sealed to {sealed}, signed by {key}")
```

Then compare the key it prints with the one you expect. BackDown shows a key as a fingerprint: the first 16 hex digits of the SHA-256 of the key's 64 hex digits, in groups of four.
