use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt::{self, Debug, Display, Formatter},
    fs::OpenOptions,
    time::Duration,
};

pub(crate) const USER_AGENT: &str = "ed-galnet-scraper/0.1.0";
pub(crate) const GALNET_SITE: &str = "https://community.elitedangerous.com";
pub(crate) const GALNET_SITE_UID_URL: &str = "https://community.elitedangerous.com/galnet/uid";
pub(crate) const EXTRACTED_FILES_LOCATION: &str = "./galnet/files";
pub(crate) const SYNC_STATE_FILE: &str = "./galnet/sync.json";
pub(crate) const DOWNLOADED_PAGES_FILE: &str = "./galnet/successful-pages.json";
pub(crate) const FAILED_PAGES_FILE: &str = "./galnet/failed-pages.json";
pub(crate) const EMPTY_PAGES_FILE: &str = "./galnet/empty-pages.json";
pub(crate) const ALIASES_FILE: &str = "./galnet/aliases.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

impl PartialEq for Article {
    fn eq(&self, other: &Self) -> bool {
        // Identity + content fields. `url` derives from `uid`; `extraction_date`
        // and `deprecated` are sync bookkeeping, deliberately excluded so
        // re-runs don't rewrite every file.
        self.uid == other.uid
            && self.page_index == other.page_index
            && self.title == other.title
            && self.date == other.date
            && self.content == other.content
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
#[allow(clippy::enum_variant_names)]
pub(crate) enum GalnetError {
    FileError {
        filename: String,
        cause: Box<dyn Error>,
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
                write!(f, "Error with file \"{}\": {}", filename, cause)
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

pub(crate) fn read_string_set(filepath: &str) -> Result<HashSet<String>, Box<dyn Error>> {
    Ok(deserialize_from_file(filepath)?.unwrap_or_default())
}

/// Canonical form for comparing article text across sources: whitespace
/// differences (e.g. zaonce_cms `\r\n` vs galnet_site `<br />`) and case are ignored.
pub(crate) fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Trim every line and the ends. Both sources leave trailing spaces that are
/// invisible in the rendered text but make the same article differ byte-wise
/// depending on which source filed it.
pub(crate) fn trim_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
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

/// `"03 SEP 3312"` -> `"3312-09-03"` so files group by year first and sort
/// chronologically within it. Anything that doesn't match the fixed shape
/// (or names an unknown month) passes through verbatim, so a bad date never
/// silently files under a wrong name.
pub(crate) fn galnet_date_to_iso(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    // Fixed shape: 2 digits, space, 3 uppercase letters, space, 4 digits.
    let mut parts = date.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(day), Some(mon), Some(year), None)
            if day.len() == 2
                && day.bytes().all(|b| b.is_ascii_digit())
                && mon.len() == 3
                && mon.bytes().all(|b| b.is_ascii_uppercase())
                && year.len() == 4
                && year.bytes().all(|b| b.is_ascii_digit()) =>
        {
            match MONTHS.iter().position(|m| *m == mon) {
                Some(index) => format!("{year}-{:02}-{day}", index + 1),
                None => date.to_owned(),
            }
        }
        _ => date.to_owned(),
    }
}

/// Drupal `body.value` is usually plain text but a few articles are wrapped in
/// a single `<p>...</p>` (e.g. 25 APR 3308, 15 DEC 3308, 28 JUL 3309). Unwrap
/// exactly that shape; anything else is kept verbatim.
pub(crate) fn strip_paragraph_wrapper(text: &str) -> String {
    // One pass only: the strip fires only when the inner text has no `<`,
    // so the result can never match the `<p>...</p>` shape again.
    let trimmed = text.trim();
    if trimmed.len() >= 7
        && trimmed.starts_with("<p>")
        && trimmed.ends_with("</p>")
        && trimmed[3..trimmed.len() - 4].find('<').is_none()
    {
        let inner = trimmed[3..trimmed.len() - 4].trim_end_matches('\n');
        format!("{inner}\n")
    } else {
        text.to_owned()
    }
}

/// One full scan of `galnet/files`: uid -> article, plus the path set.
/// Replaces the five ad-hoc `read_dir` + deserialize loops.
pub(crate) struct DiskScan {
    /// uid -> article; first file wins per uid.
    pub(crate) by_uid: HashMap<String, Article>,
    /// uid -> all paths holding it (duplicates included).
    pub(crate) paths_by_uid: HashMap<String, Vec<String>>,
    /// path -> parsed article (unparseable files absent).
    pub(crate) by_path: HashMap<String, Article>,
    pub(crate) paths: HashSet<String>,
}

pub(crate) fn scan_disk() -> DiskScan {
    use std::fs;

    let mut by_uid: HashMap<String, Article> = HashMap::new();
    let mut paths_by_uid: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_path: HashMap<String, Article> = HashMap::new();
    let mut paths: HashSet<String> = HashSet::new();
    if let Ok(entries) = fs::read_dir(EXTRACTED_FILES_LOCATION) {
        let mut files: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    return None;
                }
                p.to_str().map(str::to_owned)
            })
            .collect();
        files.sort();
        for path in files {
            paths.insert(path.clone());
            if let Ok(Some(article)) = deserialize_from_file::<Article>(&path) {
                paths_by_uid
                    .entry(article.uid.clone())
                    .or_default()
                    .push(path.clone());
                by_path.insert(path.clone(), article.clone());
                by_uid.entry(article.uid.clone()).or_insert(article);
            }
        }
    }
    DiskScan {
        by_uid,
        paths_by_uid,
        by_path,
        paths,
    }
}

