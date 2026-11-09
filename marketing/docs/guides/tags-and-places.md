---
title: Tag and find photos by date and place
description: Read the date and position a photo or video recorded, turn the position into country, region, city and nearest airport without going online, find everything taken within a distance of a place, and add tags, flags and links of your own. Tags follow a file's content to every copy of it.
order: 9.8
nav: Tags and places
---

Most photos and videos record when and where they were taken. BackDown reads that from the files themselves and turns the position into a country, a region, a city and the nearest airport, from place data built into BackDown: nothing is looked up online. You can add tags of your own (an event, a trip, who it belongs to), mark decisions with flags, and link things together.

A tag goes on a file's **content**, its SHA-256, not on a path. Every copy of the photo, on any drive, carries the same tags, and moving, renaming or deduplicating copies doesn't lose them.

Then find them again: everything within 120 km of HKG, in Paris in 2019, from an event you named. In the app that's the **Places** view; in a terminal, `backdown tag find`. Links are in the `backdown` command only.

## Read a drive's photos

Index the drive first ([Getting started](/docs/getting-started/)). In the app, open **Places** and choose **Read dates and places…**: every drive, or one. It runs in the background service, shows how far it is in Places and in Activity, and can be stopped; the photos show up as it goes. Each photo is read once: reading a drive again only reads what's new, and nothing on the drive is touched.

In a terminal, give `tag exif` a folder, and `--geo` to name the places too:

```sh
backdown tag exif /media/photos --geo
```

```text
/media/photos/2019/IMG_2041.jpg: year=2019, date=2019-11-02, location=22.289000,113.941000, country=Hong Kong, region=Islands, city=Tung Chung, airport=HKG
/media/photos/misc/plain.jpg: no date or location in its metadata
…
12 photos and videos: 11 with a date or position (10 with a position), 1 with neither
```

It goes through every indexed photo and video under the folder that wasn't read before (`--again` reads those too). The sections below are the two steps it takes, which can also be run on their own.

## Date and position from the photo

`tag exif` also takes single files:

```sh
backdown tag exif ~/Pictures/2019/*.jpg
```

```text
Photos/IMG_0388.jpg: year=2023, date=2023-04-18, location=22.281000,114.158900
Photos/IMG_2041.jpg: year=2019, date=2019-11-02, location=26.100300,-80.128100
```

It reads JPEG, HEIC, AVIF, PNG, WebP and TIFF photos, the raw formats built on TIFF, CR3, RAF and IIQ, and MP4, MOV, 3GP, MKV and WebM videos: the date and time the picture was taken (EXIF `DateTimeOriginal`, or the video's creation date) and the GPS position, if the camera recorded one. Each becomes a tag: the **year**, the **date** under it, and the **location**.

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

## Find photos by place

```sh
backdown tag find --near HKG
```

```text
Within 120 km of HKG (Hong Kong International Airport, Hong Kong): 7 found
   0.53 km  2019-11-02  Lau Fau Shan, Hong Kong · HKG     /media/photos/2019/IMG_2042.jpg
   3.69 km  2019-11-02  Tung Chung, Hong Kong · HKG       /media/photos/2019/IMG_2041.jpg
   25.3 km  2023-04-18  Central, Hong Kong · HHP          /media/photos/2023/IMG_0388.jpg
   26.5 km  2023-04-19  Tsim Sha Tsui, Hong Kong · HHP    /media/photos/2023/IMG_0390.jpg
   29.6 km  2023-04-21  Gangxia, China · SZX              /media/photos/2023/IMG_0401.jpg
   40.2 km  2023-04-23  Macau, Macao · XZM                /media/photos/2023/IMG_0420.jpg
    113 km  2023-04-25  Guangzhou, China · FUO            /media/photos/2023/IMG_0455.jpg
```

`--near` takes an airport's three-letter code, a town or city (the most populous of that name; `"Paris, Texas"` or `"Paris, France"` says which), or a position (`22.3089,113.9146`). `--km` is how far from it, 120 unless you say. The nearest come first; `--sort newest` or `oldest` orders by date instead. A photo with copies on several drives is one line, with how many more copies there are.

Searching around HKG isn't the same as looking for the `airport=HKG` tag. A photo's airport tag is the single nearest airport code, and in the middle of Hong Kong that's a heliport (HHP), so the tag finds only photos from Lantau and around the airport; the circle finds all of them.

Tags narrow it down, and dates:

```sh
backdown tag find --tag country="Hong Kong" --from 2023
backdown tag find --near "Paris, France" --km 20 --tag event="Boat show 2019"
```

`--tag` takes `KIND=VALUE` in any case, or a tag's number from `tag list`, and can be given more than once: every one has to be there. A tag stands for the tags under it too, so `--tag year=2019` finds every date in 2019. `--from` and `--to` take a year, a month or a day.

`--paths` prints only the path of every copy, one a line, for other tools; `--json` prints everything, positions and distances included.

## In the app: Places

**Places** in the sidebar is the same search, with a map:

- **The search box** suggests as you type: an airport or town to search around, tags that carry those words, or a year. What you've chosen shows as chips in the box; **within … km** sets the circle, and **from** and **to** the dates.
- **The map** is drawn from the place data built into BackDown, without going online: every town of 1,000 people or more is a dot, and the names of the biggest show as you zoom. The photos found are numbered clusters; click one to zoom in, and once it's one spot, to search around it. Shift+click searches around any point, and **Search this area** searches what's in view.
- **Refine** lists what everything found is tagged with (countries, regions, towns, airports, years, your events), with how many of each; click to narrow, click again to undo.
- **The photos**, nearest or newest first, grouped by distance or by month. Click to choose (Ctrl or Shift for more), double-click to open, and **Copy paths** copies every copy's path.
- **Choosing photos** shows their tags, where each came from, their copies (**Show in folder** opens the folder in the browser) and **Keep**, **Drop** and **Reviewed**. Add a tag of your own (an event, trip, album, place, memory, or whose they are) to all of them at once, or take one off: the airport a photo got wrong, say.
- **All tags** lists every tag with how many photos carry it. Your own can be renamed (to a name another tag has, and the two become one) or deleted.

Over the network (`backdown serve --host`), Places shows and searches but doesn't change tags.

<figure class="demo" data-view="places" data-caption="Places: the map drawn from the data built into BackDown, the refine list, and the photos nearest or newest first."></figure>

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

- Links don't show in the app yet, and the browser's folders don't show tags.
- Reading dates and places runs when you ask, not after every scan.
- Flags record decisions; nothing acts on them yet. Freeing up a drive still only deletes what has a verified copy elsewhere ([Free up a drive](/docs/guides/free-up-a-drive/)).
- Places are towns of 1,000 people or more, so a photo from somewhere remote gets the nearest such town. The nearest "airport" can be a heliport that has an airport code.
- The map has no coastlines or roads: only the towns, which is what's built in.
- Pictures show for photos the browser can display (JPEG, PNG, WebP, …); a HEIC or raw photo shows its icon, but opens in the viewer where the browser can.
- Trips and events aren't found for you; name them with `tag add`.
