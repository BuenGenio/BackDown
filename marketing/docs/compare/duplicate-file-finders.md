---
title: BackDown and duplicate file finders
nav: Duplicate file finders
seo_title: Duplicates across drives: BackDown vs dupeGuru and fdupes
description: dupeGuru, fdupes, jdupes, rmlint and Czkawka find duplicates in the folders you point them at, while they're connected. BackDown keeps an index of every drive you own, so it finds copies across drives that aren't plugged in, and deletes a file only when a verified copy exists somewhere else.
order: 1
---

Duplicate file finders are good at one job: scan a folder or a drive, group identical files, let you delete the extras. If you have one disk to clean up today, any of the tools below will do it well. BackDown starts from a different question — *what do I have, across all my drives, and which of them can go?* — and that changes what it has to be.

## What the finders do

- **fdupes and jdupes** compare files by size, then by hash, then byte for byte, in the folders you name. Fast, exact, scriptable; jdupes can replace copies with hardlinks. They look at what's mounted when they run.
- **rmlint** finds duplicates and other "lint" (empty files, broken links, duplicate directories) very quickly, and writes a shell script you review before anything is deleted.
- **dupeGuru** adds fuzzy matching: similar file names, music by tags, pictures that look alike even when the bytes differ. A desktop app for Windows, macOS and Linux.
- **Czkawka** (and its Krokiet interface) does the same in Rust: exact duplicates, similar images and videos, big files, empty folders, with a GUI and a CLI.

All of them share two traits. They compare what is connected at the moment they run, and they forget everything when they finish. Run them next month, with a different drive plugged in, and they start from zero.

## What BackDown does differently

**It remembers.** BackDown indexes each drive once (and rescans it when it's back), so the index knows every file on every drive you've ever shown it. A photo on a card you indexed in May is found as a copy of a file on the laptop you index in November, whether or not the card is plugged in. You can see every copy of a file, on which drive, in which folder, as of which scan.

**Every claim has a hash.** Files that could be duplicates are hashed with SHA-256 — a sample first, then the whole file — and "duplicate" means identical content, nothing fuzzier. BackDown doesn't do look-alike matching of photos or music: for that, dupeGuru or Czkawka remain the right tools.

**Deleting is a proof, not a pick.** A duplicate finder lets you choose which copy to keep. BackDown's [free up a drive](/docs/guides/free-up-a-drive/) deletes a file only when an identical copy exists *outside* what you're wiping, re-reads both before deleting, and moves anything that exists only there to a place you chose. The same proof tells you whether a whole drive is [safe to wipe](/docs/concepts/#what-safe-to-wipe-means), and the [chain of custody](/docs/guides/chain-of-custody/) records what was deleted and why.

**It knows what things are.** Three snapshots of a Time Machine backup are full of files that look like duplicates of each other and of your home folder; a Maildir is a thousand small files that are copies of another mailbox's. BackDown [recognises](/docs/concepts/#recognised-things-and-provenance) backups, mailboxes, phone backups, photo libraries, repositories and the like, so it files and deduplicates them as units, and shows you what each drive holds in those terms.

**It works across machines.** Copies on a NAS or another computer running BackDown count too ([several machines](/docs/guides/several-machines/), [the swarm](/docs/guides/the-swarm/)), without copying anything over the network to compare it.

<figure class="demo" data-view="dupes" data-caption="Duplicates across six demo drives, grouped by content; each group lists every copy and where it is."></figure>

## When to use which

- One drive, a tidy-up today: a duplicate finder. Czkawka or dupeGuru if you want a GUI and similar-image matching; rmlint or jdupes if you want a script.
- A pile of old drives, cards and backups, and the question of which can go: BackDown. Index everything, see the overlaps, file what matters into one archive, then wipe with proof.
- Both: nothing stops you. BackDown only reads your drives unless you tell it to copy or wipe, so running another finder on the same disks changes nothing it has recorded; a rescan picks up what the other tool deleted.
