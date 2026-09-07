mod common;
mod galnet_site;
mod merge;
mod zaonce_cms;

use std::{collections::HashMap, error::Error};

use chrono::prelude::Utc;

use common::{
    DOWNLOADED_PAGES_FILE, EMPTY_PAGES_FILE, FAILED_PAGES_FILE, GALNET_SITE, SYNC_STATE_FILE,
    SyncState, USER_AGENT, deserialize_from_file, read_string_set, serialize_to_file,
};
use galnet_site::{ErroredPage, GalnetSiteArticle, discover_pages, fetch_pages};
use merge::{disk_uids, load_carried, sync_to_disk};
use zaonce_cms::fetch_all as fetch_zaonce_cms;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;

    // ---- zaonce_cms JSON:API (canonical from 07 DEC 3306) ----
    let zaonce_cms = fetch_zaonce_cms(&client).await?;
    if zaonce_cms.is_empty() {
        return Err(Box::new(common::GalnetError::ParserError {
            cause: "JSON:API collection returned no articles".to_owned(),
        }) as Box<dyn Error>);
    }
    println!("zaonce_cms: {} articles", zaonce_cms.len());

    // ---- galnet_site (only source before 07 DEC 3306; fallback after) ----
    let mut discovered = discover_pages(&client).await?;
    println!("galnet_site: {} date pages discovered", discovered.len());
    // Reconcile with history: keep tracking pages that vanished from the
    // homepage so their downloaded status is not lost.
    let known_downloaded = read_string_set(DOWNLOADED_PAGES_FILE)?;
    for page in &known_downloaded {
        if !discovered.contains(page) {
            discovered.push(page.clone());
        }
    }
    discovered.sort();

    // Skip pages already downloaded; retry empties and failures every run
    // (content upstream can change: empty pages may gain articles).
    let empty_known = read_string_set(EMPTY_PAGES_FILE)?;
    let failed_known: HashMap<String, Vec<String>> = deserialize_from_file(FAILED_PAGES_FILE)?
        .map(|entries: Vec<ErroredPage>| {
            entries
                .into_iter()
                .map(|entry| (entry.url, entry.errors))
                .collect()
        })
        .unwrap_or_default();
    let mut todo: Vec<String> = discovered
        .iter()
        .filter(|page| !known_downloaded.contains(page.as_str()))
        .cloned()
        .collect();

    // Repair pass: re-scrape pages whose stored articles still carry the old
    // extraction bugs (empty titles) or the duplicated page_index copies.
    let repair_pages = repair_pages();
    let mut repair_new = 0usize;
    for page in repair_pages {
        if known_downloaded.contains(&page) && !todo.contains(&page) {
            todo.push(page);
            repair_new += 1;
        }
    }
    todo.sort();
    println!(
        "galnet_site: {} pages todo ({} known ok, {} known empty, {} known failed, {} repair)",
        todo.len(),
        known_downloaded.len(),
        empty_known.len(),
        failed_known.len(),
        repair_new
    );

    let fetch = fetch_pages(&client, &todo).await;
    println!(
        "galnet_site: {} articles from {} ok pages ({} empty, {} failed, {} dupes collapsed, {} blocks skipped)",
        fetch.articles.len(),
        fetch.ok_pages.len(),
        fetch.empty_pages.len(),
        fetch.failed_pages.len(),
        fetch.dupes_collapsed,
        fetch.skipped_blocks
    );

    // Persist page bookkeeping (downloaded / empty / failed), unioned with
    // history.
    let mut downloaded = known_downloaded;
    downloaded.extend(fetch.ok_pages.iter().cloned());
    let mut downloaded: Vec<String> = downloaded.into_iter().collect();
    downloaded.sort();
    serialize_to_file(DOWNLOADED_PAGES_FILE, &downloaded)?;

    let mut empty = empty_known;
    for page in &fetch.empty_pages {
        empty.insert(page.clone());
    }
    // Pages that now parse are no longer empty.
    for page in &fetch.ok_pages {
        empty.remove(page);
    }
    let mut empty: Vec<String> = empty.into_iter().collect();
    empty.sort();
    serialize_to_file(EMPTY_PAGES_FILE, &empty)?;

    let mut failed = failed_known;
    for page in &fetch.failed_pages {
        failed.insert(page.url.clone(), page.errors.clone());
    }
    for page in fetch.ok_pages.iter().chain(fetch.empty_pages.iter()) {
        failed.remove(page);
    }
    let mut failed: Vec<ErroredPage> = failed
        .into_iter()
        .map(|(url, errors)| ErroredPage { url, errors })
        .collect();
    failed.sort_by(|a, b| a.url.cmp(&b.url));
    serialize_to_file(FAILED_PAGES_FILE, &failed)?;

    // ---- conciliate ----
    // NOTE: `fetch.articles` only covers *todo* pages this run. The merge
    // needs the full galnet_site picture (all downloaded pages), so reload
    // previously fetched articles from the stored files as the site side too.
    let disk = disk_uids();
    let extraction_date = Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut site_all = load_site_from_disk();
    {
        let mut seen: std::collections::HashSet<String> = site_all
            .iter()
            .map(|a: &GalnetSiteArticle| a.uid.clone())
            .collect();
        for article in fetch.articles {
            if seen.insert(article.uid.clone()) {
                site_all.push(article);
            }
        }
    }
    let (unified, mut stats, carried_uids) =
        merge::merge(&zaonce_cms, &site_all, &disk, &extraction_date);
    let carried = load_carried(&carried_uids);
    println!(
        "merge: {} unified ({} uid-match, {} text-match, {} site-only, {} carried from disk)",
        unified.len(),
        stats.matched_by_uid,
        stats.matched_by_text,
        stats.site_only,
        carried.len()
    );

    let (written, unchanged) = sync_to_disk(&unified, &carried, &mut stats)?;
    let total = unified.len() + carried.len();
    println!(
        "{total} articles synced: {written} written, {unchanged} already up to date, {} files removed",
        stats.files_removed
    );

    // uid aliases (galnet_site uid -> canonical zaonce_cms guid). Only pairs
    // where the canonical side is a zaonce_cms article count; purely
    // site-side same-text reposts (e.g. weekly placeholders repeated under
    // different uids on different runs) are data, not aliases.
    let cms_guids: std::collections::HashSet<&str> =
        zaonce_cms.iter().map(|a| a.guid.as_str()).collect();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let canonical: HashMap<String, String> = unified
        .iter()
        .filter(|u| cms_guids.contains(u.article.uid.as_str()))
        .map(|u| {
            (
                crate::merge::text_key(&u.article.date, &u.article.title, &u.article.content),
                u.article.uid.clone(),
            )
        })
        .collect();
    for article in &site_all {
        let key = crate::merge::text_key(&article.date, &article.title, &article.content);
        if let Some(canonical_uid) = canonical.get(&key)
            && &article.uid != canonical_uid
        {
            aliases.insert(article.uid.clone(), canonical_uid.clone());
        }
    }
    let mut aliases: Vec<(String, String)> = aliases.into_iter().collect();
    aliases.sort();
    serialize_to_file("./galnet/aliases.json", &aliases)?;
    println!("aliases: {}", aliases.len());

    // ---- sync state ----
    let newest = &zaonce_cms[0];
    let oldest = &zaonce_cms[zaonce_cms.len() - 1];
    serialize_to_file(
        SYNC_STATE_FILE,
        &SyncState {
            extraction_date,
            article_count: total,
            newest_published_at: newest.published_at.clone(),
            newest_uuid: newest.uuid.clone(),
            oldest_published_at: oldest.published_at.clone(),
            galnet_site_pages_total: discovered.len(),
            galnet_site_pages_ok: downloaded.len(),
            galnet_site_pages_empty: empty.len(),
            galnet_site_pages_failed: failed.len(),
            galnet_site_articles: site_all.len(),
            matched_by_uid: stats.matched_by_uid,
            matched_by_text: stats.matched_by_text,
            site_only: stats.site_only,
            carried_from_disk: carried.len(),
            files_removed: stats.files_removed,
        },
    )?;
    println!(
        "zaonce_cms range: {} ({}) -> {} ({})",
        oldest.galnet_date, oldest.published_at, newest.galnet_date, newest.published_at
    );
    println!("site: {GALNET_SITE}");
    Ok(())
}

