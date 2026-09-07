//! Galnet extraction from Frontier's Drupal JSON:API (`cms.zaonce.net`).
//!
//! This module replaces the old community-site HTML scraper. The API collection
//! only covers 07 DEC 3306 onwards, so per-date files already on disk for older
//! articles are left untouched; overlapping articles are upserted in place.

use crate::common::{
    Article, EXTRACTED_FILES_LOCATION, GalnetError, SYNC_STATE_FILE, SyncState,
    deserialize_from_file, serialize_to_file,
};

use chrono::prelude::Utc;
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs,
    sync::OnceLock,
    time::Duration,
};

const ZAONCE_COLLECTION_URL: &str = "https://cms.zaonce.net/en-GB/jsonapi/node/galnet_article";
const COMMUNITY_UID_URL: &str = "https://community.elitedangerous.com/galnet/uid";
const PAGE_LIMIT: &str = "50";
const USER_AGENT: &str = "ed-galnet-scraper/0.1.0";
const ACCEPT_JSONAPI: &str = "application/vnd.api+json";
const MAX_ATTEMPTS: u32 = 3;

fn date_matcher() -> &'static Regex {
    static MATCHER: OnceLock<Regex> = OnceLock::new();
    MATCHER.get_or_init(|| {
        Regex::new(r"(\d{2})[\s-](\w{3})[\s-](\d{4,})").expect("Article date matcher")
    })
}

// JSON:API response shapes. Only the fields we persist are modelled;
// unknown fields are ignored by serde.
#[derive(Debug, Deserialize)]
struct JsonApiResponse {
    #[serde(default)]
    data: Vec<JsonApiNode>,
    #[serde(default)]
    links: JsonApiLinks,
}

#[derive(Debug, Default, Deserialize)]
struct JsonApiLinks {
    #[serde(default)]
    next: Option<JsonApiLink>,
}

#[derive(Debug, Deserialize)]
struct JsonApiLink {
    href: String,
}

#[derive(Debug, Deserialize)]
struct JsonApiNode {
    #[serde(default)]
    id: String,
    #[serde(default)]
    attributes: JsonApiAttributes,
}

#[derive(Debug, Default, Deserialize)]
struct JsonApiAttributes {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<JsonApiBody>,
    #[serde(default)]
    field_galnet_date: Option<String>,
    #[serde(default)]
    field_galnet_guid: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonApiBody {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug)]
struct RawArticle {
    uuid: String,
    guid: String,
    title: String,
    published_at: String,
    galnet_date: String,
    content: String,
}

impl RawArticle {
    /// Returns `None` when the node lacks the fields needed to file it
    /// (`field_galnet_guid` or `field_galnet_date`).
    fn from_node(node: JsonApiNode) -> Option<Self> {
        let attrs = node.attributes;
        Some(RawArticle {
            uuid: node.id,
            guid: attrs.field_galnet_guid?,
            title: attrs.title.unwrap_or_default(),
            published_at: attrs.published_at.unwrap_or_default(),
            galnet_date: attrs.field_galnet_date?,
            content: attrs
                .body
                .and_then(|body| body.value)
                .unwrap_or_default()
                .replace("\r\n", "\n"),
        })
    }
}

async fn fetch_page(
    client: &reqwest::Client,
    href: Option<&str>,
) -> Result<JsonApiResponse, Box<dyn Error>> {
    let url = href.unwrap_or(ZAONCE_COLLECTION_URL).to_owned();
    let mut last_error: Option<Box<dyn Error>> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let request = match href {
            Some(h) => client.get(h),
            None => client.get(ZAONCE_COLLECTION_URL).query(&[
                ("sort", "-published_at"),
                ("page[offset]", "0"),
                ("page[limit]", PAGE_LIMIT),
            ]),
        }
        .header(reqwest::header::ACCEPT, ACCEPT_JSONAPI);
        match request.send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<JsonApiResponse>().await {
                    Ok(payload) => return Ok(payload),
                    Err(e) => last_error = Some(Box::new(e)),
                },
                Err(e) => last_error = Some(Box::new(e)),
            },
            Err(e) => last_error = Some(Box::new(e)),
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
        }
    }
    Err(Box::new(GalnetError::ScraperError {
        url,
        cause: last_error.expect("fetch_page must have an error after retries"),
    }))
}

