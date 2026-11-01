# BackDown — the swarm

**One line:** every machine you run BackDown on works as one: each knows which machine and which drive holds every file, how much room there is everywhere, and the pool, folders on any of them, becomes one filed, indexed archive that drives from any machine can be poured into, each file copied once, verified, and the old drive then freed.

Asked for on 1 Nov 2026: "a fleet of servers/devices can interact with the others in order to better organize files I may throw into the pool at any given moment from any machine; e.g. I have a stack of 2.5" drives I'm trying to simmer down to a solid, logical, well filed block, fully indexed so I can work with it". Four parts:

1. **Where is it**: which machine + drive holds which file, across the swarm.
2. **How much room**: every drive's size and free space, per machine and in total.
3. **The pool**: one logical, filed tree (Mail/…, Media/Photos/…, Machines/…) spread over folders on several drives and machines.
4. **The wizard**: for a new drive, what is already in the pool, where the rest should go and why, then copying it there and offering to free the drive.

Decisions (the user, 1 Nov 2026): the pool keeps **one copy** of each file (a second stays optional, through standing mirrors); after a drive's files are copied and verified, the **wizard's last step offers to free up the drive** at once, with Free up's usual proofs and typed confirmation; the swarm is **Community** (free).

---

## 1. Built on `backdown remote`, not beside it

BackDown already reaches other machines (`backdown remote`, Pro until now): the system's own `ssh` with the person's keys, BatchMode, host keys checked, no listener of BackDown's own on the network; at the other end `backdown serve-stdio`, a read-only JSON-lines answerer. Each machine's files and hashes are cached here by its scan ids, and copies there count for duplicates and "safe to wipe" only by SHA-256, only when confirmed recently, and are re-read there before a wipe relies on them.

The swarm keeps all of that:

- **A swarm is the machines this BackDown reaches, plus itself.** No membership protocol, no coordinator, no new daemon: `backdown remote add` (or `swarm join`, which also adds the machines that one reaches) is joining. Each machine shows the swarm it can see. An always-on server that reaches every other machine is the natural place to look from, but nothing depends on it.
- **Each machine describes itself.** Its drives and free space (`volumes`), and its pool folders (`pool`), come from it, are cached with each sync, and are refreshed live when a view opens. No configuration has to be kept the same on several machines: a pool folder belongs to the machine whose drive it's on, and only that machine can add, change or remove it.
- **Nothing new listens on the network.** Writing to another machine (copying into its pool folders) goes through ssh too, to `backdown swarm-stdio`, which writes only inside that machine's own pool folders (see §5).

**No new index, central or distributed.** Each machine's index stays what it is (same file, same schema; the swarm only adds a few small tables the way earlier features did, which older versions ignore) and stays the one authority on that machine's files. Other machines are read through the same requests a pre-swarm `backdown remote` sends (`roots`, `ls`, `find`, `changes`, `hash`), plus additive ones (`volumes`, `pool`) that an older BackDown politely doesn't know; every answer is tagged with the machine, and now the drive, it came from. The only thing kept about another machine is the cache `remote` already keeps (its files and hashes by scan id, so copies count and "where is it" works while it's offline): it can be dropped and rebuilt. Writing (§5) never touches another machine's index either: files land in that machine's own pool folders, and its own BackDown rescans them.

Because the swarm needs to reach machines, reaching machines (`multi-machine`, "Several machines") moves to Community with it. That's a pricing change; it's one line in `tiers.rs` if it should go back.

## 2. Storage: what's where, and how much room

A **drive** is a physical disk (model, serial), a **volume** a file system on it (label, UUID, size, free space, mount points). Each machine probes its own (`volumes::probe`, never as root, never mounting) and adds each mounted volume's size and free space (`statvfs`; `GetDiskFreeSpaceEx` on Windows). The indexed drives (roots) are matched to the volumes they're on.

- `serve-stdio` answers a new `volumes` request with that probe; `sync` keeps it per machine (`remote_volumes`); a BackDown there from before simply doesn't say ("update BackDown on nas to see its drives").
- `backdown swarm` and the browser's **Swarm** view: each machine (this one first, live; the others as last synced, refreshed live when reachable), each drive as a bar of used and free space, what's indexed on it, its health where known, and totals.
- **Where is it**: `backdown swarm where NAME` and the Swarm view's search: this machine's index, and each other machine's, asked live (its own name index answers in a fraction of a second), or, when it can't be reached, its cached files. Each hit says machine, drive (label, model, serial) and path, and how fresh that is.

A drive that moves between machines is the same drive: matching by serial and volume UUID (not by mount path) is what lets the swarm say "the WD 2.5" s/n WX…, now on nextcloud, was indexed on tomahawk last week".

## 3. The pool

A **pool folder** is a folder on a drive of one machine, declared there: `backdown pool add /srv/pool [--reserve 50G] [--takes Mail,Media]`. Its machine keeps the list (`pool_areas`); every other machine learns it with each sync (`pool` request). Together the pool folders are one logical tree: the filing schema's places (`Mail/{origin}`, `Media/Photos/{name}`, `Machines/{origin}/…`), each kept whole on one folder.

