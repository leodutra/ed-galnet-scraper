#[macro_use]
extern crate lazy_static;

use fmt::Debug;
use futures::future::join_all;
use glob::glob;
use glob::Paths;
use regex::Regex;
use serde::Serialize;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::{collections::HashSet, error::Error};


use scraper::{ElementRef, Html, Selector};

lazy_static! {
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
        Regex::new(r"(\w+).json").expect("Filename UID matcher");
}

const ELITE_DANGEROUS_COMMUNITY_SITE: &'static str = "https://community.elitedangerous.com";
const EXTRACT_LOCATION: &'static str = "./galnet-files";

#[derive(Debug)]
enum GalnetError {
    FileError {
        filename: String,
        cause: Box<dyn Error>,
    },
    ParserError {
        cause: Box<dyn Error>,
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

impl<'a> From<cssparser::ParseError<'a, cssparser::BasicParseErrorKind<'a>>> for GalnetError {
    fn from(_: cssparser::ParseError<'a, cssparser::BasicParseErrorKind>) -> Self {
        todo!()
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

fn get_element_text(element_ref: &ElementRef) -> String {
    element_ref
        .text()
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_owned()
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

fn extract_date_links(html: &str) -> Result<Vec<String>, GalnetError> {
    let fragment = Html::parse_document(html);
    let date_anchor_selector = Selector::parse("a.galnetLinkBoxLink")
            .map_err(|e| ParserError { cause: e as Box<dyn Error> })?;
    Ok(fragment
        .select(&date_anchor_selector)
        .map(|element| element.value().attr("href"))
        .filter(|href| href.is_some())
        .map(|href| with_site_base_url(href.unwrap().trim()))
        .collect())
}

async fn extract_page_articles(
    url: &str,
    avoided_uids: HashSet<String>,
) -> Result<Vec<Article>, GalnetError> {
    match fetch_text(url).await {
        Ok(html) => match extract_articles(&html) {
            Ok(articles) => Ok(articles),
            Err(e) => Err(e),
        },
        Err(e) => Err(ScraperError {
            url: url.into(),
            cause: e,
        }),
    }
}

fn extract_articles(html: &str) -> Vec<Result<Article, GalnetError>> {
    Html::parse_document(html)
        .select(&ARTICLE_SELECTOR)
        .map(|article| {
            let select_in_article = |selector| &article.select(selector).next();
            let parser_error = |cause: &str| {
                Err(ParserError {
                    cause: cause.into(),
                })
            };

            let url = if let Some(url_el) = select_in_article(&ARTICLE_URL_SELECTOR) {
                with_site_base_url(&get_element_url(url_el))
            } else {
                return parser_error("coudn't find article url");
            };

            let uid = if let Some(uid) = extract_galnet_url_uid(&url) {
                uid
            } else {
                return parser_error("coudn't find article date");
            };

            let title = if let Some(title_el) = select_in_article(&ARTICLE_TITLE_SELECTOR) {
                get_element_text(title_el)
            } else {
                return parser_error("coudn't find article title");
            };

            let date = if let Some(date_el) = select_in_article(&ARTICLE_DATE_SELECTOR) {
                get_element_text(date_el)
            } else {
                return parser_error("coudn't find article date");
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

async fn extract_all() -> Result<(Vec<Article>, Vec<Box<GalnetError>>), Box<dyn Error>> {
    let html = fetch_text(ELITE_DANGEROUS_COMMUNITY_SITE).await?;
    let links: Vec<String> = extract_date_links(&html);
    let extraction_results = join_all(
        links
            .iter()
            .map(|link| extract_page_articles(link, downloaded_uids)),
    )
    .await;

    let mut articles = vec![];
    let mut errors: Vec<Box<GalnetError>> = vec![];
    for result in extraction_results {
        match result {
            Ok(mut page_articles) => articles.append(&mut page_articles),
            Err(error) => errors.push(Box::new(error) as Box<GalnetError>),
        }
    }

    fs::create_dir_all(EXTRACT_LOCATION)?;

    let mut file_errors = articles
        .iter()
        .map(|article| serialize_to_file(&gen_article_filename(article), article))
        .filter(|result| result.is_err())
        .map(|error_result| {
            Box::new(FileError {
                filename: "".to_owned(),
                cause: error_result.unwrap_err(),
            }) // as Box<GalnetError>
        })
        .collect();

    errors.append(&mut file_errors);

    Ok((articles, errors))
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
    let downloaded_articles = list_downloaded_articles(EXTRACT_LOCATION)?;
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
        EXTRACT_LOCATION, article.date, article.uid
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (articles, failures) = extract_all().await?;
    println!("{:#?}", articles);
    println!("{:#?}", failures);

    println!(
        "{}",
        extract_galnet_url_uid("/galnet/uid/5fdcdca955fd67154d2f1b54").unwrap()
    );
    // let resp = fetch_text("https://gist.githubusercontent.com/leodutra/6ce7397e0b8c20eb16f8949263e511c7/raw/galnet.html").await?;
    // let links = extract_date_links(&resp);
    // println!("{:#?}", links);
    // println!("{:#?}", extract_date_articles(&resp));

    Ok(())
}
