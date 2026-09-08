//! Galnet extraction from the galnet_site
//! (`community.elitedangerous.com/galnet/<DD-MON-YYYY>`).
//!
//! The zaonce_cms JSON:API only covers 07 DEC 3306 onwards, so everything older
//! (plus a few newer articles missing from the API) must still come from here.
//!
//! Fixes vs the original `cmtypage_scraper`:
//! - Early-3301 pages have an **empty `<h3>` title element**; the real headline
//!   is the first line of the body text (see `title_fallback`). Previously
//!   these were stored with `title: ""`.
//! - Date pages render **each article div twice** (verified live on
//!   `17-DEC-3301`: 8 `<div class="article">` blocks, 4 distinct uids with
//!   byte-identical content). Articles are deduplicated by uid per page.
//! - Pages with no article divs (e.g. `29-JUN-3301`) are recorded as *empty*,
//!   not as failures, so they don't pollute the failed list.
//! - The date is read from `div.i_right > p`, not the ambiguous `div > p`.

use crate::common::{GALNET_SITE, get_text_with_retry, title_fallback};

use regex::Regex;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, error::Error, sync::OnceLock};

#[derive(Debug)]
pub(crate) struct GalnetSiteArticle {
    pub(crate) uid: String,
    pub(crate) title: String,
    pub(crate) date: String,
    pub(crate) content: String,
    pub(crate) page_url: String,
    pub(crate) index_in_page: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PageParse {
    pub(crate) articles: Vec<GalnetSiteArticle>,
    pub(crate) dupes_collapsed: usize,
    pub(crate) skipped: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ErroredPage {
    pub(crate) url: String,
    pub(crate) errors: Vec<String>,
}

pub(crate) enum PageOutcome {
    Ok(PageParse),
    Empty,
    Failed(String),
}

pub(crate) struct SiteFetch {
    pub(crate) articles: Vec<GalnetSiteArticle>,
    pub(crate) ok_pages: Vec<String>,
    pub(crate) empty_pages: Vec<String>,
    pub(crate) failed_pages: Vec<ErroredPage>,
    pub(crate) dupes_collapsed: usize,
    pub(crate) skipped_blocks: usize,
}

struct Sels {
    article: Selector,
    link: Selector,
    title: Selector,
    date: Selector,
    date_fallback: Selector,
    body: Selector,
    uid_matcher: Regex,
}

fn sels() -> &'static Sels {
    static SELS: OnceLock<Sels> = OnceLock::new();
    SELS.get_or_init(|| Sels {
        article: Selector::parse(".article").expect("Article selector"),
        link: Selector::parse("a").expect("Link selector"),
        title: Selector::parse("h3").expect("Article title selector"),
        date: Selector::parse("div.i_right > p").expect("Article date selector"),
        date_fallback: Selector::parse("div > p").expect("Article date fallback selector"),
        // Live markup puts the body in a bare `> p` child of the article div.
        // (`ElementRef::select` on a descendant selector matches within the
        // subtree; on the verified 17-DEC-3301 page there is exactly one such
        // `p` per block besides the dated `div.i_right > p`.)
        body: Selector::parse(":scope > p").expect("Article content selector"),
        uid_matcher: Regex::new(r"/uid/([^/#?]+)").expect("URL UID matcher"),
    })
}

/// Text of an element excluding nested elements (e.g. `<h3>` title text
/// without the `<a>` link text — which matters when the title is empty).
fn element_own_text(element: &ElementRef) -> String {
    let mut out = String::new();
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(&text.text);
        }
    }
    out.trim().to_owned()
}

