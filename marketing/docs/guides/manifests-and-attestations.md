---
title: Signed manifests and attestations
nav: Manifests and attestations
description: Sign a statement of exactly what a drive held at a scan that anyone can check wasn't changed, have the service sign one after each scan or on a schedule, and check one on another computer, with BackDown or without it.
order: 11
---

A **manifest** is a signed statement of exactly what one drive held at one scan: every file's path, size and hash, sealed into a single fingerprint (a Merkle root) and signed with your BackDown's own key. Change anything in it afterwards, a name, a size, a hash, a date or a count, and it no longer checks. Each drive's manifests are chained, every one naming the one before it, so one left out or two swapped shows too.

They're for when someone else needs to rely on what a drive held: handing a drive over, archiving one before it's wiped, keeping evidence, or proving later that nothing in an archive has changed. An **attestation** is a manifest the background service signs by itself, after each scan or on a schedule.

Signing manifests yourself is marked Forensic, and scheduled attestations Team; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)). Checking a manifest is free, for anyone, and needs no index: `backdown manifest verify`, or a few lines of Python ([below](#without-backdown)).

## What a manifest proves, and what it doesn't

A manifest that checks proves that **the holder of this installation's signing key saw these files, with these sizes and these hashes, at that time**: that at scan 17 of `/media/old-drive`, BackDown's index held exactly the files its listing names, no more and no fewer, and nothing in the document has changed since it was signed. A listing that gives the signed Merkle root lists exactly those files; change one byte of it and it gives another root.

Its limits:

- **Files that weren't hashed are attested by their path and size only.** BackDown [reads only what could be a duplicate](/docs/concepts/#hashes-and-proof), so a file whose size no other file shares is never read, and its line in the listing says `"sha256": null`. A change to such a file that kept its size wouldn't show. The manifest counts them (`unhashed`), and `backdown manifest create` says how many. An empty file's hash is known without reading it, so it counts as hashed.
- **It's what the index held.** A scan records every file it can read under the drive's folder; one it couldn't read, or one on another file system mounted inside it (unless it was indexed with `--cross-filesystems`), isn't in the manifest.
- **It doesn't prove the key's owner is honest.** It proves which key signed. Whoever controls the computer, its index and its key decides what's signed, and the dates in it come from that computer's clock. To tie the key to a person, they give you its fingerprint beforehand, some other way (`backdown manifest key`). To prove a manifest existed by a certain day, put its Merkle root, or the SHA-256 of the file, somewhere that keeps dates you trust: an email, a ticket, a timestamping service.

## Sign one

```sh
backdown manifest create /media/old-drive --out old-drive.manifest.json --list old-drive.listing.jsonl
```

```text
Signed manifest 4 of /media/old-drive at scan 17: 182.4 K files, 412.6 GB; 151.0 K hashed, 31.4 K by path and size only
  Merkle root 6a1e0c9b3d4f…
  follows manifest 3 of this drive · key 37f2-ec0e-8ce7-d314
  document: old-drive.manifest.json
  listing:  old-drive.listing.jsonl
```

It attests the drive's latest finished scan, read from the index, so the drive needn't be connected; `--scan N` attests an earlier one (`backdown scans` lists them). A folder on the drive names the drive too. Hash first if you want as much as possible attested by content: `backdown hash`, or let the background service finish after its scan.

The **document** is small JSON; without `--out` it goes to the terminal. The **listing** has a line for every file, which is what lets someone check *which* files were signed; keep it with the document. Neither file is ever overwritten. Every manifest is also kept in the index, chained to the drive's previous one, and the first one you sign creates your key ([below](#the-signing-key)).

## Have the service sign them

In **Settings > Attestations**, "Sign a manifest of each drive":

- **Off**, the default;
- **After each scan**: as soon as a drive's scan and the hashing after it are done;
- **Once a day**, **Once a week**: when the drive's newest manifest is that old.

A manifest is only made of a scan no manifest covers yet: a drive that hasn't been rescanned isn't signed again. Activity lists each one as "Sign a manifest of *drive*", with what it signed. At the top of a drive in the browser, the details pane (<kbd>Ctrl+I</kbd>) lists its signed manifests, newest first, each with **Download** for the signed document.

The machine-wide service ([Settings](/docs/guides/settings-and-licences/#a-machine-wide-service)) signs with a key of its own, kept in `/var/lib/backdown` and readable only by its `backdown` user, not with yours; Settings shows which key it is. If it can't keep a key there, it doesn't sign, and its log says why.

## List them, and get one out

```sh
backdown manifest list
```

```text
  ID  SIGNED                  SCAN      FILES       SIZE  HASHED  KEY                  DRIVE
   3  2026-09-24 09:20 UTC      12   181.9 K   411.8 GB     82%  37f2-ec0e-8ce7-d314  /media/old-drive
   4  2026-10-01 09:20 UTC      17   182.4 K   412.6 GB     82%  37f2-ec0e-8ce7-d314  /media/old-drive

/media/old-drive: 2 manifests, the chain is whole
```

`backdown manifest list /media/old-drive` shows one drive's. It exits with 2 when a chain is broken.

A manifest the service signed has no listing yet. `export` writes out any kept manifest and makes its listing again from the index's record of that scan, with the hashes it had when it was signed (the index remembers which files had none):

```sh
backdown manifest export 4 --out old-drive.manifest.json --list old-drive.listing.jsonl
```

It refuses if the index's record no longer gives the signed root, which means the index changed since.

## Check one

```sh
backdown manifest verify old-drive.manifest.json --list old-drive.listing.jsonl
```

```text
old-drive.manifest.json:
  Signature: good, by key 37f2-ec0e-8ce7-d314 (this installation's own)
  A manifest of /media/old-drive (drive 3) at scan 17, which finished 2026-10-01T09:12:44Z; signed 2026-10-01T09:20:03Z
  182.4 K files, 412.6 GB: 151.0 K hashed, 31.4 K attested by path and size only
  Chain: kept here as manifest 4, following manifest 3
  Listing: good: its 182.4 K files give the signed Merkle root
```

It checks the signature, and with `--list` that the listing is exactly what was signed: the same files, sizes and hashes, in the same order. Where the index here keeps the drive's manifests, it checks the chain against them. Given several manifests of a drive, in any order, it puts them in their chain's order and checks that each follows the one before:

```sh
backdown manifest verify old-drive-*.manifest.json
```

A missing one shows as "a gap", two swapped as "out of order". `verify` exits with 0 when everything checks and 2 when anything doesn't; `--json` gives all of it.

`--against-index` also compares the manifest with what the index holds for the drive now: what was added, removed and changed since. Without `--list`, the manifest's side comes from the index's record of its scan, and it first says whether that still gives the signed root, which shows whether the index's history has been changed:

```text
  Compared with the index: /media/old-drive at scan 21 (2026-10-03 08:00 UTC)
    The index's record of scan 17 still gives the signed Merkle root: it holds exactly what was signed.
    since then: 12 added (48 MB), 1 removed (2 MB), 1 changed; 182.4 K unchanged, 31.4 K of them the same size but not hashed on both sides
    added    Photos/2026/IMG_2210.jpg
    removed  Notes/old.txt
    changed  Documents/report.odt  (24 KB 942854ac803d… → 25 KB 6dbfea99b2f0…)
```

A file counts as changed when its size differs, or both sides have a hash and they differ.

### On another computer

Copy the document and its listing over, and run `backdown manifest verify` there: it needs no index, and checking is free in every edition. Then compare the key's fingerprint with the one the signer gave you. "this installation's own" appears only on the computer that signed it.

### Without BackDown

The format is plain ([below](#the-format)), so anything that has SHA-256 and Ed25519 can check a manifest. This Python checks the signature (with the `cryptography` package) and the listing:

```python
#!/usr/bin/env python3
"""Check a BackDown manifest without BackDown: python3 verify_manifest.py MANIFEST.json LISTING.jsonl"""
import base64, hashlib, json, struct, sys

H = lambda b: hashlib.sha256(b).digest()
b64 = lambda s: base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))
doc = json.load(open(sys.argv[1], encoding="utf-8"))
ok = True

# 1. the signature: Ed25519, over the canonical form (keys sorted, no spaces) without "sig"
try:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    unsigned = {k: v for k, v in doc.items() if k != "sig"}
    canonical = json.dumps(unsigned, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    try:
        key = Ed25519PublicKey.from_public_bytes(bytes.fromhex(doc["key"]))
        key.verify(b64(doc["sig"]), canonical.encode("utf-8"))
        print("signature: good, by key", doc["key"])
    except InvalidSignature:
        print("signature: DOESN'T MATCH: the document changed after it was signed")
        ok = False
except ImportError:
    print("signature: not checked (pip install cryptography)")

# 2. the listing: one leaf per line, in path-byte order, giving the signed Merkle root
level, last, files, size = [], None, 0, 0
for line in open(sys.argv[2], encoding="utf-8"):
    if not line.strip():
        continue
    f = json.loads(line)
    path = b64(f["path_b64"]) if "path_b64" in f else f["path"].encode("utf-8")
    if last is not None and path <= last:
        sys.exit(f"listing: out of order at {f['path']}")
    last = path
    digest = bytes.fromhex(f["sha256"]) if f["sha256"] else bytes(32)
    level.append(H(b"\x00" + struct.pack(">I", len(path)) + path + struct.pack(">Q", f["size"]) + digest))
    files, size = files + 1, size + f["size"]
while len(level) > 1:
    up = [H(b"\x01" + level[i] + level[i + 1]) for i in range(0, len(level) - 1, 2)]
    if len(level) % 2:
        up.append(level[-1])  # an odd node at the end of a level is carried up unchanged
    level = up
root = level[0] if level else H(b"")
same = root.hex() == doc["merkle"] and files == doc["counts"]["files"] and size == doc["counts"]["bytes"]
print(f"listing: {files} files, {size} bytes, Merkle root {root.hex()}:", "matches" if same else "DOESN'T MATCH")
sys.exit(0 if ok and same else 2)
```

## The signing key

Each installation of BackDown makes an Ed25519 key of its own the first time it signs, and keeps it in its data folder as `signing.key`, readable only by you. It never leaves the computer.

```sh
backdown manifest key
```

```text
This installation's signing key, kept in ~/.local/share/backdown/signing.key (readable by you alone):
  fingerprint  37f2-ec0e-8ce7-d314
  public key   918dfc0ea3a6c491479b5ca11e3df7f2a10ff812adf042449e74fc7fe0eb6b55
```

Every manifest names the public key that signed it. The fingerprint is the first 16 hex digits of the SHA-256 of the public key's hex, in groups of four: short enough to read out over the phone. Back the key up with the rest of your data folder: a new key starts new chains, since nothing can sign as the old one.

## The format

### The listing

[JSON Lines](https://jsonlines.org/): one file per line, in the order of the tree's leaves.

```text
{"path":"Documents/report.odt","size":24576,"sha256":"942854ac803d5a3c54ac30beaaddd5b344181aa5bccb071c4e3652c2ed75359e"}
{"path":"Notes/caf�.txt","path_b64":"Tm90ZXMvY2Fm6S50eHQ","size":9,"sha256":null}
{"path":"empty.txt","size":0,"sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
```

- `path` is relative to the drive, with the separator of the system that made it (`/`, or `\` on Windows), exactly as the drive spells it (not normalised).
- A name that isn't valid Unicode, such as a Latin-1 name from an old drive, can't be written exactly as a JSON string. Its line also has `path_b64`, the path's bytes in base64url without padding, and its `path` is then only a readable stand-in.
- `size` is in bytes. `sha256` is the content's SHA-256 in hex, or `null` when it wasn't hashed.

### The Merkle root (`backdown-merkle-v1`)

1. A file's **path bytes** are the UTF-8 bytes of `path`, or the bytes `path_b64` decodes to when it's there. (For a name that isn't valid Unicode, those are the bytes the system stores: the name's own bytes on Linux and macOS, its UTF-16LE on Windows.)
2. The leaves are sorted by path bytes, compared as unsigned bytes. Each path appears once.
3. A **leaf** is `SHA-256(0x00 ‖ length ‖ path bytes ‖ size ‖ hash)`: `length` is the number of path bytes as 4 bytes big-endian, `size` the file's size as 8 bytes big-endian, `hash` the 32 bytes of its SHA-256, or 32 zero bytes when it isn't hashed.
4. An **inner node** is `SHA-256(0x01 ‖ left ‖ right)`. Each level pairs its nodes from the left; an odd node at the end of a level is carried up to the next level unchanged.
5. The **root** is the one node left at the top. A drive with no files has the SHA-256 of nothing as its root (`e3b0c442…b855`).

For example, three files `a.txt` (3 bytes, the SHA-256 of `abc`), `b/c` (empty) and `d` (5 bytes, not hashed) give the root `6dc17965ade625c342c4ac85bf48b5a5a844e335ad00f3f452dd6a7b8ba8d675`: the leaves of `a.txt` and `b/c` are paired, and `d`'s leaf, left over, joins them at the top.

### The document

```json
{
  "algorithm": "backdown-merkle-v1",
  "counts": { "bytes": 412612345678, "files": 182431, "hashed": 151012, "unhashed": 31419 },
  "created": "2026-10-01T09:20:03Z",
  "drive": { "path": "/media/old-drive", "root_id": 3 },
  "key": "918dfc0ea3a6c491479b5ca11e3df7f2a10ff812adf042449e74fc7fe0eb6b55",
  "kind": "backdown-manifest",
  "merkle": "6a1e0c9b3d4f…",
  "prev": "c0ffee52e9a7…",
  "scan": { "finished_at": "2026-10-01T09:12:44Z", "id": 17 },
  "sig": "8oiPdgZ1YopwqG7mjw-1-5qaUMNqmFPKime-…",
  "v": 1
}
```

- `drive` is the drive's folder on the computer that signed it (with a `path_b64` too when that isn't valid Unicode) and its number in that index. `scan` is the scan attested and when it finished; `created` is when the manifest was signed, in UTC.
- `counts`: the files, their total size, and how many were and weren't hashed.
- `key` is the Ed25519 public key that signed it, in hex. `sig` is the Ed25519 signature, in base64url without padding, of the document's **canonical form** without `sig`: its JSON with every object's keys sorted (by their UTF-8 bytes) and no spaces or line breaks, strings and numbers as standard JSON writes them, non-ASCII characters as they are.
- `prev` is the SHA-256, in hex, of the canonical form of the drive's previous manifest, whole (its `sig` included), or `null` for a drive's first. That's the chain: a manifest can't be taken out of it, put in another place, or changed, without the next one's `prev` no longer matching.
