---
title: Tag photos by date and place
description: Read the date and position a photo or video recorded, turn the position into country, region, city and nearest airport without going online, and add tags, flags and links of your own. Tags follow a file's content to every copy of it.
order: 9.8
nav: Tags and places
preview: yes
---

Most photos and videos record when and where they were taken. BackDown reads that from the files themselves and turns the position into a country, a region, a city and the nearest airport, from place data built into BackDown: nothing is looked up online. You can add tags of your own (an event, a trip, who it belongs to), mark decisions with flags, and link things together.

A tag goes on a file's **content**, its SHA-256, not on a path. Every copy of the photo, on any drive, carries the same tags, and moving, renaming or deduplicating copies doesn't lose them.

Tags, flags and links are in the `backdown` command for now; the app doesn't show them yet.

## Date and position from the photo

Index the drive first ([Getting started](/docs/getting-started/)), then:

```sh
backdown tag exif ~/Pictures/2019/*.jpg
```

```text
Photos/IMG_0388.jpg: year=2023, date=2023-04-18, location=22.281000,114.158900
Photos/IMG_2041.jpg: year=2019, date=2019-11-02, location=26.100300,-80.128100
```

It reads JPEG, HEIC and TIFF photos and MOV and MP4 videos: the date and time the picture was taken (EXIF `DateTimeOriginal`, or the video's creation date) and the GPS position, if the camera recorded one. Each becomes a tag: the **year**, the **date** under it, and the **location**.

`backdown hash` only reads files that could be duplicates, so most photos have no hash yet. `tag exif` hashes each one it needs, if the file is still exactly as BackDown indexed it; a file that has changed since is skipped with "changed since it was indexed; run `backdown index` again".

## Country, region, city and airport

```sh
backdown tag geo
```

```text
content:066ca40e…: country=Hong Kong, region=Central and Western, city=Central, airport=HHP
content:a80e2fc0…: country=United States, region=Florida, city=Fort Lauderdale, airport=FLL
```

For every piece of content with a location, `tag geo` finds:

- the **country**, and its **region** (a state, province or county);
- the **city**: the nearest town or city of 1,000 people or more;
- the **airport**: the nearest one with a three-letter code, within 120 km.

They nest: the city under its region, the region under its country. The further a photo was taken from the nearest town, the lower the confidence recorded with its city tag. Give `tag geo` some `content:` keys to do only those.

The place data is built into BackDown: about 170,000 places and 9,000 airports. `backdown tag geo --about` says where it comes from:

```text
171806 places, 9053 airports embedded
Place, country and region data © GeoNames (geonames.org), CC BY 4.0 <https://creativecommons.org/licenses/by/4.0/>. Airport data from OurAirports (ourairports.com), public domain.
```

## What a photo carries

A photo's content key is `content:` and its SHA-256, which any SHA-256 tool gives you: `sha256sum` on Linux, `shasum -a 256` on macOS, `Get-FileHash` in PowerShell.

```sh
backdown tag of content:$(sha256sum IMG_2041.jpg | cut -d' ' -f1)
```

```text
airport   FLL  [geo]
city      Fort Lauderdale  [geo]
country   United States  [geo]
date      2019-11-02  [exif]
location  26.100300,-80.128100  [exif]
region    Florida  [geo]
year      2019  [exif]
```

The part in brackets is where each tag came from. `backdown tag list` lists every tag, with `--kind city` (or `country`, `year`, …) to see just one kind, and `--json` for scripts.

## Your own tags

```sh
backdown tag add --kind event --value "Boat show 2019" --to content:a80e2fc0…
```

```text
tag 15 (event=Boat show 2019) attached to content:a80e2fc0…
```

A tag has a kind (`owner`, `kind`, `place`, `event`, `memory`, `album`, or any of the ones above) and a value. `--parent ID` puts it under another tag, as a city is under its country; `--confidence` says how sure you are, from 0 to 1. Without `--to`, it's only created. `backdown tag rm --tag ID --from TARGET` takes it off again.

Tags can also go on a person or a database (`subject:` and `entity:` keys) and on a path (`path:`), for things that aren't one file.

## Flags and links

A flag records a decision about something:

```sh
backdown flag set content:a80e2fc0… keep
backdown flag of content:a80e2fc0…
```

```text
content:a80e2fc0… flagged keep
keep             by user
```

The flags are `canonical`, `duplicate` (with `--canonical TARGET`, the copy to keep), `keep`, `drop`, `reviewed`, `owner-confirmed`, and for people `mine` and `not-mine`. `flag clear` removes one. Each flag remembers who set it and when; `--note` adds why.

A link connects any two things, in one direction, with a relationship you name:

```sh
backdown edge add content:a80e2fc0… taken-at path:/photos/2019
backdown edge from content:a80e2fc0…
backdown edge to path:/photos/2019
```

## What it doesn't do yet

- Tags, flags and links don't show in the app or the browser yet.
- `tag exif` and `tag geo` run when you ask, not after every scan.
- Flags record decisions; nothing acts on them yet. Freeing up a drive still only deletes what has a verified copy elsewhere ([Free up a drive](/docs/guides/free-up-a-drive/)).
- Places are towns of 1,000 people or more, so a photo from somewhere remote gets the nearest such town. The nearest "airport" can be a heliport that has an airport code.
- Trips and events aren't found for you; name them with `tag add`.
