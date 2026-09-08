# CLAUDE.md

@AGENTS.md

Sources, merge semantics and output format: `README.md`. This file only holds
what those two don't, and what is expensive to rediscover.

## `galnet/files/` is the product — treat deletions as suspects, not facts

`git status` showing ` D ` on article files is **usually correct dedup**, not
data loss. The site renders each `<div class="article">` twice, so the archive
accumulated same-uid pairs at consecutive `pageIndex` values; the scraper
removes the extras. Before reporting or "fixing" a deletion, prove it:

```bash
# 1. does every deleted file still have a surviving twin with the same uid?
git status --short -- galnet/files | grep '^ D' | sed 's/^ D "\?//; s/"\?$//' |
  while read -r f; do uid="${f##* - }"; echo "$(ls galnet/files | grep -c -- "${uid%.json}") $f"; done

# 2. the one that actually matters: did any uid disappear?
comm -23 <(git ls-tree --name-only HEAD galnet/files/ | sed 's/.* - \(.*\)\.json/\1/' | sort -u) \
         <(ls galnet/files | sed 's/.* - \(.*\)\.json/\1/' | sort -u)
```

Empty output from (2) means nothing was lost. Never `git checkout --
galnet/files` or delete files to "restore" before running (2).

## Invariants to re-check after any change that writes files

```bash
cargo run --release   # then run it a SECOND time: must report "0 written"
```

Plus: `pageIndex` contiguous per date, matching both the filename and the JSON
field; no untrimmed content lines; no uid lost. A rerun that writes files it
already wrote means the write path is not normalizing something.

## Content format

One `\n` per break — a site `<br /><br />` **pair** is one newline, matching
zaonce_cms; longer runs halve, which preserves the blank line early-3301
bodies put after their repeated headline. Every line is trimmed. All of this
is applied in `article_shell` (`src/merge.rs`), the single path that writes a
file, so CMS rows, live site rows and disk reloads converge byte-for-byte. Put
new normalization there, not in a fetcher.

## Re-fetching is gated

`galnet/successful-pages.json` suppresses re-fetching. After a parser change,
either drop the specific pages you want to verify from that list, or force a
full rebuild with `echo '[]' > galnet/successful-pages.json` (~2071 sequential
requests, 20–40 min). Nothing is lost by doing so: the homepage re-discovers
every date page, and unfetched articles are carried from disk.

Known-tricky pages to verify a parser change against — a clean re-fetch of
these should produce a **zero diff**:

- `01-AUG-3301` — headline repeated as the body's first line, doubled `<br>` run
- `17-DEC-3301` — 8 article divs, 4 uids (the double-render)
- `01-APR-3301` — plain multi-paragraph shape

## Upstream quirks that are not bugs

- zaonce_cms starts at **07 DEC 3306**. Everything older exists only on the
  site. This is expected, not a fetch failure.
- Six 3301 dates serve zero article divs upstream (`galnet/empty-pages.json`).
  Recorded as *empty*, never as failed.
- `3305 APR 29 - 1` has empty content because the site serves a literal
  `<p></p>`. It survives only as a disk carry-over; a fresh parse skips it.
- `matchedByUid` / `matchedByText` of `0` in `sync.json` is normal on an
  incremental run: with every page already downloaded, the site side is
  reloaded from disk, and disk rows are deliberately barred from text-matching.
  The conciliation path only engages on freshly fetched pages.
