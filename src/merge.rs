//! Conciliation of the two Galnet sources into one archive.
//!
//! - **zaonce_cms JSON:API**: canonical for everything from 07 DEC 3306. Clean
//!   text, but the collection has gaps (e.g. whole dates like 01 SEP 3307 are
//!   absent) and some articles exist under a different guid.
//! - **galnet_site**: the only source before 07 DEC 3306, and the fallback
//!   for anything the API lacks.
//!
//! Matching is primarily textual: articles with equivalent normalized
//! `(date, title, content)` are the same article even if the uid differs or
//! one side is missing. Matching passes, in order:
//!
//! 1. **by uid** — the galnet_site guid equals the zaonce_cms `field_galnet_guid`.
//! 2. **by text (CMS only, live rows)** — normalized `(date, title, content)`
//!    of a freshly scraped site row matches a zaonce_cms article. The date is
//!    part of the key so recurring syndicated placeholders (e.g. weekly
//!    powerplay updates reposted verbatim for months) never collapse into a
//!    single entry. Disk-reload rows never text-match (no page proof).
//!
//! Site-vs-site: different uids NEVER collapse on text. The site renders
//! some pages with each `<div class="article">` twice under the SAME uid
//! (verified: 17 DEC 3301 = 8 divs / 4 uids; /galnet/uid/<uid> = 2 divs /
//! 1 uid) — always same-uid, deduped by the fetcher and by per-run uid
//! dedupe. Distinct uids with identical normalized text are distinct
//! upstream articles (22 JUL 3301 etc. serve one div per uid).
//!
//! zaonce_cms has no duplicates at all: 884 nodes, 884 unique uuids and
//! guids, every node carrying guid + date (verified live).
//!
//! Winners and losers:
//!
//! - A galnet_site article matching zaonce_cms is written under the **zaonce_cms
//!   guid** and zaonce_cms text (cleaner source of truth). Its galnet_site uid
//!   survives in `galnet/aliases.json` so the old URL keeps resolving.
//! - A galnet_site article with no match is kept as-is (**site-only**), e.g.
//!   pre-3306 articles or newer ones missing from the API.
//! - Duplicate files (same uid stored under several `page_index`
//!   values) collapse to the stored winner (first file wins); extras are deleted.
//! - Files on disk whose uid matches nothing from either source are carried
//!   over untouched (e.g. dates the live site no longer serves, or uids the
//!   galnet_site has since replaced).
//! - Anything else on disk (stale duplicates, superseded uids) is removed.
//!
//! `page_index` is a stable slot within an in-game date group, assigned
//! first-seen-wins: known uids keep their stored slot forever, new uids take
//! the smallest free slot in their date group. Gaps left by deletions are
//! never compacted, so reruns never rename surviving files. Merge order only
//! decides collision priority and the order new slots are handed out.

use crate::common::{
    Article, DiskScan, EXTRACTED_FILES_LOCATION, GALNET_SITE_UID_URL, normalize_text,
    revert_galnet_date, title_fallback,
};
use crate::galnet_site::GalnetSiteArticle;
use crate::zaonce_cms::ZaonceArticle;

use std::collections::{HashMap, HashSet};

/// One reconciled article plus the provenance needed to file it.
/// (`is_site_only` is data for tests/future reporting; the sync path files
/// everything through the same writer.)
#[derive(Debug)]
pub(crate) struct UnifiedArticle {
    pub(crate) article: Article,
    #[allow(dead_code)]
    pub(crate) is_site_only: bool,
}

#[derive(Debug, Default)]
pub(crate) struct MergeStats {
    pub(crate) matched_by_uid: usize,
    pub(crate) matched_by_text: usize,
    pub(crate) site_only: usize,
    /// Same-uid rows that arrived twice with different text (defense in
    /// depth; normally impossible — see site-only branch). Different uids
    /// never collapse.
    pub(crate) site_dupes_collapsed: usize,
    pub(crate) carried_from_disk: usize,
    pub(crate) files_removed: usize,
    /// galnet_site uids that resolve to a different canonical zaonce_cms guid
    pub(crate) alias_uids: HashSet<String>,
}

/// The single alias list: (site uid, canonical uid), sorted. Covers CMS
/// text-matches only — site-vs-site never aliases (different uids are
/// always distinct articles).
pub(crate) fn aliases_sorted(
    stats: &MergeStats,
    canonical_of: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut aliases: Vec<(String, String)> = stats
        .alias_uids
        .iter()
        .filter_map(|uid| canonical_of.get(uid).map(|c| (uid.clone(), c.clone())))
        .collect();
    aliases.sort();
    aliases
}

