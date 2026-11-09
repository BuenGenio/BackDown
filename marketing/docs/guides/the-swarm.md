---
title: The swarm and the pool
description: Every machine you run BackDown on as one: see each machine's drives and free space, find a file wherever it is, declare pool folders on any of them, and pour an old drive into the pool from any machine, each file copied once and verified, then free the drive.
order: 5.5
seo_title: The swarm: every machine's drives as one, with a shared pool
---

You have a stack of old drives, a NAS, a server or two, and a desktop. Each runs BackDown and knows its own drives. The **swarm** is those machines working as one: every machine can say which machine and which drive holds a file, how much room there is everywhere, and the **pool**, folders on any of their drives, becomes one filed, indexed archive that any drive can be poured into, from any machine, with each file copied once, read back and compared by SHA-256, and the old drive then freed.

The swarm is free (Community), and so, with it, is reaching other machines.

## What a swarm is

There is no membership, coordinator or new service. A swarm is **the machines this BackDown reaches over ssh, plus itself**. Adding a machine with `backdown remote add` is joining; see [Several machines](/docs/guides/several-machines/) for the ssh setup, syncing, and how copies there count. Each machine shows the swarm it can see, so an always-on server that reaches every other machine is the natural place to look from, but nothing depends on it.

Each machine describes itself: its drives and their free space, its indexed drives, and its pool folders. The others learn that at each sync and refresh it live when a view opens. Nothing new listens on the network: reading another machine goes through ssh to `backdown serve-stdio` as before, and writing into its pool goes through ssh to `backdown swarm-stdio`, which writes only inside that machine's own pool folders. No machine's index is touched by another: files that arrive in a pool folder are indexed by that machine's own BackDown.

## See the swarm

In the app, the sidebar's **Swarm** view shows each machine, this one first and live, the others as last synced and then refreshed as each answers: every drive as a bar of used and free space, what's indexed on it, its health where known, drives that aren't connected and where they were last scanned, and the totals. In a terminal:

```sh
backdown swarm
backdown swarm --cached     # don't ask the other machines now
```

A drive is known by its physical identity (model, serial, the volume's UUID), not its mount path, so a drive that moves between machines is the same drive: "indexed on tomahawk last week, now on the NAS".

<figure class="demo" data-view="swarm" data-caption="The Swarm view: every machine’s drives, how full they are, what’s indexed on them, and the pool folders."></figure>

## Find a file wherever it is

```sh
backdown swarm where tax return 2019
```

Every machine's name index is asked at once, live where it can be reached and from what was synced where it can't. Each hit says the machine, the drive (label, model, serial), the path, and how fresh that answer is. The Swarm view's search box does the same, and so does the main search box's **Everywhere**. What was synced of another machine includes its folders too, worked out from its files' paths.

## Declare pool folders

A pool folder is a folder on a drive of one machine, declared **on that machine**; only that machine can add, change or remove it, and the others learn of it at their next sync.

```sh
backdown pool add /srv/pool --reserve 50G --takes Mail,Media
backdown pool list
backdown pool set /srv/pool --takes Mail
backdown pool remove /srv/pool    # nothing in it is touched
```

- **reserve** is the room kept free on that drive: a size or a share, 5 % of the drive and at least 20 GB unless said.
- **takes** lists the top-level places the folder accepts, from the [filing schema](/docs/guides/file-into-an-archive/#2-make-a-plan): `Mail`, `Media`, `Machines`, `Phones`, `Servers`, `People`… A media drive takes `Media`; the mail server's disk takes `Mail`. Empty means anything.

Together the pool folders are one logical tree. Each place in it (`Mail/example.org`, `Media/Photos/Holidays 2019`) is kept whole on one folder, and the pool keeps **one copy** of each file: content already anywhere in the pool, on any machine, is never copied into it again. A second copy stays a matter for [mirrors](/docs/guides/mirrors/).

## Pour a drive into the pool

Plug the drive into any machine of the swarm. In the Swarm view, the drive has **Into the pool…**; on another machine's drive it's offered too, and the plan is then made, changed and carried out *there*, by that machine's own BackDown, and only shown here. In a terminal, on the drive's machine:

```sh
backdown index /media/old-drive && backdown hash
backdown pool plan /media/old-drive
```

The plan goes through the things BackDown [recognises](/docs/concepts/#recognised-things-and-provenance) on the drive, a mail store, a Time Machine set, a photo library, a folder, and for each says:

1. **what's in the pool already**: how much of it, by SHA-256, and where (the pool machines are synced first, and files there that could be copies are hashed there and here, so this is never decided by name or size);
2. **what's new**, and the place the filing plan gives it;
3. **which pool folder it goes into**: the folder where that place already lives, if it fits; else a folder that takes its top-level place; else the folder with the most room; never past a folder's reserve, never splitting a thing. Whatever fits nowhere is listed with the room it needs.

Read it, move a thing to another folder or leave it out, then:

```sh
backdown pool plans
backdown pool send 3
```

Into a pool folder on this machine, that's the usual verified copy: written under a temporary name, flushed, renamed into place, read back and compared by SHA-256, journaled, never overwriting. Into a pool folder on another machine, every file streams over one ssh connection to `backdown swarm-stdio` there, which does the same on its side and answers with the hash it read back; the sender compares it with what it read here. It's resumable and stoppable, and shows in Activity. Each machine that received files rescans its pool folder and takes the hashes it verified while writing, so nothing is read twice; this machine syncs it.

When the copy is done, the wizard offers to **free up the drive**: the usual [Free up](/docs/guides/free-up-a-drive/) assessment, every file proved by a copy here or in the pool on another machine (re-read there just before deleting), and its typed confirmation. Nothing is deleted without it.

## What the receiving machine allows

Whoever asks, `swarm-stdio` on the receiving machine writes only inside its own pool folders, to plain names (no `..`, absolute paths or control characters, never through a link that leads out of the folder); it never overwrites or deletes; it keeps the folder's reserve; a refused file's bytes are still read so the stream stays in step; and every file it takes is an event in that machine's [chain of custody](/docs/guides/chain-of-custody/), from `machine:path`.

## Not yet

- `backdown swarm join`, which would add a machine and the machines it reaches in one go: for now, add each machine with `backdown remote add`.
- Freeing another machine's drive from here: start it on that machine.
- A machine running Windows can't be the other end yet.
