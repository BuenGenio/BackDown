---
title: People, databases, and searching inside things
description: See everyone and every database your drives hold, gathered across drives from mail stores, Nextcloud data, home folders, site configs and dumps; then search inside mail and databases, reusing the indexes already there.
order: 9.7
---

A drawer of old drives holds the same people and the same databases many times over: Alice's mailbox in the mail server's store, her Nextcloud account on a later backup, her home folder in a system image; the shop's database dumped every night for years, its live data directory in a server backup, and the WordPress site that used it. BackDown gathers each into one **person** and one **database**, across drives, and lets you search inside the mail and the databases themselves.

Both come from [what's recognised](/docs/concepts/#recognised-things-and-provenance), so index and hash the drives first; the background service does the rest after every scan.

## People

```sh
backdown people
backdown people alice
```

```text
Alice Example
  Email addresses: alice@example.org
  Logins: alice on cloud.example.org
  Mailboxes: 1, 12,040 messages, 4.1 GB, 2011–2020
  Nextcloud accounts: 1, 8.2 GB
  Home folders: 1, 1.3 GB
  Drives: 3
  ↳ Mailbox            12,040 messages                          BD-500:backups/mail/vmail/example.org/alice/Maildir
  ↳ Nextcloud account  8.2 GB of files on cloud.example.org    NAS:cloud/data/alice
  ↳ Home folder        Linux home on tomahawk                   OLD-LAPTOP:home/alice
```

A person is found in:

- a **mail store**: `<domain>/<user>` under a Dovecot or mailcow store becomes `user@domain`; a Maildir of its own names its user;
- a **Nextcloud data directory**: every user folder with `files/` in it; when the server's `config/config.php` is on the drive too, its `mail_domain` gives the address and its `trusted_domains` the instance;
- a **home folder** in a system image, with the machine's host name;
- a **Thunderbird profile**: the identities (names and addresses) in `prefs.js`;
- a **website's configuration**: the contact a Joomla `configuration.php` or Laravel `.env` names.

The same email address, or the same login on the same host, anywhere is the same person; a name alone never joins two people. Each person shows what there is of them, in which places, on which drives. In the app, **What's here** has a **People** tab with the same, each place a link into the browser.

Nothing is read from inside messages or files for this: names come from paths and from the small configuration files above, on connected drives only.

## Databases

```sh
backdown databases
backdown databases shop
```

```text
shop
  Engine: MariaDB
  Dumps: 3, 2.1 GB, 2019–2021
  Live data: 1, 2 tables, 480 MB
  Used by: WordPress site shop.example.org
  ↳ Dump         MariaDB, dumped 4 Mar 2021          BD-500:backups/db/shop_2021-03-04.sql
  ↳ Dump         MariaDB, dumped 5 Mar 2021          BD-500:backups/db/shop_2021-03-05.sql
  ↳ Live data    MariaDB, 2 tables                   OLD-SERVER:var/lib/mysql/shop
  ↳ Used by      WordPress site shop.example.org     OLD-SERVER:srv/www/shop.example.org
```

A database is gathered from:

- **dumps**: `mysqldump` and MariaDB, phpMyAdmin, `pg_dump` and `pg_dumpall`, SQLite's `.dump`, plain or compressed; three or more in one folder are one thing. The header names the engine, the database and the host when the drive is connected; otherwise the file name without its date does ("shop_2021-03-04" is `shop`);
- **mongodump folders** (`dump/<database>/<collection>.bson`);
- **live data directories** of MySQL or MariaDB (each folder with table files is a database), and PostgreSQL and MongoDB data directories as things of their own;
- **SQLite files**, by their first bytes when the drive is connected; a program's own `.db` files inside a profile, home or phone backup don't count;
- **what uses it**: a WordPress, Joomla or Laravel site's configuration, and a Nextcloud server's, name the database, its engine and host.

Names are joined within an engine family: a MariaDB dump of `shop` and a MySQL data directory of `shop` are the same database; a PostgreSQL `shop` is another. **What's here** has a **Databases** tab.

## Search inside mail and databases

The search box finds files by name. Its third scope, **Inside**, and `backdown search`, find what's in them:

```sh
backdown search invoice 2021
backdown search "garden work" --kind message
backdown search customers --kind table
```

```text
3 matches (2 ms)
  message  Invoice for the garden work      Bob <bob@example.org> → alice@example.org   14 Jul 2017
           BD-500:backups/mail/vmail/example.org/alice/Maildir/cur/1500000000.M1P1.host:2,S
  table    customers                        shop
           BD-500:backups/db/shop_2021-03-04.sql (customers)
```

What's indexed:

- **mail**: every message's subject, sender, recipients and date, from Maildirs, mail stores, Dovecot mdbox stores and Thunderbird mbox files;
- **dumps and SQLite files**: every table with its columns, and which columns the database itself indexed for full text (`FULLTEXT KEY`); mongodump collections and MySQL data directories by table name;
- at **full depth**, the text in the rows of dumps and SQLite tables (text-typed columns only, up to 64 MiB a file) and message bodies.

A hit opens the mailbox folder with the message selected, or the dump or file.

### The indexes already there are reused

- **Dovecot's own index.** A Dovecot mailbox keeps the headers it cached for IMAP clients in `dovecot.index.cache`. When that file matches the mailbox's `dovecot.index`, BackDown reads the subjects, senders, recipients and dates from it and opens no message: a mailbox of a million messages is a few hundred megabytes read once. Messages the cache doesn't cover are read from their files, headers only. A Dovecot full-text plugin's own index (`fts-flatcurve`, `fts-xapian`, `fts-lucene`, Squat) is noted, but can't be read.
- **SQLite's full-text tables.** A SQLite file with FTS3, FTS4 or FTS5 tables is searched in place, through SQLite itself: the hit says how many rows match in which table.
- **What a dump says it indexed.** A MySQL or MariaDB table's `FULLTEXT KEY` names the columns the database indexed for search; they're listed with the table. The engine's own index files in a data directory aren't readable outside the server.

### Depth, and building the index

```sh
backdown search                      # what's indexed, per drive and source
backdown search --build              # index now what was scanned since
backdown search --depth full --build # the text in rows and bodies too, from now on
backdown search --depth off          # stop indexing after scans
```

`headers`, the default, is enough to find a message or a table and costs little: the index keeps no copy of the text, only where each hit is, so a hit shows the subject, who and when rather than a snippet. `full` grows with the data. The background service indexes after each recognition, only drives scanned since; the Activity view shows it, and the empty search result has **Index contents now**.
