---
title: Merge mailboxes
description: Turn copies of the same mailbox taken at different times into one Maildir, every message once and in the right folder, and export a folder as mbox. The copies are only read.
order: 3
---

Old drives often hold the same mailbox several times: a laptop's mail from 2016, a server backup from 2019, Thunderbird's local folders from last year. Each has most of the same messages, and each has a few the others don't. BackDown merges them into one Maildir with every message once, by Message-ID, in the folder the newest copy has it in, with the read, replied and flagged marks of all of them. The copies are only read.

## What it reads

- **Maildir**, including Maildir++ subfolders (`.Sent`, `.Archives.2019`) and Dovecot's folder-per-folder layout;
- **mbox files**, one per folder;
- a **Thunderbird account folder** (`Mail/Local Folders` or `ImapMail/<server>` in a profile): its mbox files and their `.sbd` subfolders;
- **whole mail stores**, folders of Maildirs such as a mail server's `domain/user/Maildir`, merged mailbox by mailbox.

Give either mailboxes or stores, not a mix. Dovecot's mdbox and sdbox formats aren't read yet.

## See what the merge would be

```sh
backdown mail merge /media/old-drive/Mail/ann /media/laptop-2012/Thunderbird/Mail/Local\ Folders /media/archive/Backups/mail-2021/ann/Maildir \
  --to /media/archive/Archive/Mail/ann --dry-run
```

```text
Snapshots, oldest first:
  1. /media/old-drive/Mail/ann (Maildir): 6.2 K messages, the newest from 1 Jun 2019
     only in this one: 212 messages
     folders only here: Old
  2. /media/laptop-2012/Thunderbird/Mail/Local Folders (mbox folder): 5.9 K messages, the newest from 3 Mar 2020
  3. /media/archive/Backups/mail-2021/ann/Maildir (Maildir): 6.8 K messages, the newest from 9 Feb 2021
     folders only here: Archive
Would merge 7.3 K distinct messages (1.4 GB), in 4 folders; 11.6 K duplicate copies left out
  without a Message-ID (matched by headers and body): 38
  in another folder in an older snapshot (placed where the newest has them): 144
  copies differing only in headers a server or mail program added (the newest kept): 902
  flags combined from several copies (read, replied or flagged in any): 1.1 K
  Folders: Archive 1.2 K · INBOX 5.6 K · Old 212 · Sent 310

Nothing was written. Without --dry-run, the merged mailbox goes to /media/archive/Archive/Mail/ann
```

The dry run reads every message and writes nothing. The copies are ordered by their newest message, whatever order you name them in.

## How messages are matched and placed

- Two copies are the same message when they have the same Message-ID and the same body. The same Message-ID with a different body is kept twice. A message without a Message-ID is matched by its headers and body, leaving out headers that servers and mail programs add or change (status, UIDs, keywords and the like).
- Each message goes in the folder the newest copy that has it keeps it in, so messages you filed later end up filed. One that was deleted since stays in the folder it was in. If the newest copy has it in two folders, it's in both.
- The message written is the newest copy's, exactly as stored (from an mbox, without its `From ` line and quoting).
- Read, replied, flagged and forwarded marks are combined from every copy; draft and trashed come from the copy that's kept.

In a mail store, messages are matched mailbox by mailbox: the same message in two people's mailboxes stays in both.

## Merge

```sh
backdown mail merge /media/old-drive/Mail/ann /media/laptop-2012/Thunderbird/Mail/Local\ Folders /media/archive/Backups/mail-2021/ann/Maildir \
  --to /media/archive/Archive/Mail/ann
```

The destination must be a new or empty folder (or one this merge already started). It becomes a Maildir++ mailbox: the inbox is the folder itself, and other folders are `.Name` inside it (`.Archives.2019` for nested ones), the layout Dovecot and Courier use, and most mail servers and programs that read Maildir understand. Each message is written under a temporary name, flushed to disk, moved into place, then read back and compared, and journaled in BackDown's index. An interrupted merge carries on where it stopped when you run the same command again. It ends with "Written and verified", and exits with 2 if any message couldn't be read or written.

With the background service running, the merge is handed to it (it shows in Activity as "Merge mailboxes"), and the destination's drive is indexed afterwards.

## In a filing plan

A [filing plan](/docs/guides/file-into-an-archive/) notices mailboxes of the same origin that are at least 90% the same (the same Maildir or mail store on two old drives, say). By default it files one and copies only the other's extra files beside it, into `Also from <drive>/…`. To merge them instead:

- `backdown plan --to /media/archive/Archive --merge-mail` merges every such pair;
- in the Plan view, **Merge these mailboxes** on an item does it for that one, before the plan is applied.

Applying the plan then writes one merged mailbox for each ("Merge snapshots of a mailbox (by Message-ID)" in the plan), store by store and mailbox by mailbox. The plan's dry run doesn't read the messages; `backdown mail merge … --dry-run` on the same folders shows what the merge will be.

## Export a folder as mbox

```sh
backdown mail export /media/archive/Archive/Mail/ann/.Sent --mbox /media/archive/Exports/ann-sent.mbox
```

```text
exported 310 messages (48 MB) to /media/archive/Exports/ann-sent.mbox as mboxrd, read back and verified
Only this folder's messages (not its subfolders); read, replied and flagged marks aren't kept: mbox has no standard place for them.
```

It writes one Maildir folder's own messages, oldest first, as an mbox file (the mboxrd kind, which quotes `From ` lines so they come back exactly). The file must not exist yet, and can't be inside the Maildir. It's written under a temporary name and read back before it takes its name.

`backdown mail` is free; merging in a plan is part of the filing plan, which is marked Pro ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)).
