# AGENTS.md

Rust scraper that unifies two Galnet sources into `galnet/files/`. See `README.md` for source/merge semantics.

## Skills (must read before writing Rust)

- `skills/rust-type-driven/SKILL.md` — type-driven Rust rules (precedence over default conventions; ADRs may override).

If more skills appear under `skills/*/SKILL.md`, read the ones relevant to the task.

## Commands

```bash
cargo check
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo run --release
```

`cargo check` first (fastest). `clippy` + `test` must pass before done.

## Layout

- `src/main.rs` — pipeline: fetch CMS → discover/fetch site pages → merge → sync disk → write bookkeeping.
- `src/zaonce_cms.rs` — Drupal JSON:API fetcher (canonical from 07 DEC 3306).
- `src/galnet_site.rs` — HTML date-page scraper (only source pre-3306, fallback after).
- `src/merge.rs` — conciliation (uid-match, then normalized-text match) + `sync_to_disk`.
- `src/common.rs` — shared types, retry, disk scan, normalize/title/date helpers.
- `galnet/` — output (`files/`), page state (`successful/empty/failed-pages.json`), `aliases.json`, `sync.json`. Do not hand-edit; reruns are idempotent.
