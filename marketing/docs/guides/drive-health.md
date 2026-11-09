---
title: Drive health
description: What each drive says about itself, what BackDown can read of it on Linux, macOS and Windows, the warnings and trends it keeps, notifications, other machines' drives gathered here, and what to do when a drive is failing.
order: 6
---

Drives keep a record of how they're doing: sectors they've had to replace, sectors they can't read, errors on the cable, how much of an SSD's write endurance is used, temperature, and their own overall verdict (SMART). BackDown reads it for every physical drive, keeps a history of it, and tells you when a drive gets worse, so that you copy what's on it while it still reads.

It only reads. It never runs the drive's own self-tests, and never changes a setting on it.

## See every drive

In the app, Activity's "Connected drives" has a pill beside each disk (**healthy**, **warning**, **failing** or **no health data**), and the sidebar marks an indexed drive with a warning or failing dot. Click a pill for the details. In a terminal:

```sh
backdown health
```

```text
DRIVE      MODEL                    SERIAL             HEALTH    TEMP   HOURS REALLOC  PEND   UNC   CRC  WEAR  SINCE FIRST READING
sdc        WDC WD5000AAKX-00ERMA0   WD-WCC000000000    warning   36°C  20.0 K       6     2     0     0     -  29 Sep 2026: +6 reallocated, +2 pending
nvme0n1    Samsung SSD 970 EVO 1TB  S000NX0000000000   ok        35°C    4.1 K      -     -     -     -    3%  first reading

sdc: 6 sectors had gone bad and have been replaced by spares (reallocated)
sdc: 2 sectors can't be read and are waiting to be replaced (pending)
```

The columns are the temperature, hours powered on, reallocated, pending and uncorrectable sectors, cable (CRC) errors, and wear, then what changed since BackDown first read the drive. Lines after the table explain each finding, and any `note:` says what couldn't be read and how to read more. `--json` gives everything, history included.

`backdown health` exits with 2 when a connected drive is failing, so a script can watch for it. With the background service running (on Linux and macOS), it asks the service to read every drive now; otherwise it reads them itself.

<figure class="demo" data-view="activity" data-caption="Connected drives in Activity: a pill beside each disk — healthy, warning, failing or no health data — and a page per drive with its readings and trend."></figure>

## What BackDown can read

Everything is read as you, without administrator rights. What an ordinary account is told depends on the system:

| System | How | What it gets |
|---|---|---|
| Linux | udisks, over the system bus | Most SATA drives' verdict, temperature, hours and bad-sector counts, and NVMe drives' health (with udisks 2.10 or later). With `busctl` installed, reallocated and pending sectors separately; without it, together |
| Linux | `smartctl`, if it's installed and your account may open the drive (usually only root or the `disk` group) | Everything the drive reports, on top of udisks |
| Linux | sysfs | A temperature, where nothing else gave one |
| macOS | `diskutil` | The drive's verdict and, for NVMe drives on recent macOS, wear, spare blocks, media errors, temperature and hours |
| macOS | `smartctl`, if installed (`brew install smartmontools`) | More drives, NVMe fully, USB where the enclosure passes it on |
| Windows | the storage cmdlets | Every drive's overall health, to everyone; its temperature, wear and error counters only to an administrator |

What it can't read, it says, with the command that would, for example:

- "needs administrator rights: sudo smartctl -a /dev/sdc";
- "its USB enclosure doesn't pass health data on", which is common: many USB cases and docks don't;
- on Windows, "Windows shares this drive's error counters only with an administrator": running `backdown health` once in an administrator terminal reads them.

`smartctl` is asked not to wake a drive that has spun down; a sleeping drive keeps its last reading until it's awake. udisks answers from what it already knows.

## Verdicts and warnings

A drive is **failing** when it says so itself, when it marks an attribute as failing now, when an NVMe drive's spare blocks fall below its own limit or it reports media errors that degrade it, or when bad sectors pile up (100 or more reallocated, or 10 or more pending or uncorrectable). It's a **warning** for any reallocated, pending or uncorrectable sector, unrecoverable media errors, spare blocks running low, 90% or more of an SSD's rated write endurance used, or running hot (55 °C for a spinning disk, 70 °C for others). Cable errors alone don't change the verdict: they usually mean a cable or an enclosure, not the drive. **Unknown** means there's no health data at all.

## History and trends

Each reading is kept in the index under the drive's serial number, so its history follows it from one port or enclosure to another: whenever something changed, and at least once every 20 hours otherwise. The details show the counters, small charts of temperature, bad sectors, wear, cable and media errors since the first reading, what changed since then ("+6 reallocated"), and a list of what changed and when.

