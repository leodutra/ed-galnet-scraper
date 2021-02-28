#[macro_use]
extern crate lazy_static;

use fmt::Debug;
use futures::future::join_all;
use glob::glob;
use glob::Paths;
use regex::Regex;
use serde::Serialize;
use std::{collections::HashMap, fs::ReadDir};
use std::fs::{self, OpenOptions};
use std::{collections::HashSet, error::Error};
use std::{fmt, vec};

use scraper::{ElementRef, Html, Selector};

const ELITE_DANGEROUS_COMMUNITY_SITE: &'static str = "https://community.elitedangerous.com";

const EXTRACT_LOCATION: &'static str = "./galnet";

lazy_static! {
    // FILES
    static ref FAILED_PAGES_FILE: String = String::from(EXTRACT_LOCATION) + "/failed-pages.json";
    static ref EXTRACTED_FILES_LOCATION: String = String::from(EXTRACT_LOCATION) + "/files";

    // EXTRACTION
    static ref GALNET_DATE_LINK_SELECTOR: Selector =
        Selector::parse("a.galnetLinkBoxLink").expect("GalNet link selector");
    static ref ARTICLE_SELECTOR: Selector = Selector::parse(".article").expect("Article selector");
    static ref ARTICLE_TITLE_SELECTOR: Selector =
        Selector::parse("h3").expect("Article title selector");
    static ref ARTICLE_DATE_SELECTOR: Selector =
        Selector::parse("div > p").expect("Article date selector");
    static ref ARTICLE_URL_SELECTOR: Selector =
        Selector::parse("h3 > a").expect("Article URL selector");
    static ref ARTICLE_CONTENT_SELECTOR: Selector =
        Selector::parse(":scope > p").expect("Article content selector");
    static ref URL_UID_MATCHER: Regex = Regex::new(r"/uid/([^/#?]+)").expect("URL UID matcher");
    static ref FILENAME_UID_MATCHER: Regex =
        Regex::new(r"[^-]+ - (\w+).json").expect("Filename UID matcher");
    static ref ARTICLE_DATE_MATCHER: Regex =
        Regex::new(r"(\d{2})\s(\w{3})\s(\d{4,})").expect("Article date matcher");
}

