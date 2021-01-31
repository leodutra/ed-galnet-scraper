#[macro_use]
extern crate lazy_static;

#[macro_use]
extern crate nanoid;

use std::{fs::OpenOptions};
use futures::future::join_all;
use serde::Serialize;
use std::error::Error;
use std::fmt;
use std::fs;
use regex::Regex;

use scraper::{ElementRef, Html, Selector};

lazy_static! {
    static ref ARTICLE_TITLE_SELECTOR: Selector = Selector::parse("h3").unwrap();
    static ref ARTICLE_DATE_SELECTOR: Selector = Selector::parse("div > p").unwrap();
    static ref ARTICLE_URL_SELECTOR: Selector = Selector::parse("h3 > a").unwrap();
    static ref ARTICLE_CONTENT_SELECTOR: Selector = Selector::parse(":scope > p").unwrap();

    static ref URL_UID_MATCHER: Regex = Regex::new(r"/uid/([^/#?]+)").unwrap();
}

const ELITE_DANGEROUS_COMMUNITY_SITE: &'static str = "https://community.elitedangerous.com";
const EXTRACT_LOCATION: &'static str = "./galnet-files";

trait GalnetError {
    fn error_string(&self) -> String;
}

impl fmt::Display for dyn GalnetError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.error_string())
    }
}

impl fmt::Debug for dyn GalnetError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.error_string())
    }
}

#[derive(Debug)]
struct ScraperError {
    url: String,
    cause: Box<dyn Error>,
}

impl GalnetError for ScraperError {
    fn error_string(&self) -> String {
        format!("Error while scraping from \"{}\" {}", self.url, self.cause)
    }
}

impl ScraperError {
    fn from(url: String, error: Box<dyn Error>) -> Self {
        ScraperError { url, cause: error }
    }
}

#[derive(Debug)]
struct FileError {
    filename: String,
    cause: Box<dyn Error>,
}

impl GalnetError for FileError {
    fn error_string(&self) -> String {
        format!(
            "Error while scraping from \"{}\" {}",
            self.filename, self.cause
        )
    }
}

impl FileError {
    fn from(filename: String, error: Box<dyn Error>) -> Self {
        FileError {
            filename,
            cause: error,
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

fn with_site_url(url: &str) -> String {
    return String::from(ELITE_DANGEROUS_COMMUNITY_SITE) + url;
}

async fn fetch_link(link: &str) -> Result<String, Box<dyn std::error::Error>> {
    let resp = reqwest::get(link).await?.text().await?;
    Ok(resp)
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
    element_ref.value().attr("href").unwrap().to_owned()
}

fn extract_galnet_url_uid(url: &str) -> String {
    if let Some(cap) = URL_UID_MATCHER.captures(url) {
        cap[1].into()
    } else {
        nanoid!()
    }
}

fn extract_date_links(html: &str) -> Vec<String> {
    let fragment = Html::parse_document(html);
    let date_anchor_selector = Selector::parse("a.galnetLinkBoxLink").unwrap();
    fragment
        .select(&date_anchor_selector)
        .map(|element| element.value().attr("href"))
        .filter(|href| href.is_some())
        .map(|href| with_site_url(href.unwrap()))
        .collect()
}

async fn extract_page_articles(url: &str) -> Result<Vec<Article>, ScraperError> {
    match fetch_link(&url).await {
        Ok(html) => Ok(extract_articles(&html)),
        Err(e) => Err(ScraperError::from(url.into(), e)),
    }
}

fn extract_articles(html: &str) -> Vec<Article> {
    let document = Html::parse_document(html);
    let article_selector = Selector::parse(".article").unwrap();

    document
        .select(&article_selector)
        .map(|article| {
            let url = &get_element_url(
                &article.select(&ARTICLE_URL_SELECTOR).next().unwrap(),
            );
            Article {
                title: get_element_text(&article.select(&ARTICLE_TITLE_SELECTOR).next().unwrap()),
                date: get_element_text(&article.select(&ARTICLE_DATE_SELECTOR).next().unwrap()),
                url: with_site_url(url),
                uid: extract_galnet_url_uid(url), 
                content: get_element_text(&article.select(&ARTICLE_CONTENT_SELECTOR).next().unwrap()),
            }
        })
        .collect()
}

async fn extract_all(
) -> Result<(Vec<Article>, Vec<Box<dyn GalnetError>>), Box<dyn std::error::Error>> {
    let html = fetch_link(ELITE_DANGEROUS_COMMUNITY_SITE).await?;
    let links = extract_date_links(&html);

    let extraction_results = join_all(links.iter().map(|link| extract_page_articles(&link))).await;

    let mut articles = vec![];
    let mut errors: Vec<Box<dyn GalnetError>> = vec![];
    for result in extraction_results {
        match result {
            Ok(mut page_articles) => articles.append(&mut page_articles),
            Err(error) => errors.push(Box::new(error) as Box<dyn GalnetError>),
        }
    }

    fs::create_dir_all(EXTRACT_LOCATION)?;

    let mut file_errors = articles
        .iter()
        .map(|article| serialize_to_file(&gen_article_filename(article), article))
        .filter(|result| result.is_err())
        .map(|error_result| {
            Box::new(FileError::from("".to_owned(), error_result.unwrap_err()))
                as Box<dyn GalnetError>
        })
        .collect();

    errors.append(&mut file_errors);

    Ok((articles, errors))
}

fn gen_article_filename(article: &Article) -> String {
    let title = if article.title.trim().is_empty() {
        &article.uid
    } else {
        article.title.trim()
    };
    format!("{}/{} - {}.json", EXTRACT_LOCATION, article.date, title)
}

fn serialize_to_file(
    filename: &str,
    value: &impl Serialize,
) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::ser::to_writer(OpenOptions::new().write(true).truncate(true).create(true).open(filename)?, value)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (articles, failures) = extract_all().await?;
    println!("{:#?}", articles);
    println!("{:#?}", failures);

    println!("{}", extract_galnet_url_uid("/galnet/uid/5fdcdca955fd67154d2f1b54"));
    // let resp = fetch_link("https://gist.githubusercontent.com/leodutra/6ce7397e0b8c20eb16f8949263e511c7/raw/galnet.html").await?;
    // let links = extract_date_links(&resp);
    // println!("{:#?}", links);
    // println!("{:#?}", extract_date_articles(&resp));

    Ok(())
}
