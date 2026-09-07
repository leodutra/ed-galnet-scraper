# ed-galnet-scraper

Extracts all Galnet articles (3301 → present) into `galnet/files/`, unifying
two sources.

1. clone this repo
2. `cargo run --release`
3. look into `galnet/files`

## Sources

- **zaonce_cms** (`src/zaonce_cms.rs`): `GET
  https://cms.zaonce.net/en-GB/jsonapi/node/galnet_article` with `Accept:
  application/vnd.api+json`, `sort=-published_at`, `page[offset]` /
  `page[limit]=50`, following `links.next`. Canonical for everything from
  **07 DEC 3306** (~884 articles). Unofficial endpoint, no auth; it can change
  without notice. Has gaps (whole dates absent) and retires some guids.
- **galnet_site** (`src/galnet_site.rs`): date pages at
  `https://community.elitedangerous.com/galnet/<DD-MON-YYYY>`, discovered from
  the homepage (all links are in the served HTML; the MORE button only toggles
  CSS). The only source before 07 DEC 3306, and the fallback for anything the
  API lacks. Page fetch state lives in `galnet/successful-pages.json`,
  `galnet/empty-pages.json`, `galnet/failed-pages.json`.

## Conciliation (`src/merge.rs`)

Matching is primarily textual — normalized `(date, title, content)`:

1. **by uid** — site guid equals zaonce_cms `field_galnet_guid`;
2. **by text** — normalized text matches a zaonce_cms article (the date is
   part of the key, so recurring syndicated placeholders never collapse).

A site article matching zaonce_cms is filed under the zaonce_cms guid and
text; its site uid survives in `galnet/aliases.json`. Unmatched site articles
are kept as-is. Files on disk matching neither source are carried over
untouched. Stale duplicates and superseded files are removed. Re-runs are
idempotent; `galnet/sync.json` records the last sync.

## Output

One JSON per article:
`galnet/files/<YYYY MON DD> - <pageIndex> - <guid>.json`

| Field | Meaning |
| --- | --- |
| `uid` | zaonce_cms `field_galnet_guid` / site guid; article URL is `https://community.elitedangerous.com/galnet/uid/<uid>` |
| `pageIndex` | Order within the in-game date |
| `title` / `date` | Title and in-game date (e.g. `03 SEP 3312`) |
| `content` | Plain text (`body.value` / site paragraph) |
| `extractionDate` / `deprecated` | Sync bookkeeping |