/// Merge output: unified records, stats, carry-over uids, aliases.
pub(crate) type MergeOutput = (
    Vec<UnifiedArticle>,
    MergeStats,
    Vec<String>,
    Vec<(String, String)>,
);

/// Canonical (date, title, content) key: textual identity across sources.
pub(crate) fn text_key(date: &str, title: &str, content: &str) -> String {
    format!(
        "{}\x1f{}\x1f{}",
        normalize_text(date),
        normalize_text(title),
        normalize_text(content)
    )
}

fn cms_key(article: &ZaonceArticle) -> String {
    text_key(&article.galnet_date, &article.title, &article.content)
}

fn site_key(article: &GalnetSiteArticle) -> String {
    text_key(&article.date, &article.title, &article.content)
}

fn article_shell(
    uid: &str,
    page_index: usize,
    title: &str,
    date: &str,
    content: &str,
    extraction_date: &str,
) -> Article {
    // Early-3301 galnet_site pages have an empty `<h3>`; the real headline is
    // the first body line.
    let title = if title.trim().is_empty() {
        title_fallback(content)
    } else {
        title.to_owned()
    };
    Article {
        uid: uid.to_owned(),
        page_index,
        title,
        date: date.to_owned(),
        url: format!("{GALNET_SITE_UID_URL}/{uid}"),
        content: content.to_owned(),
        extraction_date: extraction_date.to_owned(),
        deprecated: false,
    }
}

