# BackDown — tags and flags

**One line:** *tags* say what content **is** (a photo, taken in Hong Kong, showing Shkuleta); *flags* say what BackDown has **decided** about an item (this is the canonical copy, that one is a duplicate, this is reviewed, drop that); both are small additive tables keyed by stable ids, journaled through custody, and they feed the model's analysis and the filing projection.

Companion to [`MODEL.md`](MODEL.md): the model is the *why*, this is the *how* of the metadata it carries. Asked for on 7 Nov 2026, alongside a canonical `Media/Photos/{year}` archive tagged by location, place, airport, event and date.

---

## 1. Tags vs flags

- A **tag** is a *fact about content*: "this is a photo", "taken in Hong Kong", "shows Shkuleta". Descriptive, many-to-many, derived from the content or asserted by you.
- A **flag** is a *decision about an item*: "canonical", "duplicate of X", "reviewed", "drop", "mine". State, mutable, and it drives the dedup/curation pipeline.

The precedent already in the code is the pair `Entity.low_value` and the `Junk` subtrees (`recognise/mod.rs:66-81`, `docs/PRODUCT.md:50`) — proto-flags on recognised things. Tags and flags generalise that into a first-class layer.

## 2. What they attach to (identity)

A target is one of:

- **content** — `hex(sha256)`. For intrinsic facts (it is a photo; it was taken here). Survives dedup, renames and moves, and applies to every copy at once.
- **subject** — a `Subject.key`, the stable cross-root person/database id `"{kind}:{first-identifying-alias}"` (`subjects.rs:963-987`). For ownership and people.
- **entity** — an `Entity.key` = `browse::key(path)` (`recognise/mod.rs:279`). For a place/collection as a whole.

Never a bare path — paths are versions (`files_current_path`, `db.rs:66`). Reuse the identities BackDown already derives rather than invent new ones: owner/person tags ride `Subject.key`, media-kind rides `Entity.kind`. Because entities and subjects are recomputed each scan and upserted by `root_id`, tags live in their own tables keyed by these stable ids, so a recomputation never erases them.

## 3. Tag kinds

| kind | attaches to | source | example |
|---|---|---|---|
| `owner` / person | subject | derived + you | `person:email:alice@…` (mine vs not-mine is a flag, §5) |
| `kind` | content / entity | derived | photo, music, video (from `Entity.kind`) |
| `location` | content | exif | `lat,lon` |
| `country`, `city` | content | reference | HK, Hong Kong |
| `place` | content | you / POI | Foreign Correspondents Club |
| `airport` | content | reference | HKG, CDG, FLL |
| `event` / `trip` | content | cluster + you | FLIBS 2019, Colombia 2019 |
| `date`, `year` | content | exif / mtime | 2019 |
| `memory`, `album` | content | you | — |

## 4. Deriving tags (honest about effort)

| tag | from | effort |
|---|---|---|
| date / year | EXIF `DateTimeOriginal`, else `mtime` | easy (`kamadak-exif`) |
| location (coords) | EXIF GPS | easy |
| country / city | reverse-geocode coords against a bundled offline dataset (GeoNames CC-BY / Natural Earth PD) | medium — ship the data, look up via R-tree/SpatiaLite |
| airport | nearest airport from the OurAirports CSV (public domain) within N km | easy |
| trip / event | cluster photos in time + space (DBSCAN-style), propose a grouping; **you name it** | medium; the naming is yours |
| place (named POI) | OSM/Overpass, or your assertion | hard — default to user-asserted, optionally POI-assisted |
| owner / person | from `Subject` (mail / nextcloud / home folders, `subjects.rs:390-555`); you confirm mine/not-mine | reuse subjects; the call is yours |

Coords, date and airport are mechanical; country/city need the bundled reference data; place and event need you (optionally assisted). "Florida resolving anywhere from Fort Lauderdale to Port Orange" is a `country`→region hierarchy plus a time+space cluster that becomes one named `trip`.

## 5. Flags

`canonical` · `duplicate` (→ its canonical key) · `keep` · `drop` · `reviewed` · `owner-confirmed` · `mine` / `not-mine` (on a subject).

Flags are what make "collapse identical content despite cosmetic metadata" safe and explicit. The differences blocking the current `dedup` are, measured on the real archive, **timezone-shifted timestamps** (pairs exactly ±8–9 h apart — the HK/UTC offset) and **stray permission bits** (`0o674` vs `0o775`) — non-identifying. So the flow is: flag one copy `canonical` (earliest mtime, say), flag the rest `duplicate`, and reconcile keeps the canonical and drops the rest — each only when a verified copy exists.

## 6. Relationships (the graph)

`edges(src, rel, dst)` captures the graph the roadmap already names (`docs/PRODUCT.md:64`, listed "Next" at `:125`; architecture home at `:85`):

- photo —`taken_at`→ place —`in`→ city —`in`→ country
- photo —`part_of`→ trip; trip —`at`→ place
- person —`owns`→ collection
- backup —`came_from`→ machine —`came_from`→ server
- photo —`copy_of`→ photo (the same image across locations)

Traversal is recursive CTEs on a node; geo edges use SpatiaLite/the R-tree module; a hub may add Apache AGE (Cypher). See [`MODEL.md`](MODEL.md) §6.