async fn fetch_all_nodes(client: &reqwest::Client) -> Result<Vec<RawArticle>, Box<dyn Error>> {
    let mut articles = Vec::new();
    let mut seen_uuids = HashSet::new();
    let mut seen_guids = HashSet::new();
    let mut visited_hrefs = HashSet::new();
    let mut skipped_duplicates = 0usize;
    let mut skipped_incomplete = 0usize;
    let mut href: Option<String> = None;
    let mut page_count = 0usize;

    loop {
        let payload = fetch_page(client, href.as_deref()).await?;
        page_count += 1;
        if payload.data.is_empty() {
            break;
        }
        for node in payload.data {
            // Offset pagination is slightly unstable when many articles share
            // the same `published_at`; deduplicate by Drupal UUID.
            if !seen_uuids.insert(node.id.clone()) {
                skipped_duplicates += 1;
                continue;
            }
            match RawArticle::from_node(node) {
                Some(article) => {
                    if !seen_guids.insert(article.guid.clone()) {
                        skipped_duplicates += 1;
                        continue;
                    }
                    articles.push(article);
                }
                None => skipped_incomplete += 1,
            }
        }
        match payload.links.next {
            Some(link) if !visited_hrefs.contains(&link.href) => {
                visited_hrefs.insert(link.href.clone());
                href = Some(link.href);
            }
            _ => break,
        }
    }

    println!(
        "Fetched {} pages: {} unique articles ({} duplicates, {} incomplete skipped)",
        page_count,
        articles.len(),
        skipped_duplicates,
        skipped_incomplete
    );
    Ok(articles)
}

/// `"03 SEP 3312"` -> `"3312 SEP 03"` so per-date files sort chronologically.
fn revert_galnet_date(date: &str) -> String {
    if let Some(cap) = date_matcher().captures(date) {
        format!("{} {} {}", &cap[3], &cap[2], &cap[1])
    } else {
        date.to_owned()
    }
}

fn build_file_plan(raw_articles: &[RawArticle], extraction_date: &str) -> Vec<(String, Article)> {
    // Group by in-game date, preserving newest-first fetch order. The position
    // inside each date group is the article's `page_index`, matching the order
    // the community site lists them in.
    let mut date_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&RawArticle>> = HashMap::new();
    for article in raw_articles {
        groups
            .entry(article.galnet_date.clone())
            .or_insert_with(|| {
                date_order.push(article.galnet_date.clone());
                Vec::new()
            })
            .push(article);
    }

    let mut plan = Vec::new();
    for date in &date_order {
        for (page_index, raw) in groups[date.as_str()].iter().enumerate() {
            let filename = format!(
                "{}/{} - {} - {}.json",
                EXTRACTED_FILES_LOCATION,
                revert_galnet_date(date),
                page_index,
                raw.guid
            );
            plan.push((
                filename,
                Article {
                    uid: raw.guid.clone(),
                    page_index,
                    title: raw.title.clone(),
                    date: date.clone(),
                    url: format!("{}/{}", COMMUNITY_UID_URL, raw.guid),
                    content: raw.content.clone(),
                    extraction_date: extraction_date.to_owned(),
                    deprecated: false,
                },
            ));
        }
    }
    plan
}

fn index_existing_files() -> HashMap<String, Vec<String>> {
    let mut existing: HashMap<String, Vec<String>> = HashMap::new();
    let entries = match fs::read_dir(EXTRACTED_FILES_LOCATION) {
        Ok(entries) => entries,
        Err(_) => return existing,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        // Unparseable files are left alone; they are reported at startup.
        if let Ok(Some(article)) = deserialize_from_file::<Article>(path_str) {
            existing
                .entry(article.uid)
                .or_default()
                .push(path_str.to_owned());
        }
    }
    existing
}