/// Merge zaonce_cms articles with galnet_site articles into file-ready records.
///
/// `galnet_site` must hold true site-side articles; CMS-canonical uids are
/// skipped defensively. `stored_slots` maps uid -> stored `(date, page_index)`
/// from the current [`DiskScan`]; it pins `page_index` for known uids so
/// reruns never renumber. Returns unified records, stats, carry-over uids (on
/// disk but matched by neither source), and the alias list
/// (site uid -> canonical uid).
pub(crate) fn merge(
    zaonce_cms: &[ZaonceArticle],
    galnet_site: &[GalnetSiteArticle],
    disk_uids: &HashSet<String>,
    stored_slots: &HashMap<String, (String, usize)>,
    extraction_date: &str,
) -> MergeOutput {
    let mut stats = MergeStats::default();

    let mut by_guid: HashMap<&str, &ZaonceArticle> = HashMap::new();
    let mut by_text: HashMap<String, &ZaonceArticle> = HashMap::new();
    for article in zaonce_cms {
        by_guid.insert(article.guid.as_str(), article);
        // First wins on identical text; zaonce_cms guids are unique anyway.
        by_text.entry(cms_key(article)).or_insert(article);
    }

    // date -> ordered canonical records (uid, title, date, content).
    // Order here only decides new-slot handout and collision priority;
    // page_index itself comes from `stored_slots` (stable per uid).
    let mut groups: HashMap<String, Vec<(String, String, String, String)>> = HashMap::new();
    let mut date_order: Vec<String> = Vec::new();
    let mut push_record = |date: &str, record: (String, String, String, String)| {
        groups.entry(date.to_owned()).or_insert_with(|| {
            date_order.push(date.to_owned());
            Vec::new()
        });
        if let Some(records) = groups.get_mut(date) {
            records.push(record);
        }
    };

    // 1. zaonce_cms first: canonical guids, titles, content. Dates keep API order
    // (newest first); records within a date keep fetch order.
    for article in zaonce_cms {
        push_record(
            &article.galnet_date,
            (
                article.guid.clone(),
                article.title.clone(),
                article.galnet_date.clone(),
                article.content.clone(),
            ),
        );
    }

    // 2. galnet_site articles: match against zaonce_cms, else site-only.
    // Caller must exclude CMS-canonical uids from the site side (disk
    // reloads hold both) and skip freshly scraped site copies of CMS uids;
    // the belt-and-braces uid check below is the last line of defense.
    //
    // Match order: (1) uid — the site guid equals the CMS guid, regardless
    // of text drift; (2) normalized text for LIVE rows only. The uid check
    // must come first: CMS copy-edits titles/content over time, so a
    // same-uid pair with drifted text must still count as a uid-match, not
    // site-only. Disk rows (empty page_url) never text-match: normalized
    // text alone is too weak — live date pages serve distinct uids with
    // identical normalized text as distinct divs (22 JUL 3301, 07 SEP 3301,
    // 08 JUL 3302, 06 DEC 3304, 29 SEP 3308, 25 APR 3308, 29 JAN 3311),
    // differing only by e.g. a double space or a trailing space.
    //
    // Site-vs-site: NO collapse on text, ever (see site-only branch).
    // Sort order below only decides new-slot handout priority: disk rows
    // (empty page_url) first so stored slots win deterministically, then
    // live rows in page order, uid as final tiebreak.
    let mut site_sorted: Vec<&GalnetSiteArticle> = galnet_site.iter().collect();
    site_sorted.sort_by(|a, b| {
        (a.page_url.clone(), a.index_in_page, a.uid.clone()).cmp(&(
            b.page_url.clone(),
            b.index_in_page,
            b.uid.clone(),
        ))
    });
    // page URL -> uids served on that page (live rows only). A page that
    // renders the CMS guid AND a same-text sibling uid as separate divs is
    // proof the sibling is a distinct upstream article, not the same one
    // under a different guid — live-verified on 29 SEP 3308 (632c2adf +
    // CMS 63357af6) and 18 DEC 3311 (6944182b + CMS 69442c6c), matching the
    // 25 APR 3308 / 29 JAN 3311 pattern where both siblings ARE in the CMS.
    let mut page_uids: HashMap<&str, HashSet<&str>> = HashMap::new();
    for article in galnet_site {
        if !article.page_url.is_empty() {
            page_uids
                .entry(article.page_url.as_str())
                .or_default()
                .insert(article.uid.as_str());
        }
    }
    // (uid, text-key) -> uid. Same-uid guard only (see site-only branch):
    // different uids never collapse on text, so the map is keyed by uid
    // rather than page. First row per (uid, text) wins.
    let mut site_text_seen: HashMap<(String, String), String> = HashMap::new();
    // canonical site uid -> CMS guid for text-matches (alias source).
    let mut canonical_of: HashMap<String, String> = HashMap::new();
    // Per-run uid dedupe (pages render each article div twice).
    let mut seen_site_uids: HashSet<&str> = HashSet::new();
    for article in site_sorted {
        if !seen_site_uids.insert(article.uid.as_str()) {
            continue;
        }
        // Uid-match is defensive (callers exclude CMS guids from the site
        // side): same uid counts even for disk rows — CMS text is already
        // filed in pass 1, so no duplicate record is created either way.
        // Text-match needs a live row (non-empty page_url from this run's
        // fetch). Disk rows (empty page_url) always file as site-only:
        // normalized text alone is too weak — live date pages serve
        // distinct uids with identical normalized text as distinct divs,
        // so text-matching disk rows deletes real upstream articles.
        let matched_cms: Option<(&ZaonceArticle, bool)> = match by_guid.get(article.uid.as_str()) {
            Some(z) => Some((z, true)),
            None if !article.page_url.is_empty() => by_text
                .get(&site_key(article))
                .copied()
                // Same-page proof: if this page also serves the CMS guid as
                // its own div, the two uids are distinct upstream articles.
                // Collapsing here deletes a real record the site still
                // serves, so keep this row site-only.
                .filter(|z| {
                    !page_uids
                        .get(article.page_url.as_str())
                        .is_some_and(|uids| uids.contains(z.guid.as_str()))
                })
                .map(|z| (z, false)),
            None => None,
        };

        match matched_cms {
            Some((z, by_uid_match)) => {
                if by_uid_match && article.uid == z.guid {
                    // Same uid (text may have drifted through CMS copy-edits,
                    // or the site side is a stale disk copy): CMS canonical
                    // text is already recorded in pass 1.
                    // A same-uid pair whose text no longer matches is still
                    // the same article — count it, don't file it site-only.
                    stats.matched_by_uid += 1;
                } else if !by_uid_match {
                    // Different uid, same normalized text.
                    stats.matched_by_text += 1;
                    stats.alias_uids.insert(article.uid.clone());
                    canonical_of.insert(article.uid.clone(), z.guid.clone());
                } else {
                    // Unreachable: by_uid_match implies article.uid == z.guid.
                    stats.matched_by_uid += 1;
                }
            }
            None => {
                // Site-only: keep galnet_site text verbatim. Every uid files
                // as its own article — different uids NEVER collapse on
                // text. Live-verified: 17 DEC 3301 renders each div twice
                // under the SAME uid (8 divs, 4 uids), and /galnet/uid/<uid>
                // pages do the same (2 divs, 1 uid) — the double-render is
                // always same-uid, already deduped by the fetcher
                // (`parse_date_page`) and by `seen_site_uids` above.
                // Distinct uids with identical normalized text are distinct
                // upstream articles (22 JUL 3301 etc. serve one div per
                // uid), differing by e.g. a double space.
                //
                // The collapse map below only guards the same-uid case: if
                // one uid arrived twice with different text (stale disk row
                // + fresh live row), the first wins. This cannot trigger via
                // `seen_site_uids`, so it is pure defense-in-depth.
                let key = (article.uid.clone(), site_key(article));
                if site_text_seen.contains_key(&key) {
                    stats.site_dupes_collapsed += 1;
                    continue;
                }
                site_text_seen.insert(key, article.uid.clone());
                push_record(
                    &article.date,
                    (
                        article.uid.clone(),
                        article.title.clone(),
                        article.date.clone(),
                        article.content.clone(),
                    ),
                );
                stats.site_only += 1;
            }
        }
    }

    // 3. carry-over: uids on disk matched by neither source (deleted upstream
    // or never in either source). Loaded from disk by the caller.
    let live_uids: HashSet<&str> = groups
        .values()
        .flat_map(|records| records.iter().map(|r| r.0.as_str()))
        .chain(stats.alias_uids.iter().map(String::as_str))
        .collect();
    let mut carried: Vec<String> = disk_uids
        .iter()
        .filter(|uid| !live_uids.contains(uid.as_str()))
        .cloned()
        .collect();
    carried.sort();
    stats.carried_from_disk = carried.len();

    // 4. assign page_index per date group (stable slots) and build Articles.
    // Known uids keep their stored slot when the stored date still matches
    // the merged date; anything else (new uids, date changes) takes the
    // smallest free slot in its date group. Free slots include gaps left by
    // deletions AND slots vacated by uids that moved dates (their old slot
    // frees up). Within one date, contended slots resolve in merge order:
    // CMS records first (API order), then site-only records (page order).
    let mut unified = Vec::new();
    // date -> slots already claimed this run (stored keepers + new handouts).
    let mut claimed: HashMap<String, HashSet<usize>> = HashMap::new();
    for records in groups.values() {
        for (uid, _, record_date, _) in records {
            if let Some((stored_date, stored_index)) = stored_slots.get(uid)
                && stored_date == record_date
            {
                claimed
                    .entry(record_date.clone())
                    .or_default()
                    .insert(*stored_index);
            }
        }
    }
    for date in &date_order {
        let records = &groups[date.as_str()];
        for (uid, title, record_date, content) in records {
            let page_index = match stored_slots.get(uid) {
                Some((stored_date, stored_index)) if stored_date == record_date => *stored_index,
                _ => {
                    let taken = claimed.entry(record_date.clone()).or_default();
                    let mut slot = 0usize;
                    while taken.contains(&slot) {
                        slot += 1;
                    }
                    taken.insert(slot);
                    slot
                }
            };
            unified.push(UnifiedArticle {
                article: article_shell(
                    uid,
                    page_index,
                    title,
                    record_date,
                    content,
                    extraction_date,
                ),
                is_site_only: !by_guid.contains_key(uid.as_str()),
            });
        }
    }

    // CMS guid -> canonical uid is identity; merge with the site-side map so
    // the caller can serialize aliases without re-hashing any text.
    let mut canonical_of_all: HashMap<String, String> = canonical_of;
    for article in zaonce_cms {
        canonical_of_all.insert(article.guid.clone(), article.guid.clone());
    }
    stats
        .alias_uids
        .retain(|uid| canonical_of_all.contains_key(uid));
    let aliases = aliases_sorted(&stats, &canonical_of_all);

    (unified, stats, carried, aliases)
}