/// Body text: `<br>` tags are line breaks, other markup is dropped.
fn paragraph_text(paragraph: &ElementRef) -> String {
    let mut out = String::new();
    for child in paragraph.children() {
        let value = child.value();
        if let Some(text) = value.as_text() {
            out.push_str(&text.text);
        } else if let Some(element) = value.as_element() {
            if element.name() == "br" {
                out.push('\n');
            } else if let Some(inner) = ElementRef::wrap(child) {
                for piece in inner.text() {
                    out.push_str(piece);
                }
            }
        }
    }
    // Collapse blank separator lines from `<br /><br />`.
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !lines.is_empty() && !lines[lines.len() - 1].is_empty() {
                lines.push("");
            }
        } else {
            lines.push(trimmed);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Parse one date page. Never fails: unparseable blocks are counted in
/// `skipped`, page-render duplicates in `dupes_collapsed`.
pub(crate) fn parse_date_page(html: &str, page_url: &str) -> PageParse {
    let s = sels();
    let document = Html::parse_document(html);
    let mut articles = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut dupes_collapsed = 0usize;
    let mut skipped = 0usize;

    for block in document.select(&s.article) {
        // `h3` holds the title link; `div.i_right > p` the date; the bare
        // `> p` child the body. Queried directly (not via fuzzy `div > p`),
        // so nested date divs can't leak into the body.
        let link = block.select(&s.link).next();
        let uid = link
            .and_then(|a| a.value().attr("href"))
            .and_then(|href| s.uid_matcher.captures(href).map(|cap| cap[1].to_owned()));
        let Some(uid) = uid else {
            skipped += 1;
            continue;
        };

        let h3 = block.select(&s.title).next();
        let raw_title = h3.map(|h| element_own_text(&h)).unwrap_or_default();

        let date = block
            .select(&s.date)
            .next()
            .or_else(|| block.select(&s.date_fallback).next())
            .map(|d| d.text().collect::<String>().trim().to_owned())
            .unwrap_or_default();
        if date.is_empty() {
            skipped += 1;
            continue;
        }

        let content = block
            .select(&s.body)
            .next()
            .map(|p| paragraph_text(&p))
            .unwrap_or_default();
        if content.is_empty() {
            skipped += 1;
            continue;
        }

        if !seen.insert(uid.clone()) {
            dupes_collapsed += 1;
            continue;
        }

        let title = if raw_title.is_empty() {
            title_fallback(&content)
        } else {
            raw_title
        };
        // Deduped position on the page (dupes collapsed above), not the raw
        // block index — so `pageIndex` has no gaps from duplicate divs.
        let index_in_page = articles.len();
        articles.push(GalnetSiteArticle {
            uid,
            title,
            date,
            content,
            page_url: page_url.to_owned(),
            index_in_page,
        });
    }

    PageParse {
        articles,
        dupes_collapsed,
        skipped,
    }
}

async fn fetch_text(client: &Client, url: &str) -> Result<String, Box<dyn Error>> {
    get_text_with_retry(client, url).await
}

/// All date-page links from the homepage. The "MORE" button only toggles CSS
/// visibility client-side; every link is present in the served HTML.
pub(crate) async fn discover_pages(client: &Client) -> Result<Vec<String>, Box<dyn Error>> {
    let html = fetch_text(client, GALNET_SITE).await?;
    let document = Html::parse_document(&html);
    let link_selector = Selector::parse("a.galnetLinkBoxLink").expect("GalNet link selector");
    let mut links: HashSet<String> = HashSet::new();
    for element in document.select(&link_selector) {
        if let Some(href) = element.value().attr("href") {
            links.insert(format!("{}{}", GALNET_SITE, href.trim()));
        }
    }
    let mut links: Vec<String> = links.into_iter().collect();
    links.sort();
    Ok(links)
}

async fn fetch_one(client: Client, url: String) -> (String, PageOutcome) {
    let outcome = match fetch_text(&client, &url).await {
        Ok(html) => {
            let parsed = parse_date_page(&html, &url);
            if parsed.articles.is_empty() {
                PageOutcome::Empty
            } else {
                PageOutcome::Ok(parsed)
            }
        }
        Err(e) => PageOutcome::Failed(e.to_string()),
    };
    (url, outcome)
}

/// Fetch date pages sequentially (the site is not a CDN — stay polite).
/// Order of the returned articles is deterministic (page URL, then on-page).
pub(crate) async fn fetch_pages(client: &Client, pages: &[String]) -> SiteFetch {
    let mut outcomes: Vec<(String, PageOutcome)> = Vec::with_capacity(pages.len());
    for url in pages {
        outcomes.push(fetch_one(client.clone(), url.clone()).await);
    }
    outcomes.sort_by(|a, b| a.0.cmp(&b.0));

    let mut fetch = SiteFetch {
        articles: Vec::new(),
        ok_pages: Vec::new(),
        empty_pages: Vec::new(),
        failed_pages: Vec::new(),
        dupes_collapsed: 0,
        skipped_blocks: 0,
    };
    for (url, outcome) in outcomes {
        match outcome {
            PageOutcome::Ok(parsed) => {
                fetch.ok_pages.push(url);
                fetch.dupes_collapsed += parsed.dupes_collapsed;
                fetch.skipped_blocks += parsed.skipped;
                fetch.articles.extend(parsed.articles);
            }
            PageOutcome::Empty => fetch.empty_pages.push(url),
            PageOutcome::Failed(error) => fetch.failed_pages.push(ErroredPage {
                url,
                errors: vec![error],
            }),
        }
    }
    // Deterministic: page order, then on-page order.
    fetch.articles.sort_by(|a, b| {
        a.page_url
            .cmp(&b.page_url)
            .then(a.index_in_page.cmp(&b.index_in_page))
    });
    fetch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(extra: &str) -> String {
        format!(
            r#"<html><body><section id="block-system-main"><div>  </div>{extra}</section></body></html>"#
        )
    }

    fn block(uid: &str, h3: &str, date: &str, body: &str) -> String {
        format!(
            r#"<div class="article"><h3 class="hiLite galnetNewsArticleTitle"><a href="/galnet/uid/{uid}"><i class="fa fa-globe"></i> {h3}</a></h3><div class="i_right" style="margin: 5px"><p class="small" style="color:#888;">{date}</p></div><p>{body}</p></div>"#
        )
    }

    #[test]
    fn empty_h3_falls_back_to_first_body_line() {
        let html = fixture(&block(
            "abc123",
            "",
            "07 JAN 3301",
            "Real Headline<br /><br />Body text here.",
        ));
        let parsed = parse_date_page(&html, "http://example/07-JAN-3301");
        assert_eq!(parsed.articles.len(), 1);
        assert_eq!(parsed.articles[0].title, "Real Headline");
        assert_eq!(parsed.articles[0].date, "07 JAN 3301");
    }

    #[test]
    fn duplicate_divs_collapse_to_one_article() {
        let b = block("dup1", "Same Title", "17 DEC 3301", "Same body.");
        let html = fixture(&format!("{b}{b}"));
        let parsed = parse_date_page(&html, "http://example/17-DEC-3301");
        assert_eq!(parsed.articles.len(), 1);
        assert_eq!(parsed.dupes_collapsed, 1);
    }

    #[test]
    fn blocks_without_uid_are_skipped() {
        let html = fixture(r#"<div class="article"><h3>No link</h3><p>Body</p></div>"#);
        let parsed = parse_date_page(&html, "http://example/x");
        assert!(parsed.articles.is_empty());
        assert_eq!(parsed.skipped, 1);
    }

    #[test]
    fn empty_page_parses_to_nothing() {
        let html = fixture("");
        let parsed = parse_date_page(&html, "http://example/empty");
        assert!(parsed.articles.is_empty());
    }
}