fn sync_files_to_disk(plan: &[(String, Article)]) -> Result<(usize, usize), Box<dyn Error>> {
    fs::create_dir_all(EXTRACTED_FILES_LOCATION)?;

    let existing = index_existing_files();
    let mut written = 0usize;
    let mut unchanged = 0usize;
    let mut file_errors: Vec<GalnetError> = Vec::new();

    for (filename, article) in plan {
        // A known uid may live under a stale filename when its `page_index`
        // within the date group shifted. If the content is identical, move the
        // file to the canonical name; otherwise delete the stale file so each
        // uid maps to exactly one file. This also collapses legacy duplicates
        // where the same uid was stored under two `page_index` values.
        // Uids absent from the API (pre-Dec-2020 archive) are never touched.
        if let Some(known_files) = existing.get(&article.uid) {
            for known in known_files {
                if known == filename {
                    continue;
                }
                match deserialize_from_file::<Article>(known) {
                    Ok(Some(ref on_disk)) if *on_disk == *article => {
                        if let Err(e) = fs::rename(known, filename) {
                            file_errors.push(GalnetError::FileError {
                                filename: known.clone(),
                                cause: Box::new(e),
                            });
                        }
                    }
                    _ => {
                        if let Err(e) = fs::remove_file(known) {
                            file_errors.push(GalnetError::FileError {
                                filename: known.clone(),
                                cause: Box::new(e),
                            });
                        }
                    }
                }
            }
        }

        match deserialize_from_file::<Article>(filename) {
            Ok(Some(ref on_disk)) if *on_disk == *article => {
                unchanged += 1;
                continue;
            }
            Ok(Some(_on_disk)) => {
                // Content changed upstream (e.g. title typo fixed in the CMS):
                // just overwrite. Git history preserves the previous version,
                // so no separate backup copy is needed.
            }
            Ok(None) => {}
            Err(cause) => {
                file_errors.push(GalnetError::FileError {
                    filename: filename.clone(),
                    cause,
                });
                continue;
            }
        }

        match serialize_to_file(filename, article) {
            Ok(()) => written += 1,
            Err(cause) => file_errors.push(GalnetError::FileError {
                filename: filename.clone(),
                cause,
            }),
        }
    }

    for error in &file_errors {
        eprintln!("file sync error: {}", error);
    }
    if let Some(first) = file_errors.into_iter().next() {
        return Err(Box::new(first));
    }
    Ok((written, unchanged))
}

pub async fn extract_all_articles() -> Result<(), Box<dyn Error>> {
    let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;

    let raw_articles = fetch_all_nodes(&client).await?;
    if raw_articles.is_empty() {
        return Err(Box::new(GalnetError::ParserError {
            cause: "JSON:API collection returned no articles".to_owned(),
        }));
    }

    let extraction_date = Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let plan = build_file_plan(&raw_articles, &extraction_date);
    let (written, unchanged) = sync_files_to_disk(&plan)?;
    println!(
        "{} articles synced: {} written, {} already up to date",
        plan.len(),
        written,
        unchanged
    );

    let newest = &raw_articles[0];
    let oldest = &raw_articles[raw_articles.len() - 1];
    serialize_to_file(
        SYNC_STATE_FILE,
        &SyncState {
            extraction_date,
            article_count: plan.len(),
            newest_published_at: newest.published_at.clone(),
            newest_uuid: newest.uuid.clone(),
            oldest_published_at: oldest.published_at.clone(),
        },
    )?;
    println!(
        "Range: {} ({}) -> {} ({})",
        oldest.galnet_date, oldest.published_at, newest.galnet_date, newest.published_at
    );
    Ok(())
}