/// Canonical filename for an article.
///
/// The uid is part of the filename, so identity never depends on the slot:
/// two articles sharing a date and page_index still land on distinct paths.
/// With stable slots this only happens transiently (hand-restored files),
/// and the next run resolves it by keeping stored slots per uid.
pub(crate) fn filename_for(article: &Article) -> String {
    format!(
        "{}/{} - {} - {}.json",
        EXTRACTED_FILES_LOCATION,
        revert_galnet_date(&article.date),
        article.page_index,
        article.uid
    )
}

/// Sync the unified plan to disk using one precomputed [`DiskScan`].
/// Returns (written, unchanged). `carried` holds Articles loaded from disk
/// for carry-over uids.
pub(crate) fn sync_to_disk(
    unified: &[UnifiedArticle],
    carried: &[Article],
    stats: &mut MergeStats,
    scan: &DiskScan,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    use crate::common::{GalnetError, serialize_to_file};
    use std::fs;

    fs::create_dir_all(EXTRACTED_FILES_LOCATION)?;

    // Desired state: canonical filename -> article.
    let mut desired: HashMap<String, &Article> =
        HashMap::with_capacity(unified.len() + carried.len());
    for item in unified {
        desired.insert(filename_for(&item.article), &item.article);
    }
    for article in carried {
        desired.insert(filename_for(article), article);
    }

    let mut live_paths: HashSet<String> = HashSet::with_capacity(desired.len());
    let mut file_errors: Vec<GalnetError> = Vec::new();

    // Every desired article claims exactly one path; other files holding the
    // same uid (duplicate `page_index` copies) are removed.
    for (path, article) in &desired {
        live_paths.insert(path.clone());
        if let Some(known) = scan.paths_by_uid.get(&article.uid) {
            for other in known {
                if other != path && !desired.contains_key(other) {
                    if let Err(e) = fs::remove_file(other) {
                        file_errors.push(GalnetError::FileError {
                            filename: other.clone(),
                            cause: Box::new(e),
                        });
                    } else {
                        stats.files_removed += 1;
                    }
                }
            }
        }
    }

    let mut written = 0usize;
    let mut unchanged = 0usize;
    for (path, article) in &desired {
        match scan.by_path.get(path) {
            Some(on_disk) if *on_disk == **article => {
                unchanged += 1;
                continue;
            }
            Some(_) => {
                // Changed upstream or re-conciliated: overwrite. Git history
                // preserves the previous version.
            }
            None => {
                // New path — but a file may exist that the scan couldn't parse.
                // Fall through to overwrite; serialize errors surface below.
                if scan.paths.contains(path) && !scan.by_path.contains_key(path) {
                    file_errors.push(GalnetError::FileError {
                        filename: (*path).clone(),
                        cause: Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "existing file is not a parseable Article; refusing to overwrite blindly",
                        )),
                    });
                    continue;
                }
            }
        }
        match serialize_to_file(path, *article) {
            Ok(()) => written += 1,
            Err(cause) => file_errors.push(GalnetError::FileError {
                filename: (*path).clone(),
                cause,
            }),
        }
    }

    // Remove anything left on disk that is not desired: stale duplicates,
    // superseded alias-uid files, orphans. Unparseable files are left alone.
    // Same-slot caution: desired paths embed the uid, so a file whose uid
    // is still live (canonical) is never stale even when its
    // `<date> - <page_index>` slot is also claimed by another uid's file.
    // Identity is the uid, not the slot: with stable slots, a stored file
    // for a live uid always matches its desired path (same slot) unless
    // the uid changed dates, in which case the old-date file is cleaned up
    // by the per-uid pass above (same uid, different path).
    let canonical_uids: HashSet<&str> = desired.values().map(|a| a.uid.as_str()).collect();
    for path in &scan.paths {
        if live_paths.contains(path) {
            continue;
        }
        let stale = match scan.by_path.get(path) {
            Some(article) => !canonical_uids.contains(article.uid.as_str()),
            None => false,
        };
        if stale {
            match fs::remove_file(path) {
                Ok(()) => stats.files_removed += 1,
                Err(e) => file_errors.push(GalnetError::FileError {
                    filename: path.clone(),
                    cause: Box::new(e),
                }),
            }
        }
    }

    for error in &file_errors {
        eprintln!("file sync error: {error}");
    }
    if let Some(first) = file_errors.into_iter().next() {
        return Err(Box::new(first));
    }
    Ok((written, unchanged))
}