The background service reads every connected drive 20 seconds after it starts, then every six hours, when a disk is plugged in (on Linux and macOS), when an indexed drive comes back, and when you press **Check now**.

## Notifications

When a reading is worse than the one before, the service tells you, in Activity's Notifications and, unless you've turned them off, on your desktop:

| Notice | When |
|---|---|
| "*drive* is failing" | its verdict became failing |
| "*drive* needs watching" | its verdict became warning |
| "*drive* is getting worse" | with the verdict unchanged: more bad sectors or media errors, wear crossed 80, 90 or 100%, or it ran hot (it reached the hot mark, 10 °C or more above the last reading) |
| "Transfer errors on *drive*" | only its cable errors rose: "check or replace the cable or enclosure; the drive itself may be fine" |

A failing drive's notice always reaches the desktop, however many others were shown. If a mirror's copy is on that drive, the notice says so: "this mirror drive is dying, add another mirror". [Notifications](/docs/guides/settings-and-licences/#notifications) has the settings and the limits.

## Other machines' drives {#other-machines-drives}

With [other machines](/docs/guides/several-machines/) added, BackDown here also gathers their drives' health, so a NAS or a computer nobody sits at can still warn you. Each machine keeps reading its own drives (its own background service, or `backdown health` there); every sync of it (every 6 hours with the background service, or `backdown remote sync`) fetches what it last found: each drive's verdict, its readings and latest changes, and that machine's own latest drive-health notifications.

That's read from the other machine's index, over the same ssh connection as the rest of the sync, with your own keys: nothing goes through any server, no drive there is read again for it, and nothing there is changed.

In the app, Activity's **Other machines' drives** lists each machine with when it was last synced, and its drives with a pill each (click it for the details) and how old each reading is. In a terminal:

```sh
backdown health --machines
```

```text
nas (ssh://you@nas.example.org) · synced 2 h ago
  HEALTH   MODEL                    SERIAL              TEMP   HOURS REALLOC  PEND   UNC   CRC  WEAR  READ THERE
  warning  WDC WD40EFRX-68N32N0     WD-WCC0000000000    38°C  31.2 K       8     0     0     0     -  3 h ago
  ok       Samsung SSD 870 EVO 1TB  S000NX0000000000    31°C   9.0 K       -     -     -     -    2%  3 h ago
  WDC WD40EFRX-68N32N0: 8 sectors had gone bad and have been replaced by spares (reallocated)
  notified on nas 2 days ago: WDC WD40EFRX-68N32N0 needs watching

old-pc (ssh://you@old-pc.example.org) · synced 5 h ago
  update BackDown on old-pc to see its drive health
```

It shows what the last sync fetched, without reaching any machine. `--json` gives everything. It exits with 2 when a drive there is failing, so a script can watch a whole fleet. A drive that machine hadn't read for over a week before the sync (taken out, or its BackDown isn't running) is still listed, marked †.

When a drive there is worse than at the last sync (ok, then warning, then failing), you're told here as you would be about a drive of this computer's: "A drive on nas needs watching" or "A drive on nas is failing", with what changed in its readings. Like other drive-health notices, it's listed in Activity's Notifications and pops up on the desktop if those do; a failing one always reaches the desktop. The first sync of a machine never notifies (that's when BackDown first learns how its drives are), and the same news isn't told twice within a week.

A BackDown on the other machine older than this feature doesn't know how to send its drive health. Its sync goes on as before, and it says "update BackDown on old-pc to see its drive health".

Gathering other machines' drive health is part of BackDown **Team**; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)). Each machine's own readings, warnings and notifications are free. The machine-wide service (`backdown service install --system`) doesn't sync other machines, as it has no ssh keys of yours, so it doesn't gather their drive health either: your own BackDown does.

## When a drive is failing

A connected drive that's failing gets a banner across the app: what's wrong, and **Copy it now**. It opens [Rescue](/docs/guides/rescue/) with that drive chosen and "It's failing" selected, so the next step is choosing where the copy goes (another drive: it suggests `BackDown Rescue/<drive> <date>.img` on one of your indexed drives) and pressing **Start copying**. A failing drive can stop reading at any moment, and every read wears it further: copy it once, the good areas first, then look for files in the copy rather than on the drive.

If the drive only needs watching, make sure what's on it has a copy elsewhere: `backdown check` on it says whether it does ([what "safe to wipe" means](/docs/concepts/#what-safe-to-wipe-means)).

## Not yet

BackDown doesn't start SMART self-tests or read their logs. It doesn't read another machine's drives itself either: each machine's BackDown reads its own, and this one only [gathers what they found](#other-machines-drives).