## 7. Schema (additive, matching conventions)

A new module `tags.rs` exposes `pub(crate) const SCHEMA` and is stitched into `Db::open` with one line beside the others (`db.rs:449-462`, e.g. after `conn.execute_batch(crate::custody::SCHEMA)?;`). All `CREATE TABLE IF NOT EXISTS`; **no `user_version` bump** (`VERSION` stays `2`, `db.rs:292`) — this follows the "small additive tables older versions ignore" convention (`docs/SWARM.md:26`). Any index expensive over the ~17 M-row scale goes in `LATER_INDEXES` (`db.rs:356-374`), built as a daemon job, not at open.

```sql
-- the content anchor (new: today content is an index, not a row)
CREATE TABLE IF NOT EXISTS content (
  sha256 BLOB PRIMARY KEY,
  size   INTEGER NOT NULL,
  kind   TEXT                 -- photo|music|video|… cached from recognise/magic
);

-- the tag vocabulary; hierarchical (country>city, trip>place)
CREATE TABLE IF NOT EXISTS tags (
  id        INTEGER PRIMARY KEY,
  kind      TEXT NOT NULL,     -- owner|kind|location|country|city|place|airport|event|date|memory
  value     TEXT NOT NULL,
  parent_id INTEGER REFERENCES tags(id),
  data      TEXT,              -- JSON: coords, codes, confidence hints
  UNIQUE(kind, value, parent_id)
);

-- attachments: a tag on a target
CREATE TABLE IF NOT EXISTS taggings (
  target_type TEXT NOT NULL,   -- content|subject|entity
  target_key  TEXT NOT NULL,   -- hex(sha256) | Subject.key | Entity.key
  tag_id      INTEGER NOT NULL REFERENCES tags(id),
  source      TEXT NOT NULL,   -- exif|derived|reference|user
  confidence  REAL NOT NULL DEFAULT 1.0,
  added_at    INTEGER NOT NULL,
  PRIMARY KEY (target_type, target_key, tag_id)
) WITHOUT ROWID;

-- flags: a decision on an item
CREATE TABLE IF NOT EXISTS item_state (
  target_type   TEXT NOT NULL, -- content|subject|entity|path
  target_key    TEXT NOT NULL,
  state         TEXT NOT NULL, -- canonical|duplicate|keep|drop|reviewed|owner-confirmed|mine|not-mine
  canonical_key TEXT,          -- for duplicate → the canonical target_key
  decided_by    TEXT NOT NULL, -- user|auto
  decided_at    INTEGER NOT NULL,
  note          TEXT,
  PRIMARY KEY (target_type, target_key, state)
) WITHOUT ROWID;

-- relationship edges (the graph)
CREATE TABLE IF NOT EXISTS edges (
  src_type TEXT NOT NULL, src_key TEXT NOT NULL,
  rel      TEXT NOT NULL,
  dst_type TEXT NOT NULL, dst_key TEXT NOT NULL,
  data     TEXT,
  at       INTEGER NOT NULL,
  PRIMARY KEY (src_type, src_key, rel, dst_type, dst_key)
) WITHOUT ROWID;
```

Derived rows (exif/reference) are keyed so re-derivation overwrites cleanly; authored rows (`source = 'user'`, hand-set flags) are the durable layer.

## 8. Journaling (audit + durability)

Authored tags and flags are the crown jewels ([`MODEL.md`](MODEL.md) §7), so every hand change is journaled through the existing custody chain (`custody.rs`): add an `Action::Tag` / `Untag` / `Flag` variant (`custody.rs:102-123`) and a `"tag"` entry to `JOBS` (`custody.rs:97`), then wrap each write in `custody::journaled(conn, &event, |…| write)` (`custody.rs:332`) so the table write and the hash-chained event commit in one transaction. Derived tags are recomputable and need not be journaled.

## 9. Filing integration

The filing schema's placeholders (`plan.rs:37-42`) extend naturally: add `{person}`, `{country}`, `{place}`, `{event}` to the `fill()` replace chain (`plan.rs:337-349`). Sourcing them is new plumbing — today `fill()` and its caller (`plan.rs:462`) resolve placeholders from the `Entity` alone; tag-driven ones need per-item tag/subject input passed in. `{origin}`, computed per kind in `origin()` (`plan.rs:269-319`), is the precedent for a computed, context-specific placeholder. The target layout, e.g.:

```
Media/Photos/{year}/{country}/{place}      # a photo
People/{person}/{year}                     # someone's own files
```

## 10. Extensibility

Mirror the subjects rule (`docs/ROADMAP.md:39`, "new kinds of subject are one extractor function each"): a **new tag kind** = a deriver function + a vocabulary entry; a **new flag** = a state value + its reconcile meaning; a **new relationship** = a `rel` value. No schema change for any of them.

## 11. Non-goals / principles

1. Tags and flags never move or delete a file themselves; only reconcile does, and only with a SHA-256-verified copy ([`MODEL.md`](MODEL.md) §5, §8).
2. Derived tags are recomputable; authored tags and flags are durable and journaled.
3. Cosmetic metadata — timezone-shifted mtime, stray permission bits — is non-identifying; content identity plus a `canonical` flag decides sameness.
4. Tags attach to content, subjects or entities by stable key, never to a path, so recognition re-runs and file moves never orphan them.