/// Load carry-over Articles from one [`DiskScan`] by uid (first file wins).
pub(crate) fn load_carried(scan: &DiskScan, uids: &[String]) -> Vec<Article> {
    let mut carried = Vec::with_capacity(uids.len());
    for uid in uids {
        if let Some((_, article)) = scan.by_uid.get(uid) {
            carried.push(Article {
                uid: article.uid.clone(),
                page_index: article.page_index,
                title: article.title.clone(),
                date: article.date.clone(),
                url: article.url.clone(),
                content: article.content.clone(),
                extraction_date: article.extraction_date.clone(),
                deprecated: article.deprecated,
            });
        }
    }
    carried
}

/// Disk uids from one [`DiskScan`]: every uid currently stored.
pub(crate) fn disk_uids(scan: &DiskScan) -> HashSet<String> {
    scan.by_uid.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galnet_site::GalnetSiteArticle;

    fn cms_article(guid: &str, date: &str, title: &str, content: &str) -> ZaonceArticle {
        ZaonceArticle {
            uuid: format!("uuid-{guid}"),
            guid: guid.to_owned(),
            title: title.to_owned(),
            published_at: "2022-01-01T00:00:00+00:00".to_owned(),
            galnet_date: date.to_owned(),
            content: content.to_owned(),
        }
    }

    fn site_article(uid: &str, date: &str, title: &str, content: &str) -> GalnetSiteArticle {
        site_article_on_page(
            uid,
            date,
            title,
            content,
            "https://community.elitedangerous.com/galnet/01-JAN-3308",
            0,
        )
    }

    fn site_article_on_page(
        uid: &str,
        date: &str,
        title: &str,
        content: &str,
        page_url: &str,
        index_in_page: usize,
    ) -> GalnetSiteArticle {
        GalnetSiteArticle {
            uid: uid.to_owned(),
            title: title.to_owned(),
            date: date.to_owned(),
            content: content.to_owned(),
            page_url: page_url.to_owned(),
            index_in_page,
        }
    }

    /// Test helper: empty disk (no uids, no stored slots) — every record is
    /// new and takes slots 0..n in merge order.
    fn merge_fresh(zaonce_cms: &[ZaonceArticle], galnet_site: &[GalnetSiteArticle]) -> MergeOutput {
        merge(
            zaonce_cms,
            galnet_site,
            &HashSet::new(),
            &HashMap::new(),
            "2026-01-01T00:00:00Z",
        )
    }

    #[test]
    fn uid_match_prefers_zaonce_text_and_guid() {
        let z = vec![cms_article("g1", "01 JAN 3308", "CMS Title", "Same body")];
        let c = vec![site_article("g1", "01 JAN 3308", "Site Title", "Same body")];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_uid, 1);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "g1");
        assert_eq!(unified[0].article.title, "CMS Title");
    }

    #[test]
    fn text_match_merges_different_uids_under_zaonce_guid() {
        // Live-row text-match: a freshly scraped site row whose normalized
        // text equals a CMS article files under the CMS guid with an alias.
        let z = vec![cms_article(
            "zg",
            "01 JAN 3308",
            "Same Title",
            "Same body here",
        )];
        let c = vec![site_article(
            "site-guid",
            "01 JAN 3308",
            "Same Title",
            "Same body here",
        )];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_text, 1);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "zg");
        assert!(stats.alias_uids.contains("site-guid"));
    }

    #[test]
    fn disk_row_matching_cms_text_stays_site_only() {
        // No live page proof: a disk-reload row (empty page_url) whose
        // normalized text equals a CMS article must NOT alias — it files
        // as its own site-only article. Normalized text alone is too weak:
        // live date pages serve distinct uids with identical normalized
        // text as distinct divs (e.g. a double-space vs single-space
        // variant), so text-matching disk rows deletes real articles.
        let z = vec![cms_article("zg", "01 JAN 3308", "Same Title", "Same body")];
        let c = vec![site_article_on_page(
            "site-guid",
            "01 JAN 3308",
            "Same Title",
            "Same body",
            "",
            0,
        )];
        let (unified, stats, _, aliases) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_text, 0);
        assert_eq!(stats.site_only, 1);
        assert_eq!(unified.len(), 2);
        assert!(aliases.is_empty());
    }

    #[test]
    fn whitespace_only_differences_still_match() {
        // Live row: CMS `\r\n` vs site `<br />` whitespace collapses under
        // normalization, so this still text-matches.
        let z = vec![cms_article(
            "zg",
            "01 JAN 3308",
            "Title",
            "line one\r\nline two",
        )];
        let c = vec![site_article(
            "other",
            "01 JAN 3308",
            "Title",
            "line one line two",
        )];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_text, 1);
        assert_eq!(unified.len(), 1);
    }

    #[test]
    fn same_text_different_date_does_not_merge() {
        let z = vec![cms_article(
            "zg",
            "01 JAN 3308",
            "Weekly Update",
            "Same syndicated body",
        )];
        let c = vec![site_article(
            "other",
            "08 JAN 3308",
            "Weekly Update",
            "Same syndicated body",
        )];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_text, 0);
        assert_eq!(stats.site_only, 1);
        assert_eq!(unified.len(), 2);
    }

    #[test]
    fn unmatched_community_article_is_site_only() {
        let z = vec![cms_article("zg", "01 JAN 3308", "Other", "Other body")];
        let c = vec![site_article(
            "old",
            "06 JAN 3301",
            "Ancient News",
            "Very old body",
        )];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.site_only, 1);
        assert_eq!(unified.len(), 2);
        assert!(unified.iter().any(|u| u.is_site_only));
    }

    #[test]
    fn empty_title_uses_body_first_line() {
        let z = vec![];
        let c = vec![site_article(
            "e1",
            "07 JAN 3301",
            "",
            "Real Headline\nBody text",
        )];
        let (unified, _, _, _) = merge_fresh(&z, &c);
        assert_eq!(unified[0].article.title, "Real Headline");
    }

    #[test]
    fn disk_only_uid_is_carried() {
        let z = vec![cms_article("zg", "01 JAN 3308", "T", "B")];
        let disk: HashSet<String> = ["zg", "gone-uid"].iter().map(|s| s.to_string()).collect();
        let (_, stats, carried, _) = merge(&z, &[], &disk, &HashMap::new(), "2026-01-01T00:00:00Z");
        assert_eq!(stats.carried_from_disk, 1);
        assert_eq!(carried, vec!["gone-uid".to_owned()]);
    }

    #[test]
    fn live_same_page_same_text_different_uids_stay_separate() {
        // Different uids NEVER collapse on text — even live, even same
        // page. Live-verified: 17 DEC 3301 renders each div twice under
        // the SAME uid (8 divs, 4 uids), and /galnet/uid/<uid> does the
        // same (2 divs, 1 uid). The double-render is always same-uid
        // (already deduped by the fetcher + seen_site_uids); distinct
        // uids with identical normalized text are distinct upstream
        // articles (22 JUL 3301 etc. serve one div per uid).
        let z = vec![];
        let page = "https://community.elitedangerous.com/galnet/17-DEC-3301";
        let c = vec![
            site_article_on_page("aaa", "17 DEC 3301", "Same Title", "Same body", page, 0),
            site_article_on_page("bbb", "17 DEC 3301", "Same Title", "Same body", page, 1),
        ];
        let (unified, stats, _, aliases) = merge_fresh(&z, &c);
        assert_eq!(unified.len(), 2);
        assert_eq!(stats.site_only, 2);
        assert_eq!(stats.site_dupes_collapsed, 0);
        assert!(aliases.is_empty());
    }

    #[test]
    fn disk_rows_with_same_text_stay_separate() {
        // Live-verified: 22 JUL 3301, 07 SEP 3301, 08 JUL 3302, 06 DEC 3304
        // each serve distinct uids as distinct divs (5, 4, 4, 4 divs with
        // one div per uid), some with byte-identical normalized text
        // (double-space or trailing-space variants). Disk-reload rows have
        // empty page_url — no page proof — so they must never collapse:
        // each uid is its own article and both files stay.
        let z = vec![];
        let c = vec![
            site_article_on_page(
                "aaa",
                "22 JUL 3301",
                "Date Set for Imperial Wedding",
                "Same body",
                "",
                1,
            ),
            site_article_on_page(
                "bbb",
                "22 JUL 3301",
                "Date Set for Imperial Wedding",
                "Same body",
                "",
                2,
            ),
        ];
        let disk: HashSet<String> = ["aaa", "bbb"].iter().map(|s| s.to_string()).collect();
        let slots: HashMap<String, (String, usize)> = [
            ("aaa".to_owned(), ("22 JUL 3301".to_owned(), 1)),
            ("bbb".to_owned(), ("22 JUL 3301".to_owned(), 2)),
        ]
        .into_iter()
        .collect();
        let (unified, stats, carried, aliases) =
            merge(&z, &c, &disk, &slots, "2026-01-01T00:00:00Z");
        assert_eq!(unified.len(), 2);
        assert_eq!(stats.site_only, 2);
        assert_eq!(stats.site_dupes_collapsed, 0);
        assert!(aliases.is_empty());
        assert!(carried.is_empty());
    }

    #[test]
    fn same_text_different_pages_stays_separate() {
        // Two live rows from different date-page fetches are two articles,
        // not a dupe — both files stay, no alias.
        let z = vec![];
        let c = vec![
            site_article_on_page(
                "aaa",
                "29 JAN 3311",
                "Titan Wreckage",
                "Same body",
                "https://community.elitedangerous.com/galnet/29-JAN-3311",
                0,
            ),
            site_article_on_page(
                "bbb",
                "29 JAN 3311",
                "Titan Wreckage",
                "Same body",
                "https://community.elitedangerous.com/galnet/30-JAN-3311",
                0,
            ),
        ];
        let disk: HashSet<String> = ["aaa", "bbb"].iter().map(|s| s.to_string()).collect();
        let (unified, stats, carried, aliases) =
            merge(&z, &c, &disk, &HashMap::new(), "2026-01-01T00:00:00Z");
        assert_eq!(unified.len(), 2);
        assert_eq!(stats.site_dupes_collapsed, 0);
        assert!(aliases.is_empty());
        assert!(carried.is_empty());
    }

    #[test]
    fn same_page_sibling_of_a_cms_guid_never_text_collapses() {
        // Live-verified 29 SEP 3308 / 18 DEC 3311: the date page serves the
        // CMS guid AND a same-text sibling uid as separate divs. Same shape
        // as 25 APR 3308 / 29 JAN 3311, where both siblings are in the CMS
        // and are provably distinct articles — so the sibling must survive
        // as site-only instead of collapsing into an alias.
        let page = "https://community.elitedangerous.com/galnet/18-DEC-3311";
        let z = vec![cms_article("cms1", "18 DEC 3311", "T", "B")];
        let c = vec![
            site_article_on_page("cms1", "18 DEC 3311", "T", "B", page, 0),
            site_article_on_page("sib", "18 DEC 3311", "T", "B", page, 1),
        ];
        let (unified, stats, _, aliases) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_uid, 1);
        assert_eq!(stats.matched_by_text, 0);
        assert_eq!(stats.site_only, 1);
        assert!(aliases.is_empty());
        let uids: Vec<&str> = unified.iter().map(|u| u.article.uid.as_str()).collect();
        assert_eq!(uids.len(), 2);
        assert!(uids.contains(&"cms1") && uids.contains(&"sib"));
    }

    #[test]
    fn cms_text_match_beats_site_site_collapse_for_alias_target() {
        let z = vec![cms_article("zg", "01 JAN 3308", "T", "B")];
        let c = vec![
            site_article("s1", "01 JAN 3308", "T", "B"),
            site_article("s2", "01 JAN 3308", "T", "B"),
        ];
        let (unified, stats, _, aliases) = merge_fresh(&z, &c);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "zg");
        assert_eq!(stats.matched_by_text, 2);
        assert_eq!(stats.site_dupes_collapsed, 0);
        assert_eq!(
            aliases,
            vec![
                ("s1".to_owned(), "zg".to_owned()),
                ("s2".to_owned(), "zg".to_owned()),
            ]
        );
    }

    #[test]
    fn same_uid_with_drifted_text_still_counts_as_uid_match() {
        // CMS copy-edits titles over time; the site uid still identifies it.
        let z = vec![cms_article(
            "g1",
            "01 JAN 3308",
            "New CMS Title",
            "Same body",
        )];
        let c = vec![site_article(
            "g1",
            "01 JAN 3308",
            "Old Site Title",
            "Same body",
        )];
        let (unified, stats, _, _) = merge_fresh(&z, &c);
        assert_eq!(stats.matched_by_uid, 1);
        assert_eq!(stats.site_only, 0);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.title, "New CMS Title");
    }

    #[test]
    fn known_uids_keep_stored_slots_new_uids_fill_gaps() {
        // Stable slots: stored (date, page_index) pins the slot; a new uid
        // in the same date takes the smallest free slot (gap 1 from a
        // deleted uid), and a uid whose date changed frees its old slot.
        let z = vec![];
        let c = vec![
            site_article_on_page("keep", "01 JAN 3308", "T", "B", "", 5),
            site_article_on_page("moved", "02 JAN 3308", "T", "B", "", 0),
            site_article_on_page("new", "01 JAN 3308", "N", "B2", "", 0),
        ];
        let slots: HashMap<String, (String, usize)> = [
            ("keep".to_owned(), ("01 JAN 3308".to_owned(), 5)),
            ("moved".to_owned(), ("01 JAN 3308".to_owned(), 1)),
        ]
        .into_iter()
        .collect();
        let (unified, _, _, _) = merge(&z, &c, &HashSet::new(), &slots, "2026-01-01T00:00:00Z");
        let slot_of = |uid: &str| {
            unified
                .iter()
                .find(|u| u.article.uid == uid)
                .map(|u| (u.article.date.clone(), u.article.page_index))
        };
        assert_eq!(
            slot_of("keep"),
            Some(("01 JAN 3308".to_owned(), 5)),
            "stored slot sticks even though merge order is 0-based"
        );
        assert_eq!(
            slot_of("moved"),
            Some(("02 JAN 3308".to_owned(), 0)),
            "date change takes first slot of the new date"
        );
        assert_eq!(
            slot_of("new"),
            Some(("01 JAN 3308".to_owned(), 0)),
            "new uid takes smallest free slot (0 is free; 5 claimed by keeper)"
        );
    }

    #[test]
    fn fresh_disk_assigns_slots_in_merge_order() {
        // No stored slots: CMS records first (API order), then site-only
        // records (page order) — slots 0..n within the date.
        let z = vec![
            cms_article("z1", "01 JAN 3308", "T1", "B1"),
            cms_article("z2", "01 JAN 3308", "T2", "B2"),
        ];
        let c = vec![site_article("s1", "01 JAN 3308", "T3", "B3")];
        let (unified, _, _, _) = merge_fresh(&z, &c);
        let slots: Vec<(&str, usize)> = unified
            .iter()
            .map(|u| (u.article.uid.as_str(), u.article.page_index))
            .collect();
        assert_eq!(slots, vec![("z1", 0), ("z2", 1), ("s1", 2)]);
    }
}
