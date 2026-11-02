---
title: Several machines
description: Let copies on a NAS or another computer count, reached over ssh with your own keys: add a machine, sync it, browse it, gather its drive-health warnings here, and free up drives here against copies there.
order: 5
---

Your archive may be on a NAS, and old drives plugged into a different computer. BackDown on this computer can read another machine's BackDown index over ssh, so files there count as copies: for duplicates, for overlaps, for `check`, and for freeing up drives here; and its drives' health is gathered here too, so a warning on the NAS reaches you. Nothing on the other machine is ever changed, and no file's contents are sent: only its index entries, its drives' health as it last read them, and, when asked, SHA-256 hashes.

Several machines is free: it's what [the swarm](/docs/guides/the-swarm/) is built on, which also shows every machine's drives and free space, finds a file on any of them, and pours drives into shared pool folders.

## Before you start

On the other machine (Linux or macOS):

1. Install BackDown, and index its drives there (`backdown index`, or its own background service). BackDown here reads that index; it doesn't index the other machine's drives itself.
2. Make sure you can log in to it with `ssh` without a password, with a key: `ssh-copy-id you@nas.example.org` sets that up.
3. Connect once with `ssh` in a terminal, to check and accept its host key.

BackDown never asks for a password and never accepts an unknown host key on your behalf, so these have to work first.

## Add a machine

```sh
backdown remote add nas ssh://you@nas.example.org
```

```text
Added nas (ssh://you@nas.example.org).
Reached nas (nas): BackDown 1.0.0-beta.1, index /var/lib/backdown/index.db, 2 drives indexed: /volume1, /volume2
Next: backdown remote sync nas (the BackDown service also syncs it every 6 h)
```