/// Reconstruct galnet_site-side articles from the stored files so the merge
/// sees every downloaded page, not just the pages fetched this run.
fn load_site_from_disk() -> Vec<GalnetSiteArticle> {
    use common::{EXTRACTED_FILES_LOCATION, deserialize_from_file};
    use std::fs;

    let mut articles = Vec::new();
    let Ok(entries) = fs::read_dir(EXTRACTED_FILES_LOCATION) else {
        return articles;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        if let Ok(Some(stored)) = deserialize_from_file::<common::Article>(path_str) {
            articles.push(GalnetSiteArticle {
                uid: stored.uid,
                title: stored.title,
                date: stored.date,
                content: stored.content,
                page_url: String::new(),
                index_in_page: stored.page_index,
            });
        }
    }
    articles.sort_by(|a: &GalnetSiteArticle, b: &GalnetSiteArticle| {
        a.date.cmp(&b.date).then(a.uid.cmp(&b.uid))
    });
    articles
}

/// Pages to re-scrape once: stored files with empty titles (old extraction
/// bug) or duplicated `page_index` copies of the same uid.
fn repair_pages() -> Vec<String> {
    use common::{EXTRACTED_FILES_LOCATION, deserialize_from_file};
    use std::{collections::HashMap, fs};

    let mut by_uid: HashMap<String, Vec<(String, common::Article)>> = HashMap::new();
    let Ok(entries) = fs::read_dir(EXTRACTED_FILES_LOCATION) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        if let Ok(Some(article)) = deserialize_from_file::<common::Article>(path_str) {
            by_uid
                .entry(article.uid.clone())
                .or_default()
                .push((path_str.to_owned(), article));
        }
    }

    let mut pages = std::collections::HashSet::new();
    for files in by_uid.values() {
        let needs_repair = files.iter().any(|(_, a)| a.title.trim().is_empty()) || files.len() > 1;
        if !needs_repair {
            continue;
        }
        for (_, article) in files {
            // date "17 DEC 3301" -> page slug "17-DEC-3301"
            let slug = article.date.replace(' ', "-");
            pages.insert(format!("{GALNET_SITE}/galnet/{slug}"));
        }
    }
    let mut pages: Vec<String> = pages.into_iter().collect();
    pages.sort();
    pages
}
