# ed-galnet-scraper

Extracts Galnet articles from Frontier's Drupal JSON:API into `galnet/files/`.

1. clone this repo
2. `cargo run --release`
3. look into `galnet/files`

## Source

- Endpoint: `GET https://cms.zaonce.net/en-GB/jsonapi/node/galnet_article` with
  `Accept: application/vnd.api+json`, `sort=-published_at`,
  `page[offset]` / `page[limit]=50`, following `links.next` until it disappears.
- Collection covers **07 DEC 3306 → present** (~884 articles). Older Galnet
  (3301–early 3306) is not in this API; existing per-date files for those are
  kept as-is.
- Re-runs upsert: files whose content matches are skipped, new/changed files
  are written, and `galnet/zaonce-sync.json` records the last sync range.
- Unofficial endpoint, no auth; it can change without notice.

## Output

One JSON per article:
`galnet/files/<YYYY MON DD> - <pageIndex> - <guid>.json`

| Field | Meaning |
| --- | --- |
| `uid` | `field_galnet_guid`; community URL is `https://community.elitedangerous.com/galnet/uid/<uid>` |
| `pageIndex` | Order within the in-game date, newest first |
| `title` / `date` | Title and in-game date (e.g. `03 SEP 3312`) |
| `content` | Plain text from `body.value` |
| `extractionDate` / `deprecated` | Sync bookkeeping |
