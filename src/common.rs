use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    error::Error,
    fmt::{self, Debug, Display, Formatter},
    fs::OpenOptions,
    hash::{Hash, Hasher},
    sync::OnceLock,
};

use regex::Regex;

pub(crate) const USER_AGENT: &str = "ed-galnet-scraper/0.1.0";
pub(crate) const GALNET_SITE: &str = "https://community.elitedangerous.com";
pub(crate) const GALNET_SITE_UID_URL: &str = "https://community.elitedangerous.com/galnet/uid";
pub(crate) const EXTRACTED_FILES_LOCATION: &str = "./galnet/files";
pub(crate) const SYNC_STATE_FILE: &str = "./galnet/sync.json";
pub(crate) const DOWNLOADED_PAGES_FILE: &str = "./galnet/successful-pages.json";
pub(crate) const FAILED_PAGES_FILE: &str = "./galnet/failed-pages.json";
pub(crate) const EMPTY_PAGES_FILE: &str = "./galnet/empty-pages.json";

#[derive(Debug, Default, Serialize, Deserialize, Eq)]
pub(crate) struct Article {
    pub(crate) uid: String,

    #[serde(rename = "pageIndex")]
    pub(crate) page_index: usize,
    pub(crate) title: String,
    pub(crate) date: String,
    pub(crate) url: String,
    pub(crate) content: String,

    #[serde(rename = "extractionDate")]
    pub(crate) extraction_date: String,
    pub(crate) deprecated: bool,
}

impl Hash for Article {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.uid.hash(hasher);
    }
}

impl PartialEq for Article {
    fn eq(&self, other: &Self) -> bool {
        self.uid == other.uid
            && self.title == other.title
            && self.content == other.content
            && self.url == other.url
            && self.page_index == other.page_index
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SyncState {
    #[serde(rename = "extractionDate")]
    pub(crate) extraction_date: String,
    #[serde(rename = "articleCount")]
    pub(crate) article_count: usize,
    #[serde(rename = "newestPublishedAt")]
    pub(crate) newest_published_at: String,
    #[serde(rename = "newestUuid")]
    pub(crate) newest_uuid: String,
    #[serde(rename = "oldestPublishedAt")]
    pub(crate) oldest_published_at: String,
    #[serde(default, rename = "galnetSitePagesTotal")]
    pub(crate) galnet_site_pages_total: usize,
    #[serde(default, rename = "galnetSitePagesOk")]
    pub(crate) galnet_site_pages_ok: usize,
    #[serde(default, rename = "galnetSitePagesEmpty")]
    pub(crate) galnet_site_pages_empty: usize,
    #[serde(default, rename = "galnetSitePagesFailed")]
    pub(crate) galnet_site_pages_failed: usize,
    #[serde(default, rename = "galnetSiteArticles")]
    pub(crate) galnet_site_articles: usize,
    #[serde(default, rename = "matchedByUid")]
    pub(crate) matched_by_uid: usize,
    #[serde(default, rename = "matchedByText")]
    pub(crate) matched_by_text: usize,
    #[serde(default, rename = "siteOnly")]
    pub(crate) site_only: usize,
    #[serde(default, rename = "carriedFromDisk")]
    pub(crate) carried_from_disk: usize,
    #[serde(default, rename = "filesRemoved")]
    pub(crate) files_removed: usize,
}

#[derive(Debug)]
pub(crate) enum GalnetError {
    FileError {
        filename: String,
        cause: Box<dyn Error>,
    },
    ParserError {
        cause: String,
    },
    ScraperError {
        url: String,
        cause: Box<dyn Error>,
    },
}

impl Error for GalnetError {}

impl Display for GalnetError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            GalnetError::FileError { filename, cause } => {
                write!(f, "Error while scraping from \"{}\": {}", filename, cause)
            }
            GalnetError::ParserError { cause } => {
                write!(f, "Error while parsing: {}", cause)
            }
            GalnetError::ScraperError { url, cause } => {
                write!(f, "Error while scraping from \"{}\": {}", url, cause)
            }
        }
    }
}

pub(crate) fn serialize_to_file(
    filepath: &str,
    value: &impl Serialize,
) -> Result<(), Box<dyn Error>> {
    serde_json::ser::to_writer(
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(filepath)?,
        value,
    )?;
    Ok(())
}

pub(crate) fn deserialize_from_file<T>(filepath: &str) -> Result<Option<T>, Box<dyn Error>>
where
    T: DeserializeOwned,
{
    match OpenOptions::new().read(true).open(filepath) {
        Ok(file) => Ok(Some(serde_json::de::from_reader(file)?)),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Box::new(e)),
    }
}

pub(crate) fn read_string_list(filepath: &str) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(deserialize_from_file(filepath)?.unwrap_or_default())
}

pub(crate) fn read_string_set(filepath: &str) -> Result<HashSet<String>, Box<dyn Error>> {
    Ok(read_string_list(filepath)?.into_iter().collect())
}

/// Canonical form for comparing article text across sources: whitespace
/// differences (e.g. zaonce_cms `\r\n` vs galnet_site `<br />`) and case are ignored.
pub(crate) fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Early-3301 galnet_site pages have an empty `<h3>` title element; the real
/// headline is the first line of the body text.
pub(crate) fn title_fallback(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn date_matcher() -> &'static Regex {
    static MATCHER: OnceLock<Regex> = OnceLock::new();
    MATCHER.get_or_init(|| {
        Regex::new(r"(\d{2})[\s-](\w{3})[\s-](\d{4,})").expect("Article date matcher")
    })
}

/// `"03 SEP 3312"` -> `"3312 SEP 03"` so per-date files sort chronologically.
pub(crate) fn revert_galnet_date(date: &str) -> String {
    if let Some(cap) = date_matcher().captures(date) {
        format!("{} {} {}", &cap[3], &cap[2], &cap[1])
    } else {
        date.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_whitespace_and_case() {
        assert_eq!(
            normalize_text("Hello\r\n  World\nTest"),
            normalize_text("hello world test")
        );
    }

    #[test]
    fn fallback_uses_first_body_line() {
        assert_eq!(
            title_fallback("Latest News on Durius Situation\nIt seems many pilots..."),
            "Latest News on Durius Situation"
        );
        assert_eq!(title_fallback(""), "");
    }

    #[test]
    fn revert_orders_year_first() {
        assert_eq!(revert_galnet_date("03 SEP 3312"), "3312 SEP 03");
    }
}
