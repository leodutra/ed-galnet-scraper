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
//! 2. **by text** — normalized `(date, title, content)` matches a zaonce_cms
//!    article. The date is part of the key so recurring syndicated placeholders
//!    (e.g. weekly powerplay updates reposted verbatim for months) never
//!    collapse into a single entry.
//!
//! Winners and losers:
//!
//! - A galnet_site article matching zaonce_cms is written under the **zaonce_cms
//!   guid** and zaonce_cms text (cleaner source of truth). Its galnet_site uid
//!   survives in `galnet/aliases.json` so the old URL keeps resolving.
//! - A galnet_site article with no match is kept as-is (**site-only**), e.g.
//!   pre-3306 articles or newer ones missing from the API.
//! - Duplicate files (same uid stored under several `page_index`
//!   values) collapse to the canonical winner; the extras are deleted.
//! - Files on disk whose uid matches nothing from either source are carried
//!   over untouched (e.g. dates the live site no longer serves, or uids the
//!   galnet_site has since replaced).
//! - Anything else on disk (stale duplicates, superseded uids) is removed.
//!
//! `page_index` is the position within an in-game date group. zaonce_cms order
//! (newest-first fetch) sets it; site-only groups keep galnet_site page order.

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
    /// Same normalized text under a second site uid within one date.
    pub(crate) site_dupes_collapsed: usize,
    pub(crate) carried_from_disk: usize,
    pub(crate) files_removed: usize,
    /// galnet_site uids that resolve to a different canonical zaonce_cms guid
    pub(crate) alias_uids: HashSet<String>,
}

