---
title: BackDown and backup tools
nav: Backup tools
seo_title: BackDown vs rsync, restic, borg and Time Machine
description: Backup tools copy what you have now into a repository, and restore from it later. BackDown starts from the drives and backups you already have: it indexes them, proves what's duplicated, files old drives into one plain archive, keeps verified mirrors and tells you which drives are safe to wipe. They work well together.
order: 2
---

A backup tool answers *if this disk dies tomorrow, can I get my files back?* BackDown answers a different question: *what is on all these drives, which of it is a copy of what, and which drives can I let go?* Most people who have the second question also have years of backups made with the first kind of tool, and BackDown treats those as drives to index, not as competition.

## What the backup tools do

- **rsync** copies a tree to another place and keeps it in step. Plain files at the other end; no history unless you build it with `--link-dest` or snapshots.
- **restic, borg and kopia** store backups in their own repositories: deduplicated in chunks, compressed, encrypted, with a history of snapshots. Restoring means running the tool; the repository isn't browsable with a file manager.
- **Time Machine** keeps hourly, daily and weekly snapshots of a Mac on an external drive or a network share, each a full-looking copy, and restores through its own interface.
- **Cloud backup services** copy to someone else's computer, encrypted or not, on a subscription.

## What BackDown does, and doesn't

**It isn't a backup repository.** BackDown doesn't chunk, encrypt or version your files into a store of its own. Where it copies, it copies files as files: the [archive](/docs/guides/file-into-an-archive/) a plan fills is ordinary folders, and a [mirror](/docs/guides/mirrors/) is an ordinary copy, verified by re-reading it. Anything BackDown writes can be read with any tool, forever.

**It reads the backups you already have.** BackDown recognises Time Machine sets, rsync copies, phone backups and the rest for what they are, and lets you [browse them without restoring](/docs/guides/browse-without-restoring/): any folder at any scan, versions side by side, a mailbox's threads or a database dump's tables opened in place.

**It proves instead of promising.** Every backup tool verifies its own repository. BackDown verifies across everything: a file on an old drive is "backed up" if an identical copy — same SHA-256 — exists on any other drive or machine it knows. That's the proof behind [safe to wipe](/docs/concepts/#what-safe-to-wipe-means), and it holds whether the copy was made by BackDown, by rsync, by Time Machine or by hand.

**It keeps a record.** Every copy, move, merge and deletion BackDown makes goes into a hash-chained, signed [chain of custody](/docs/guides/chain-of-custody/), and a drive's contents at a scan can be sealed in a [signed manifest](/docs/guides/manifests-and-attestations/). Backup tools log; BackDown attests.

**It watches the hardware.** [Drive health](/docs/guides/drive-health/) reads each disk's own diagnostics and warns before a drive fails; [Rescue](/docs/guides/rescue/) images a failing one while it still reads.

## Using both

A sensible setup is a backup tool for tomorrow and BackDown for the past and present: restic or Time Machine running on a schedule for the machine you use every day, and BackDown indexing that backup drive along with the box of old disks, so you know what's where, free the drives that hold nothing unique, and keep one plain, verified archive of everything that matters.

<figure class="demo" data-view="entities" data-caption="What BackDown recognised on the demo drives: a Time Machine set, an iPhone backup, mailboxes, a Nextcloud data folder, libraries — each treated as one thing."></figure>
