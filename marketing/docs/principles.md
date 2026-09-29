---
title: Privacy and safety principles
nav: Principles
description: The five rules BackDown is built on, and how the program keeps them.
order: 2
group: help
---

Five rules don't change from one version to the next. This page says what each one means in practice, in the program you can download.

## 1. Never write to a source drive

The drives BackDown looks at are only read. Indexing reads names, sizes and dates; hashing, recognising, checking and browsing read contents; none of them write. The browser only reads too: everything that changes something goes through the daemon, and happens only when you ask.

When BackDown does write, it's to a place you named, never to what it's reading from:

- copying into an archive writes into the archive folder, and never overwrites a file that's already there;
- a mirror sync writes to the mirror, and moves a changed file's old copy aside rather than replacing it;
- Rescue writes the image and the files it finds to another drive, and refuses to write onto the drive being rescued.

On Linux, the machine-wide service makes this a rule the system enforces: it runs with the whole file system read-only to it, except its own folder.

The one exception is the one you ask for: freeing up a drive deletes files from it, under rule 2.

## 2. Never delete without a verified copy

BackDown deletes in one place, [freeing up a drive](/docs/guides/free-up-a-drive/), and only a file that:

- is still what the index says it is;
- has an identical copy outside what you're freeing, and both have just been read again and compared (or, if you choose, the copy is still there exactly as last scanned);
- or has just been moved, with the copy read back and checked first.

Every deletion is written to a journal with its proof: which copy, its SHA-256, and how it was checked. It refuses the running system, whole home folders and its own index, shows what would happen before anything does, and asks you to type the drive names.

## 3. Every long job can resume, and shows why it's slow

Drives drop off USB buses and computers restart. Scans and hashing save as they go, copying keeps a journal, and a drive image keeps a map of what's been read, so running the same command again carries on where it stopped, and the daemon can be stopped at any time. The terminal and the browser's Activity view show what's running, how far it is, what's waiting, and which files wait for a drive that isn't connected.

## 4. The free edition answers "what do I have, and what's duplicated?"

Knowing what's on your drives and what's duplicated is the free part of BackDown, and it's complete: every drive indexed, duplicates found across all of them, a SHA-256 behind every "this has a copy". The command-line preview is free. [Pricing](/pricing/) shows what's planned for the paid tiers.

## 5. Local first

BackDown runs on your computer and needs no account. It sends no telemetry and makes no network requests: the only connections it makes are to itself, on 127.0.0.1. The browser listens on 127.0.0.1 only, answers only its private link, and loads nothing from elsewhere. A Pro licence key is checked on your computer, by its signature. On Linux, the machine-wide service is cut off from the network beyond this computer by the system.

This is about the program. The website, backdown.io, is covered by its [privacy policy](/privacy/).