/// The single alias list: (site uid, canonical uid), sorted. Covers both CMS
/// text-matches and collapsed site-site duplicates.
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
/// skipped defensively. Returns unified records, stats, carry-over uids (on
/// disk but matched by neither source), and the alias list
/// (site uid -> canonical uid).
pub(crate) fn merge(
    zaonce_cms: &[ZaonceArticle],
    galnet_site: &[GalnetSiteArticle],
    disk_uids: &HashSet<String>,
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
    // of text drift; (2) normalized text. The uid check must come first:
    // CMS copy-edits titles/content over time, so a same-uid pair with
    // drifted text must still count as a uid-match, not site-only.
    //
    // Site-side collapse is order-dependent, so anchor the winner: the
    // earliest on-page position wins; disk-reload rows (empty page_url)
    // keep their stored page_index, so re-runs elect the same winner.
    // Final tiebreak is the uid itself — deterministic across runs.
    //
    // NOTE: this collapse only fires when both dupes reach the merge in one
    // run. A dupe whose uid already has a file on disk wins by carry-over
    // instead: its uid is live, the loser is carried, and both files stay.
    // Live-verified 25 APR 3308 / 29 JAN 3311 pages serve two distinct uids
    // with near-identical text, so collapsing on normalized text alone
    // would destroy a real upstream article. True collapse needs a page
    // fetch proving same-page duplication (same page_url), not disk rows.
    let mut site_sorted: Vec<&GalnetSiteArticle> = galnet_site.iter().collect();
    site_sorted.sort_by(|a, b| {
        (a.page_url.clone(), a.index_in_page, a.uid.clone()).cmp(&(
            b.page_url.clone(),
            b.index_in_page,
            b.uid.clone(),
        ))
    });
    // (page_url, text-key) -> canonical site uid (first in page order wins).
    // Keyed by page so only same-page reposts collapse — never two disk rows
    // from unknown pages, never two live rows from different date pages.
    let mut site_text_seen: HashMap<(String, String), String> = HashMap::new();
    // canonical site uid -> CMS guid for text-matches (alias source).
    let mut canonical_of: HashMap<String, String> = HashMap::new();
    // Per-run uid dedupe (pages render each article div twice).
    let mut seen_site_uids: HashSet<&str> = HashSet::new();
    for article in site_sorted {
        if !seen_site_uids.insert(article.uid.as_str()) {
            continue;
        }
        let matched_cms: Option<(&ZaonceArticle, bool)> = match by_guid.get(article.uid.as_str()) {
            Some(z) => Some((z, true)),
            None => by_text.get(&site_key(article)).copied().map(|z| (z, false)),
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
                // Site-only: keep galnet_site text verbatim, collapsing
                // same-page same-text reposts to the first uid.
                let key = (article.page_url.clone(), site_key(article));
                if let Some(winner) = site_text_seen.get(&key) {
                    stats.site_dupes_collapsed += 1;
                    stats.alias_uids.insert(article.uid.clone());
                    canonical_of.insert(article.uid.clone(), winner.clone());
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

    // 4. assign page_index per date group and build Articles.
    let mut unified = Vec::new();
    for date in &date_order {
        let records = &groups[date.as_str()];
        for (page_index, (uid, title, record_date, content)) in records.iter().enumerate() {
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

    #[test]
    fn uid_match_prefers_zaonce_text_and_guid() {
        let z = vec![cms_article("g1", "01 JAN 3308", "CMS Title", "Same body")];
        let c = vec![site_article("g1", "01 JAN 3308", "Site Title", "Same body")];
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
        assert_eq!(stats.matched_by_uid, 1);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "g1");
        assert_eq!(unified[0].article.title, "CMS Title");
    }

    #[test]
    fn text_match_merges_different_uids_under_zaonce_guid() {
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
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
        assert_eq!(stats.matched_by_text, 1);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "zg");
        assert!(stats.alias_uids.contains("site-guid"));
    }

    #[test]
    fn whitespace_only_differences_still_match() {
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
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
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
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
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
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
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
        let (unified, _, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
        assert_eq!(unified[0].article.title, "Real Headline");
    }

    #[test]
    fn disk_only_uid_is_carried() {
        let z = vec![cms_article("zg", "01 JAN 3308", "T", "B")];
        let disk: HashSet<String> = ["zg", "gone-uid"].iter().map(|s| s.to_string()).collect();
        let (_, stats, carried, _) = merge(&z, &[], &disk, "2026-01-01T00:00:00Z");
        assert_eq!(stats.carried_from_disk, 1);
        assert_eq!(carried, vec!["gone-uid".to_owned()]);
    }

    #[test]
    fn site_same_text_collapses_to_first_uid_with_alias() {
        // Same page, byte-identical text under two uids: page-render dupe.
        let z = vec![];
        let page = "https://community.elitedangerous.com/galnet/29-JAN-3311";
        let c = vec![
            site_article_on_page("aaa", "29 JAN 3311", "Titan Wreckage", "Same body", page, 0),
            site_article_on_page("bbb", "29 JAN 3311", "Titan Wreckage", "Same body", page, 1),
        ];
        let (unified, stats, _, aliases) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.uid, "aaa");
        assert_eq!(stats.site_only, 1);
        assert_eq!(stats.site_dupes_collapsed, 1);
        assert_eq!(aliases, vec![("bbb".to_owned(), "aaa".to_owned())]);
    }

    #[test]
    fn same_text_different_pages_stays_separate() {
        // Live-verified 25 APR 3308 / 29 JAN 3311: the site serves two
        // distinct uids with near-identical text. Without page proof they
        // are two articles, not a dupe — both files stay.
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
            site_article_on_page("bbb", "29 JAN 3311", "Titan Wreckage", "Same body", "", 1),
        ];
        let disk: HashSet<String> = ["aaa", "bbb"].iter().map(|s| s.to_string()).collect();
        let (unified, stats, carried, aliases) = merge(&z, &c, &disk, "2026-01-01T00:00:00Z");
        assert_eq!(unified.len(), 2);
        assert_eq!(stats.site_dupes_collapsed, 0);
        assert!(aliases.is_empty());
        assert!(carried.is_empty());
    }

    #[test]
    fn cms_text_match_beats_site_site_collapse_for_alias_target() {
        let z = vec![cms_article("zg", "01 JAN 3308", "T", "B")];
        let c = vec![
            site_article("s1", "01 JAN 3308", "T", "B"),
            site_article("s2", "01 JAN 3308", "T", "B"),
        ];
        let (unified, stats, _, aliases) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
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
        let (unified, stats, _, _) = merge(&z, &c, &HashSet::new(), "2026-01-01T00:00:00Z");
        assert_eq!(stats.matched_by_uid, 1);
        assert_eq!(stats.site_only, 0);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].article.title, "New CMS Title");
    }
}