- **reserve**: room left free on that drive (default: 5 % of the drive, at least 20 GB).
- **takes**: the top-level places it accepts (empty: anything). A media drive takes `Media`; a mail server's disk takes `Mail`.
- **One copy**: content already anywhere in the pool (same SHA-256) is never copied into it again. A thing whose files are partly in the pool already gets only what's missing, next to its own place.

## 4. The wizard: a new drive into the pool

On the machine the drive is plugged into, or from any machine that reaches it: the Swarm view offers "Into the pool…" on other machines' drives too, and then the plan is made, changed and carried out *there*, by that machine's own BackDown (`swarm-stdio`'s `plan`, `choose`, `send` and `progress`), and only shown here:

1. **Index and hash** the drive (the usual jobs). The pool machines are synced, and what could be a copy there (same size) is hashed there and here, so "already in the pool" is decided by SHA-256, never by name or size.
2. **What's already in the pool**: per thing on the drive (the recognisers' units: a mail store, a Time Machine set, a photo library, a folder), how much is there already and where, how much is new.
3. **Where the rest goes** (`pool::place`): the filing plan (`plan::make`, with the pool as its archive) gives each thing a place in the tree; each is assigned to one pool folder:
   1. where the same place already is (more mail of example.org goes where `Mail/example.org` lives), if it fits;
   2. else a folder that takes its top-level place;
   3. else the folder with the most room;
   never past a folder's reserve, never splitting a thing; whatever fits nowhere is listed with the room it needs.
4. **Review**: per pool folder, what goes in and the room after; per thing, where and why. Move a thing to another folder, or leave it out.
5. **Copy**: a job on the drive's machine. Into a pool folder there, the usual verified copy (`apply`: temporary name, flushed, renamed, read back, SHA-256 compared, journaled, never overwriting). Into a pool folder on another machine, streamed over one ssh connection to `backdown swarm-stdio` there (§5), which does the same on its side and answers with the hash it read back. Resumable; stoppable; progress in Activity.
6. **Indexed there**: each machine that received files rescans its pool folder and takes the hashes it verified while writing (no second read); this machine syncs it.
7. **Free up the drive** (offered right away): Free up's usual assessment of the drive, every file proved by a copy (here, or in the pool on another machine, re-read there just before deleting), and its typed confirmation. Nothing is deleted without it.

## 5. Writing to another machine: `backdown swarm-stdio`

Run by ssh on the receiving machine, like `serve-stdio`, as the person whose keys reach it, against the index `remote add --remote-db` named. JSON lines in and out; a file's bytes follow its request line raw, `size` bytes exactly, so nothing is base64'd and one connection carries every file.

| request | does |
|---|---|
| `hello {from}` | who's sending (a machine's name); answers with the protocol, version and its pool folders with their room now |
| `put {area, path, size, mtime, src, plan}` + raw bytes | `pool::receive::put`: written beside its place, flushed, timed, renamed into place, read back; answers `copied`, `renamed` (something else was at its place: a numbered name) or `same` (that content was there), with the SHA-256 read back |
| `done {area}` | everything for that pool folder has come: its BackDown service indexes it, or a `backdown index` is started there now |
| `plan {root}` / `plan-get {plan}` | a pool plan for one of its own drives, made there with its schema and its own view of the pool (another machine's Swarm view shows it) |
| `choose {plan, i, to\|skip}` | the person's choice for one thing, by that machine's numbers |
| `send {plan}` / `progress {plan}` | carry it out there (queued in its service, or a `backdown pool send` started now), and how it's going |

Rules on the receiving side, whoever asks: the place is plain names inside one of its own pool folders (no `..`, absolute paths or control characters), never reached through a link that leads out of the folder; nothing is overwritten or deleted; the folder's reserve is kept; a refused file's bytes are still read, so the stream stays in step; each file is recorded (`pool_received`, so the next index takes its hashes instead of reading it again) and is an event in that machine's chain of custody, from `machine:path` by `pool:N`. The sender (`pool::send`) compares the hash read back there with the hash of what it read here, keeps a journal (`pool_log`) so a stopped run carries on where it was, sends nothing that's in the pool by then or isn't as its scan saw it, and records the SHA-256 of what it sent in its own index, so freeing the drive afterwards needn't read it again.

A pool folder on the sending machine itself goes through the very same `receive::put`, in-process.

## 6. Phases

| | What | State |
|---|---|---|
| A | Storage and free space per machine (`volumes` request, cached), the Swarm view and `backdown swarm`, where-is search across machines, the tier change | done (#40) |
| B | Pool folders (`backdown pool add\|list\|set\|remove`), the pool in the Swarm view, the placement planner (`backdown pool plan`, kept plans) and the wizard's first four steps | done (#46) |
| C | `swarm-stdio`, `backdown pool send` and the service's copy job, hash adoption after the rescan, the wizard's copy and free-up steps | done (#56) |
| D | Starting a drive on another machine into the pool from here (the wizard over `swarm-stdio`) | building (`feat/swarm-remote-wizard`) |
| E | `swarm join` (add a machine and the ones it reaches), a second copy as an option (mirrors), a rescan of a big pool drive limited to its pool folder, freeing another machine's drive from here | later |

## 7. Not doing

- A cluster file system or a mount of the pool: the pool is a catalog over ordinary folders, browsed in BackDown and readable without it.
- Erasure coding or automatic rebalancing between pool folders.
- Anything through backdown.io: the swarm is the person's machines and their ssh keys.
