//! Galnet fetching from Frontier's Drupal JSON:API (`cms.zaonce.net`).
//!
//! The API collection only covers 07 DEC 3306 onwards; see `galnet_site` for
//! the site (the only source for older articles) and `merge` for how the two
//! sources are unified into one archive.

use crate::common::{get_text_with_retry, strip_paragraph_wrapper};

use indicatif::ProgressBar;
use serde::Deserialize;
use std::{collections::HashSet, error::Error};

pub(crate) const ZAONCE_COLLECTION_URL: &str =
    "https://cms.zaonce.net/en-GB/jsonapi/node/galnet_article";
const PAGE_LIMIT: &str = "50";

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

pub(crate) struct ZaonceArticle {
    pub(crate) uuid: String,
    pub(crate) guid: String,
    pub(crate) title: String,
    pub(crate) published_at: String,
    pub(crate) galnet_date: String,
    pub(crate) content: String,
}

impl ZaonceArticle {
    /// Returns `None` when the node lacks the fields needed to file it
    /// (`field_galnet_guid` or `field_galnet_date`).
    fn from_node(node: JsonApiNode) -> Option<Self> {
        let attrs = node.attributes;
        let raw_content = attrs
            .body
            .and_then(|body| body.value)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        Some(ZaonceArticle {
            uuid: node.id,
            guid: attrs.field_galnet_guid?,
            title: attrs.title.unwrap_or_default(),
            published_at: attrs.published_at.unwrap_or_default(),
            galnet_date: attrs.field_galnet_date?,
            content: strip_paragraph_wrapper(&raw_content),
        })
    }
}

async fn fetch_page(
    client: &reqwest::Client,
    href: Option<&str>,
) -> Result<JsonApiResponse, Box<dyn Error>> {
    // First page builds the collection URL; later pages follow the
    // API-provided `links.next` verbatim. Both use plain GET: verified
    // live that the collection returns JSON:API without the custom
    // `Accept: application/vnd.api+json` header.
    let url = match href {
        Some(h) => h.to_owned(),
        None => format!(
            "{ZAONCE_COLLECTION_URL}?sort=-published_at&page%5Boffset%5D=0&page%5Blimit%5D={PAGE_LIMIT}"
        ),
    };
    Ok(serde_json::from_str(
        &get_text_with_retry(client, &url).await?,
    )?)
}

pub(crate) async fn fetch_all(
    client: &reqwest::Client,
    progress: &ProgressBar,
) -> Result<Vec<ZaonceArticle>, Box<dyn Error>> {
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
            match ZaonceArticle::from_node(node) {
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
        progress.inc(1);
        progress.set_message(format!("page {page_count} · {} articles", articles.len()));
        match payload.links.next {
            Some(link) if !visited_hrefs.contains(&link.href) => {
                visited_hrefs.insert(link.href.clone());
                href = Some(link.href);
            }
            _ => break,
        }
    }

    // Clear the spinner before the stdout summary so the finished
    // line doesn't linger above it on a terminal.
    progress.finish_and_clear();
    println!(
        "Fetched {} pages: {} unique articles ({} duplicates, {} incomplete skipped)",
        page_count,
        articles.len(),
        skipped_duplicates,
        skipped_incomplete
    );
    Ok(articles)
}
