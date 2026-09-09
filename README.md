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
  `galnet/empty-pages.json`, `galnet/failed-pages.json`. Pages listed as
  successful are never re-fetched, so after a parser fix force a full rebuild
  with `echo '[]' > galnet/successful-pages.json` (the homepage alone
  re-discovers all ~2071 date pages, so nothing is lost).

## Conciliation (`src/merge.rs`)

Matching is primarily textual — normalized `(date, title, content)`:

1. **by uid** — site guid equals zaonce_cms `field_galnet_guid` (uid wins
   even if CMS copy-edited the text afterwards);
2. **by text** — normalized text matches a zaonce_cms article (the date is
   part of the key, so recurring syndicated placeholders never collapse).

Two site uids **never** collapse into each other on text alone. The live site
serves distinct uids with identical normalized text as separate divs, and
those are distinct upstream articles — `25 APR 3308` and `29 JAN 3311` serve
such a pair and zaonce_cms holds *both* as separate nodes.

The by-text pass carries the same guard: if the date page that served a site
row also serves the matched zaonce_cms guid as its own div, the row stays
site-only. `29 SEP 3308` and `18 DEC 3311` are exactly that shape — the CMS
retired one guid of the pair, but the site still serves both, so both are
kept.

A site article matching zaonce_cms is filed under the zaonce_cms guid and
text; its site uid survives in `galnet/aliases.json`. Unmatched site articles
are kept as-is. Files on disk matching neither source are carried over
untouched. Stale duplicates and superseded files are removed. Re-runs are
idempotent; `galnet/sync.json` records the last sync.

## Output

One JSON per article:
`galnet/files/<YYYY-MM-DD>-<pageIndex>-<guid>.json`

| Field | Meaning |
| --- | --- |
| `uid` | zaonce_cms `field_galnet_guid` / site guid; article URL is `https://community.elitedangerous.com/galnet/uid/<uid>` |
| `pageIndex` | Order within the in-game date |
| `title` / `date` | Title and in-game date (e.g. `03 SEP 3312`) |
| `content` | Plain text (`body.value` / site paragraph). One `\n` per break — a site `<br /><br />` pair is one newline, matching zaonce_cms; every line is trimmed, so the same article is byte-identical whichever source filed it |
| `extractionDate` / `deprecated` | Sync bookkeeping |