pub(crate) const MAX_FETCH_ATTEMPTS: u32 = 3;

/// GET with `error_for_status` + linear backoff. Shared by both fetchers.
pub(crate) async fn get_text_with_retry(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, Box<dyn Error>> {
    let mut last_error: Option<Box<dyn Error>> = None;
    for attempt in 1..=MAX_FETCH_ATTEMPTS {
        match client.get(url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.text().await {
                    Ok(text) => return Ok(text),
                    Err(e) => last_error = Some(Box::new(e)),
                },
                Err(e) => last_error = Some(Box::new(e)),
            },
            Err(e) => last_error = Some(Box::new(e)),
        }
        if attempt < MAX_FETCH_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
        }
    }
    Err(Box::new(GalnetError::ScraperError {
        url: url.to_owned(),
        cause: last_error.expect("get_text_with_retry must have an error after retries"),
    }))
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
    fn trim_lines_strips_line_and_string_ends() {
        assert_eq!(trim_lines("  a \n b  \n\nc \n "), "a\nb\n\nc");
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
    fn iso_orders_year_first() {
        assert_eq!(galnet_date_to_iso("03 SEP 3312"), "3312-09-03");
    }

    #[test]
    fn iso_covers_all_twelve_months() {
        let cases = [
            ("01 JAN 3301", "3301-01-01"),
            ("01 FEB 3301", "3301-02-01"),
            ("01 MAR 3301", "3301-03-01"),
            ("01 APR 3301", "3301-04-01"),
            ("01 MAY 3301", "3301-05-01"),
            ("01 JUN 3301", "3301-06-01"),
            ("01 JUL 3301", "3301-07-01"),
            ("01 AUG 3301", "3301-08-01"),
            ("01 SEP 3301", "3301-09-01"),
            ("01 OCT 3301", "3301-10-01"),
            ("01 NOV 3301", "3301-11-01"),
            ("01 DEC 3301", "3301-12-01"),
        ];
        for (input, expected) in cases {
            assert_eq!(galnet_date_to_iso(input), expected, "input: {input}");
        }
    }

    #[test]
    fn iso_rejects_slugs_lowercase_and_unknown_months() {
        // Page slugs ("03-SEP-3312"), lowercase months and unknown
        // 3-uppercase-letter months must pass through untouched so a bad
        // date never silently files under a wrong name.
        assert_eq!(galnet_date_to_iso("03-SEP-3312"), "03-SEP-3312");
        assert_eq!(galnet_date_to_iso("03 Sep 3312"), "03 Sep 3312");
        assert_eq!(galnet_date_to_iso("03 XXX 3312"), "03 XXX 3312");
        assert_eq!(galnet_date_to_iso("not a date"), "not a date");
        // Wrong shapes the old `^(\d{2}) ([A-Z]{3}) (\d{4})$` regex rejected.
        assert_eq!(galnet_date_to_iso("3 SEP 3312"), "3 SEP 3312");
        assert_eq!(galnet_date_to_iso("03  SEP 3312"), "03  SEP 3312");
        assert_eq!(galnet_date_to_iso("03 SEP 3312 "), "03 SEP 3312 ");
        assert_eq!(galnet_date_to_iso("03 SEPT 3312"), "03 SEPT 3312");
    }

    #[test]
    fn strip_unwraps_single_paragraph() {
        assert_eq!(
            strip_paragraph_wrapper("<p>Body text here.</p>\n"),
            "Body text here.\n"
        );
        assert_eq!(
            strip_paragraph_wrapper("  <p>Body text here.</p>  "),
            "Body text here.\n"
        );
    }

    #[test]
    fn strip_leaves_plain_text_alone() {
        let plain = "Line one\nLine two with < comparison and 3 > 2.\n";
        assert_eq!(strip_paragraph_wrapper(plain), plain);
    }

    #[test]
    fn strip_leaves_multi_tag_html_alone() {
        let html = "<p>One</p><p>Two</p>";
        assert_eq!(strip_paragraph_wrapper(html), html);
    }

    #[test]
    fn article_eq_ignores_bookkeeping_but_not_date() {
        let base = Article {
            uid: "u".to_owned(),
            page_index: 0,
            title: "T".to_owned(),
            date: "01 JAN 3301".to_owned(),
            url: "http://x/u".to_owned(),
            content: "B".to_owned(),
            extraction_date: "2021-01-01T00:00:00Z".to_owned(),
            deprecated: false,
        };
        let mut rerun = Article {
            uid: "u".to_owned(),
            page_index: 0,
            title: "T".to_owned(),
            date: "01 JAN 3301".to_owned(),
            url: "http://x/u".to_owned(),
            content: "B".to_owned(),
            extraction_date: "2026-01-01T00:00:00Z".to_owned(),
            deprecated: false,
        };
        assert_eq!(base, rerun);
        rerun.date = "02 JAN 3301".to_owned();
        assert_ne!(base, rerun);
    }
}