The name is yours (like `nas` or `office-pc`). The address is `ssh://[user@]host[:port]`, or a host alias from your `~/.ssh/config`. BackDown there finds its index the way it would itself (its own service's, or its default); if it's elsewhere, `--remote-db /path/to/index.db`. If `backdown` isn't on the `PATH` an ssh session gets there, `--backdown /usr/local/bin/backdown`. `backdown remote test nas` tries the connection again at any time.

In the app, the sidebar's **Machines** section has **Add a machine**, with the same fields.

## Sync

```sh
backdown remote sync
```

```text
nas: 2 drives, 412.8 K files (412.8 K new or changed, 0 gone); 1.2 K sampled and 310 hashed there for comparison; drive health of 3 drives
```

A sync fetches what changed in the other machine's index since the last one, by that machine's scans. Then, so that copies can be proved, files there whose size matches a file here are sampled there, and those whose samples match are hashed there in full; after the sync, BackDown hashes files here against what's there. The other machine reads only what it has to, and each sync spends at most 20 minutes hashing there; what's left waits for the next one.

With the background service running, you don't need to: it syncs each machine every 6 hours (`backdown remote settings --sync-hours 12` changes that), one at a time, beside its other work, and hashes here afterwards. A scheduled sync that fails becomes a notification. Syncing runs in your own BackDown: the machine-wide service has no ssh keys of yours.

## Its drives' health

Each sync also fetches the other machine's drive health, as its own BackDown last read it from its drives: each drive's verdict, its readings and latest changes, and that machine's own latest drive-health notifications. It's read from its index (no drive there is read again for it) over the same ssh connection, so it's as fresh as the last sync. Activity's **Other machines' drives** and `backdown health --machines` show it, and a drive there that's worse than at the last sync becomes a notification here: "A drive on nas is failing". [Drive health](/docs/guides/drive-health/#other-machines-drives) has the details. It's part of BackDown Team (unlocked in this beta).

A BackDown on the other machine from before this feature can't send it: its sync goes on as ever, and it says "update BackDown on nas to see its drive health".

```sh
backdown remote list
```

```text
nas  ssh://you@nas.example.org  (index /var/lib/backdown/index.db)
  synced 2 h ago
  /volume1  398.1 K files, 1.80 TB · scanned there 2 days ago · its copies count
  /volume2  14.7 K files, 96.0 GB · scanned there 9 days ago · too long ago for its copies to count
Copies on another machine count when confirmed there within 7 days (backdown remote settings --proof-days N).
```

## How copies there count

A file on another machine counts as a copy of a file here only when:

- both have the same SHA-256 and size (never by name or size alone);
- it could be read there, and hasn't changed since that machine's scan;
- it was *confirmed* there recently: that machine's last scan of its drive, as synced here, finished within the last 7 days, or that machine read the file again for BackDown within them. `backdown remote settings --proof-days 14` changes the window (1 to 90 days).

So the other machine has to keep being scanned (its own background service does it), and synced. A copy that's too old says so: "more than 7 days ago, so it doesn't count until nas has been scanned and synced again".

Where they show:

- `backdown check` says how much of what's verified is only on another machine: "of the verified, 1.2 GB in 3 files are only on another machine (nas (ssh://you@nas.example.org)): hashed there and confirmed there within 7 days". A drive can be safe to wipe with some of its copies there.
- The browser's **Duplicates** view lists copies there ("on **nas**: /volume1/Photos/…"), and "Here once, and again on another machine" for files with one copy here and one there. `backdown dupes` in a terminal lists only this computer's.
- A file's details list "On other machines", each with when it was confirmed and whether it counts.
- `backdown overlaps` counts a machine as a place copies can be ("on nas").

## Free up a drive against copies there

[Freeing up](/docs/guides/free-up-a-drive/) uses a copy on another machine only when there's no copy on this one. The dry run says how much that is. Then, just before deleting such a file here, BackDown has the other machine read its copy again, in full, and compares the SHA-256 with the file here, which has just been read again too. Only if they match is the file deleted, and the journal records the copy there (`ssh://…/path`, "remote re-read") as its proof. If the machine can't be reached, or its copy changed, the file stays here (or, with `--move-to`, is moved first).

With `--trust-index`, the other machine only checks that its copy is still there as it last scanned it, and answers with the hash in its index, without reading the file.

## Browse a machine

Each machine in the sidebar opens its page: whether it can be reached, when it was synced, its drives (and whether their copies count), **Sync now**, **Test connection** and **Remove…**. Its drives can be browsed, read-only: live over ssh as its last scan saw them, or from what was synced when it can't be reached. A file's details show its hash there and its copies here and elsewhere.

## Open, preview or save a file that's there {#fetch}

A file's details on a machine's page have **Preview**, **Download a copy** and **Open**. Each brings that one file here over the same ssh connection, only then: the other machine reads it only if it's still exactly as its index scanned it (same size and date), and says so otherwise. Files up to 256 MB are kept in a small cache here (`remote-cache` beside the index, 2 GB in all, the least recently used going first), so a second look or a video seek costs nothing more; bigger files stream the part asked for each time. In a terminal:

```sh
backdown remote fetch nas /volume1/Photos/2019/IMG_0412.jpg -o ~/Pictures/
backdown remote fetch nas 18233 -o ./notes.txt     # by its id in the NAS's index
```

```text
Fetched IMG_0412.jpg (3.1 MB) from nas: /volume1/Photos/2019/IMG_0412.jpg → /home/you/Pictures/IMG_0412.jpg; SHA-256 verified
```

The path is the one `remote sync` saw; the bytes are hashed on the way and compared with the hash the other machine has, when it has one. Nothing is overwritten. Indexing stays each machine's own business: this reads one file you asked for, never a drive.

To forget a machine: `backdown remote remove nas` (or **Remove…**). Nothing on it is touched; its copies stop counting here.

## Security

- BackDown runs your system's own `ssh`, as you, with your keys, agent, `known_hosts` and `~/.ssh/config`. It adds `BatchMode=yes`, so ssh never asks for a password, and turns off agent and X11 forwarding. Host keys are checked as your ssh configuration says.
- Nothing listens on the network, here or there. On the other machine, ssh starts `backdown serve-stdio`, which answers questions about its index over the ssh connection and exits when it closes. It opens the index read-only, never creates or changes one, and reads a file only to hash it when asked, and only if it's still as indexed.
- What travels is index entries (paths, sizes, dates, hashes), hashes, and what that machine's BackDown last read of its drives' health (their SMART readings, verdicts and its drive-health notifications). A file's contents travel only when you open, preview, download or `remote fetch` it, one file at a time, and only from that machine's indexed drives.

## When it can't connect

| It says | What to do |
|---|---|
| "ssh couldn't log in to nas with your keys" | Add your public key there (`ssh-copy-id`), or load it into your ssh agent |
| "nas's host key isn't known here, or it changed" | Connect once with `ssh` in a terminal, check the key and accept it |
| "backdown isn't installed on nas, or isn't on the PATH ssh sessions get there" | Add the machine with `--backdown /full/path/to/backdown` |
| "there's no BackDown index at … on nas" | Index something there first, or give its index with `--remote-db` |
| "the BackDown on nas is too old to be reached this way", or a protocol mismatch | Update the older one |
| "update BackDown on nas to see its drive health" | Its BackDown is from before drive health was gathered: update it there. Everything else works meanwhile |
| "nas didn't answer" | Is it on, and on this network? |

## Not yet

Windows computers can't be the other end yet (they can be this end).

## Further

[The swarm and the pool](/docs/guides/the-swarm/): the machines as one, where a file is across all of them, and filing drives into pool folders on any of them.