#[derive(Debug)]
enum GalnetError {
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

use GalnetError::{FileError, ParserError, ScraperError};

impl fmt::Display for GalnetError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FileError { filename, cause } => {
                write!(f, "Error while scraping from \"{}\": {}", filename, cause)
            }
            ParserError { cause } => {
                write!(f, "Error while parsing: {}", cause)
            }
            ScraperError { url, cause } => {
                write!(f, "Error while scraping from \"{}\": {}", url, cause)
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct Article {
    uid: String,
    title: String,
    date: String,
    url: String,
    content: String,
}

fn with_site_base_url(url: &str) -> String {
    return String::from(ELITE_DANGEROUS_COMMUNITY_SITE) + url;
}

async fn fetch_text(link: &str) -> Result<String, Box<dyn Error>> {
    Ok(reqwest::get(link).await?.text().await?)
}

fn get_element_url(element_ref: &ElementRef) -> String {
    element_ref
        .value()
        .attr("href")
        .expect("Couldn't extract href attr")
        .to_owned()
}

fn extract_galnet_url_uid(url: &str) -> Option<String> {
    URL_UID_MATCHER.captures(url).map(|cap| cap[1].into())
}

#[derive(Default, Debug)]
struct PageExtraction {
    url: String,
    articles: Vec<Article>,
    errors: Vec<GalnetError>,
}

async fn extract_page(url: &str) -> PageExtraction {
    let mut page = PageExtraction {
        url: url.to_owned(),
        ..Default::default()
    };
    match fetch_text(&page.url).await {
        Ok(html) => extract_articles(&html)
            .into_iter()
            .for_each(|result| match result {
                Ok(article) => page.articles.push(article),
                Err(e) => page.errors.push(e),
            }),
        Err(e) => {
            page.errors = vec![ScraperError {
                url: page.url.clone(),
                cause: e,
            }]
        }
    };
    page
}

fn extract_articles(html: &str) -> Vec<Result<Article, GalnetError>> {
    let parser_error = |cause: &str| {
        Err(ParserError {
            cause: cause.into(),
        })
    };
    let get_element_text = |element_ref: &ElementRef| -> String {
        element_ref
            .text()
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_owned()
    };
    Html::parse_document(html)
        .select(&ARTICLE_SELECTOR)
        .map(|article| {
            let select_in_article = |selector| article.select(selector).next();
            let url = if let Some(url_el) = select_in_article(&ARTICLE_URL_SELECTOR) {
                with_site_base_url(&get_element_url(&url_el))
            } else {
                return parser_error("Couldn't find article url");
            };

            let uid = if let Some(uid) = extract_galnet_url_uid(&url) {
                uid
            } else {
                return parser_error(&format!("Couldn't find article \"{}\" uid", url));
            };

            let title = if let Some(title_el) = select_in_article(&ARTICLE_TITLE_SELECTOR) {
                get_element_text(&title_el)
            } else {
                return parser_error(&format!("Couldn't find article \"{}\" title", uid));
            };

            let date = if let Some(date_el) = select_in_article(&ARTICLE_DATE_SELECTOR) {
                get_element_text(&date_el)
            } else {
                return parser_error(&format!("Couldn't find article \"{}\" date", uid));
            };

            Ok(Article {
                title,
                date,
                url,
                uid,
                content: get_element_text(
                    &article
                        .select(&ARTICLE_CONTENT_SELECTOR)
                        .next()
                        .expect("Scraped article content"),
                ),
            })
        })
        .collect()
}

async fn extract_all() -> Result<(), Box<dyn Error>> {
    let extract_date_links = |html: &str| -> Vec<String> {
        Html::parse_document(html)
            .select(&GALNET_DATE_LINK_SELECTOR)
            .map(|element| element.value().attr("href"))
            .filter(|href| href.is_some())
            .map(|href| with_site_base_url(href.unwrap().trim()))
            .collect()
    };

    let html = fetch_text(ELITE_DANGEROUS_COMMUNITY_SITE).await?;
    let links: Vec<String> = extract_date_links(&html);

    let future_pages = links.iter().map(|link| extract_page(&link));
    let mut page_extractions = join_all(future_pages).await;

    let mut all_failed_pages = vec![];

    fs::create_dir_all(EXTRACTED_FILES_LOCATION.clone())?;

    for page_extraction in &mut page_extractions {
        if page_extraction.errors.len() > 0 {
            all_failed_pages.push(page_extraction.url.clone());
        } else {
            for article in &page_extraction.articles {
                if let Err(cause) = serialize_to_file(&gen_article_filename(&article), &article) {
                    page_extraction.errors.push(FileError {
                        filename: gen_article_filename(&article),
                        cause,
                    });
                    all_failed_pages.push(page_extraction.url.clone())
                }
            }
        }
    }

    all_failed_pages.dedup();

    if all_failed_pages.len() > 0 {
        serialize_to_file(&FAILED_PAGES_FILE, &all_failed_pages)?;
    }

    Ok(())
}

fn list_downloaded_articles(path: &str) -> Result<Paths, Box<dyn Error>> {
    Ok(glob(&format!("{}/*.json", path))?)
}

fn extract_filename_uid(filename: &str) -> Option<String> {
    FILENAME_UID_MATCHER
        .captures(filename)
        .map(|captures| captures[1].into())
}

fn downloaded_uids() -> Result<HashSet<String>, Box<dyn Error>> {
    let downloaded_articles = list_downloaded_articles(&EXTRACTED_FILES_LOCATION)?;
    let mut downloaded_uids = HashSet::new();
    for entry in downloaded_articles {
        entry?
            .to_str()
            .and_then(extract_filename_uid)
            .map(|uid| downloaded_uids.insert(uid));
    }
    Ok(downloaded_uids)
}

fn gen_article_filename(article: &Article) -> String {
    format!(
        "{}/{} - {}.json",
        EXTRACTED_FILES_LOCATION.clone(),
        article.date,
        article.uid
    )
}

fn serialize_to_file(filename: &str, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    serde_json::ser::to_writer(
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(filename)?,
        value,
    )?;
    Ok(())
}

fn list_downloaded_files() -> Result<Paths, Box<dyn Error>> {
    Ok(glob(&(EXTRACTED_FILES_LOCATION.clone() + "/*.json"))?)
}

#[derive(Default, Debug)]
struct GalnetDate {
    day: String,
    month: String,
    year: String,
}

impl ToString for GalnetDate {
    fn to_string(&self) -> String {
        format!("{} {} {}", self.day, self.month, self.year)
    }
}

fn list_downloaded_dates() -> Result<HashMap<String, GalnetDate>, Box<dyn Error>> {
    let extract_date = |filename: String| -> Option<GalnetDate> {
        if let Some(cap) = ARTICLE_DATE_MATCHER.captures(&filename) {
            Some(GalnetDate {
                day: cap[1].to_string(),
                month: cap[2].to_string(),
                year: cap[3].to_string(),
            })
        } else {
            None
        }
    };

    let mut dates = HashMap::new();

    for entry in list_downloaded_files()? {
        if let Ok(path) = entry?.into_os_string().into_string() {
            extract_date(path).map(|date| dates.insert(date.to_string(), date));
        }
    }

    Ok(dates)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // extract_all().await
    // list_downloaded_files().unwrap().for_each(|x| println!("{:?}", x.unwrap().display()));

    list_downloaded_dates()?.iter().map(|x| println!("{:?}", x));

    // for entry in list_downloaded_files().unwrap() {
    //     match entry {
    //         Ok(path) => println!("{:?}", path.display()),
    //         Err(e) => println!("{:?}", e),
    //     }
    // }
    Ok(())

    // let resp = fetch_text("https://gist.githubusercontent.com/leodutra/6ce7397e0b8c20eb16f8949263e511c7/raw/galnet.html").await?;
    // let links = extract_date_links(&resp);
    // println!("{:#?}", links);
    // println!("{:#?}", extract_date_articles(&resp));
}
