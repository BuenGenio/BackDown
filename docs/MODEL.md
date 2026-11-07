# BackDown — the model

**One line:** the index stops being only a record of what is on each drive and becomes the authoritative *model* of what the archive should be — every file by content, its tags, flags and relationships, and where it belongs — analysed purely on metadata, then reconciled down to the filesystem; the folder tree becomes a rendered projection of the model, not the source of truth.

Asked for on 7 Nov 2026, while consolidating a failing drive into one archive: "that database, which by no means needs to be limited to the local machine, could keep track of all of the metadata, do the bulk of the analysis based purely on the available metadata, then serve as the template for the final filesystem-level changes to help organize that content canonically." This is the shape of that idea, and how it sits on top of what BackDown already is. The tag/flag/edge schema it refers to is in [`TAGGING.md`](TAGGING.md).

---

## 1. Two layers: the index (what *is*) and the model (what *should be*)

- The **node index** is physical truth: what is actually on a drive, with content hashes, sizes, inodes. It is embedded SQLite, authoritative for its own drive, and rebuildable at any time by rescanning (`db.rs`, module doc `db.rs:1-8`). This does not change, and `docs/SWARM.md:26` still holds: "No new index, central or distributed."
- The **model** is intent and meaning: every distinct piece of content, what it *is* (tags), what we have *decided* about it (flags), how things relate (edges), and the canonical place each thing belongs. Its derived parts are rebuildable; its **authored** parts are not (see §7).

The model does not replace the node index and never becomes a second "real index" of a drive's files. It is a layer *above* the indexes, about understanding rather than cataloguing — the distinction `docs/PRODUCT.md:18` draws against Bacula ("catalog is metadata, not understanding").

## 2. Content is the identity

Today a file is keyed by path-and-version (`files.id` + the unique `files_current_path`, `db.rs:66`); content addressing exists only as *indexes* (`files_sha256` `db.rs:72`, the covering `files_dupes` `db.rs:350`). There is **no one-row-per-content table**.

The model promotes content to a first-class key: tags, flags and edges attach to a `sha256` (or to a stable `Subject.key` / `Entity.key`), never to a bare path. Consequences:

- a fact asserted once applies to **every** copy of that content, and **survives** dedup, renames and moves;
- the folder layout becomes irrelevant to identity — which is what lets the tree be a projection (§4).

## 3. Analysis runs on metadata, not on files

All the heavy thinking — which copies are the same, which one is canonical, whose data it is, where each thing belongs, how big the finished archive will be — is computed from the index and model alone, touching no file byte.

This generalises what `plan`/`apply` already do: `plan` proposes and saves a plan in the DB; `apply --go` copies, verifies and journals, and never deletes (`plan.rs`, `apply.rs`). The model makes that continuous rather than one-shot. Because planning is metadata-only it is fast, safe and reversible — **you can design and size the entire canonical archive before moving a single byte**, which turns "how much storage do I need and what moves?" from guesswork into an answer the model computes.

## 4. The filesystem is a projection

The canonical tree is a *function of the model*: an entity's kind plus its tags determine its path, through the filing schema (`plan.rs` placeholders `plan.rs:37-42`, see [`TAGGING.md`](TAGGING.md) §9). Therefore:

- it is **re-projectable** — regenerate by-year, by-place or by-event at will;
- several views can coexist (e.g. a by-year tree and a by-place tree) through links;
- you never change the archive by hand-editing folders — you change the model and re-project.

## 5. Reconcile: the model down to disk

Reconcile diffs *desired* (model) against *actual* (node index) and emits the **minimal** set of filesystem operations — copy, move, hardlink, merge, and delete-only-where-a-verified-copy-exists. Every operation is dry-runnable and journaled through the custody chain (`custody.rs`), and each runs on the machine that owns the drive, through the pool's `swarm-stdio` writer, which only ever writes inside that machine's own pool folders (`docs/SWARM.md` §5, `pool/mod.rs`). A node's index is never written by another machine; the owner rescans and adopts.

## 6. Where the model lives — and the optional hub

- **Embedded per node is the default**, and keeps faith with ROADMAP principle 5 ("Local only", `docs/ROADMAP.md:129`).
- **A single, optional hub** can aggregate the whole swarm for cross-machine analysis and hold the authored curation layer. It reuses the import model `remote` already uses: a cache pulled over ssh and keyed per machine (`remotes`, `remote_roots`, `remote_files`, `db.rs:180-220`), droppable and rebuildable. The hub is the user's *own* always-on machine, not a vendor cloud — the only existing toehold for anything central is the hosted tier's scoped, read-only agent (`docs/PRODUCT.md:90`), and the model must not widen that.
- **Engines.** Nodes stay SQLite — it is already serializable-ACID, and gains geo via SpatiaLite / the R-tree module, full-text via FTS5 (already used, `fts.rs`), and the relationship graph via `edges` + recursive CTEs. Only if graph, geo or multi-writer concurrency outgrow that does the **hub** optionally move to Postgres + Apache AGE (Cypher) + PostGIS — as a *projection of the nodes*, never a replacement, and never required on an edge node. Compression, if wanted, comes from the filesystem (btrfs/ZFS zstd), not an engine swap: file hashes do not compress, paths and FTS do.

## 7. Durable vs rebuildable

- **Rebuildable:** scans, hashes, and recognition. Entities and subjects are per-scan snapshots upserted by `root_id` (`recognise/mod.rs:421-448`, `subjects.rs:1148-1175`) — recomputed, not precious.
- **Not rebuildable — the crown jewels:** authored tags, flags, canonical choices, event and place names, the you-vs-others ownership calls. This layer is what truly needs ACID, backup and journaling (via custody). Store it keyed by stable ids so a recomputation of entities/subjects never erases it (see [`TAGGING.md`](TAGGING.md) §2).

## 8. Principles that don't move

1. Never write to a source drive; never delete anything without a SHA-256-verified copy elsewhere.
2. **Local only** — the model, and any hub, are the user's own machines; no vendor cloud holds the archive's truth.
3. The node index stays the sole authority on its drive's physical contents. The model is about meaning and intent, and of other nodes keeps only a droppable, rebuildable cache.
4. The filesystem layout is a projection — change the model and re-project; never hand-edit the tree as truth.
5. Every filesystem change is dry-runnable first and journaled through custody after.
