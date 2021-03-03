#[macro_use]
extern crate lazy_static;

use fmt::Debug;
use futures::future::join_all;

use regex::Regex;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
};
use std::{collections::HashSet, error::Error};
use std::{fmt, vec};

use scraper::{ElementRef, Html, Selector};

const ELITE_DANGEROUS_COMMUNITY_SITE: &'static str = "https://community.elitedangerous.com";

const EXTRACT_LOCATION: &'static str = "./galnet";

lazy_static! {
    // FILES
    static ref DOWNLOADED_PAGES_FILE: String = String::from(EXTRACT_LOCATION) + "/successful-pages.json";
    static ref FAILED_PAGES_FILE: String = String::from(EXTRACT_LOCATION) + "/failed-pages.json";
    static ref EXTRACTED_FILES_LOCATION: String = String::from(EXTRACT_LOCATION) + "/files";

    // PARSING
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

    // MATCHERS
    static ref ARTICLE_DATE_MATCHER: Regex =
        Regex::new(r"(\d{2})[\s-](\w{3})[\s-](\d{4,})").expect("Article date matcher");
    // static ref FILENAME_UID_MATCHER: Regex =
    //     Regex::new(r"[^-]+ - (\w+).json").expect("Filename UID matcher");
}

#[derive(Debug, Serialize, Hash, Eq)]
struct Article {
    uid: String,
    page_index: usize,
    title: String,
    date: String,
    url: String,
    content: String,
}

impl PartialEq for Article {
    fn eq(&self, other: &Self) -> bool {
        self.uid == other.uid
    }
}

#[derive(Default, Debug)]
struct PageExtraction {
    url: String,
    articles: HashSet<Article>,
    links: HashSet<String>,
    errors: Vec<GalnetError>,
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

impl Error for GalnetError {}

#[derive(Default, Debug, Hash, Eq)]
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

impl PartialEq for GalnetDate {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
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

fn with_site_base_url(url: &str) -> String {
    return ELITE_DANGEROUS_COMMUNITY_SITE.to_owned() + url;
}

async fn fetch_text(link: &str) -> Result<String, Box<dyn Error>> {
    Ok(reqwest::get(link).await?.text().await?)
}

fn extract_date_links(html: &str) -> HashSet<String> {
    Html::parse_document(html)
        .select(&GALNET_DATE_LINK_SELECTOR)
        .filter_map(|element| element.value().attr("href"))
        .map(|href| with_site_base_url(href.trim()))
        .collect()
}

async fn extract_page(url: &str) -> PageExtraction {
    let mut articles = HashSet::new();
    let mut links = HashSet::new();
    let mut errors = vec![];
    match fetch_text(&url).await {
        Ok(html) => {
            links = extract_date_links(&html);
            extract_articles(&html)
                .into_iter()
                .for_each(|result| match result {
                    Ok(article) => {
                        articles.insert(article);
                    }
                    Err(e) => {
                        errors.push(e);
                    }
                });
            if articles.len() == 0 {
                errors.push(ScraperError {
                    url: url.to_owned(),
                    cause: Box::new(ParserError {
                        cause: format!("Could not find any article for this page:\n{}", &html),
                    }),
                });
            }
        }
        Err(e) => {
            errors.push(ScraperError {
                url: url.to_owned(),
                cause: e,
            });
        }
    };
    PageExtraction {
        url: url.to_owned(),
        articles,
        links,
        errors,
        ..Default::default()
    }
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
    let get_element_url = |element_ref: &ElementRef| -> String {
        element_ref
            .value()
            .attr("href")
            .expect("Couldn't extract href attr")
            .to_owned()
    };
    let extract_galnet_url_uid =
        |url: &str| -> Option<String> { URL_UID_MATCHER.captures(url).map(|cap| cap[1].into()) };
    Html::parse_document(html)
        .select(&ARTICLE_SELECTOR)
        .enumerate()
        .map(|(page_index, article)| {
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

            let content = if let Some(content_el) = select_in_article(&ARTICLE_CONTENT_SELECTOR) {
                get_element_text(&content_el)
            } else {
                return parser_error(&format!("Couldn't find article \"{}\" content", uid));
            };

            Ok(Article {
                uid,
                page_index,
                title,
                date,
                url,
                content,
            })
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct ErroredPage {
    url: String,
    errors: Vec<String>,
}

async fn extract_all() -> Result<(), Box<dyn Error>> {
    let gen_article_filename = |article: &Article| -> String {
        format!(
            "{}/{} - {}-{}.json",
            EXTRACTED_FILES_LOCATION.clone(),
            revert_galnet_date(&article.date),
            article.page_index,
            article.uid
        )
    };

    let html = fetch_text(ELITE_DANGEROUS_COMMUNITY_SITE).await?;

    let mut failed_pages = HashMap::new();
    let mut downloaded_pages = list_downloaded_pages()?;
    println!(
        "Downloaded pages before starting: {}",
        downloaded_pages.len()
    );

    let extracted_links = extract_date_links(&html);
    println!("Extracted links: {}", extracted_links.len());

    // TODO page extraction carry links, add to this list and continue
    let links = extracted_links
        .difference(&downloaded_pages)
        .cloned()
        .collect::<HashSet<String>>();
    println!("Total number of links to extract: {}", links.len());

    let future_pages = links.iter().map(|link| extract_page(&link));
    let mut page_extractions = join_all(future_pages).await;

    fs::create_dir_all(EXTRACTED_FILES_LOCATION.clone())?;

    page_extractions.iter_mut().for_each(|page_extraction| {
        for article in &page_extraction.articles {
            let filename = gen_article_filename(article);
            if let Err(cause) = serialize_to_file(&filename, article) {
                page_extraction.errors.push(FileError { filename, cause });
            }
        }
        if page_extraction.errors.len() == 0 {
            let url = page_extraction.url.clone();
            failed_pages.remove(&url);
            downloaded_pages.insert(url);
        } else {
            let url = page_extraction.url.clone();
            failed_pages.insert(
                url.clone(),
                ErroredPage {
                    url,
                    errors: page_extraction
                        .errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect(),
                },
            );
        }
    });

    // DOWNLOADED
    let mut serializated_list = downloaded_pages.iter().collect::<Vec<_>>();
    serializated_list.sort();
    serialize_to_file(&DOWNLOADED_PAGES_FILE, &serializated_list)?;

    // FAILED
    serialize_to_file(&FAILED_PAGES_FILE, &failed_pages)?;

    Ok(())
}

// fn list_downloaded_articles(path: &str) -> Result<Paths, Box<dyn Error>> {
//     Ok(glob(&format!("{}/*.json", path))?)
// }

// fn extract_filename_uid(filename: &str) -> Option<String> {
//     FILENAME_UID_MATCHER
//         .captures(filename)
//         .map(|captures| captures[1].into())
// }

// fn downloaded_uids() -> Result<HashSet<String>, Box<dyn Error>> {
//     let downloaded_articles = list_downloaded_articles(&EXTRACTED_FILES_LOCATION)?;
//     let mut downloaded_uids = HashSet::new();
//     for entry in downloaded_articles {
//         entry?
//             .to_str()
//             .and_then(extract_filename_uid)
//             .map(|uid| downloaded_uids.insert(uid));
//     }
//     Ok(downloaded_uids)
// }

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

fn deserialize_from_file<T>(filename: &str) -> Result<Option<T>, Box<dyn Error>>
where
    T: DeserializeOwned,
{
    match OpenOptions::new().read(true).open(filename) {
        Ok(file) => Ok(Some(serde_json::de::from_reader(file)?)),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Box::new(e)),
    }
}

fn list_downloaded_pages() -> Result<HashSet<String>, Box<dyn Error>> {
    Ok(deserialize_from_file(&DOWNLOADED_PAGES_FILE)?.unwrap_or_default())
}

fn revert_galnet_date(date: &str) -> String {
    if let Some(cap) = ARTICLE_DATE_MATCHER.captures(date) {
        String::new() + &cap[3] + " " + &cap[2] + " " + &cap[1]
    } else {
        date.to_owned()
    }
}

// fn list_downloaded_files() -> Result<Paths, Box<dyn Error>> {
//     Ok(glob(&(EXTRACTED_FILES_LOCATION.clone() + "/*.json"))?)
// }

// fn list_downloaded_dates() -> Result<HashSet<GalnetDate>, Box<dyn Error>> {
//     let extract_date = |filename: String| -> Option<GalnetDate> {
//         if let Some(cap) = ARTICLE_DATE_MATCHER.captures(&filename) {
//             Some(GalnetDate {
//                 day: cap[1].to_string(),
//                 month: cap[2].to_string(),
//                 year: cap[3].to_string(),
//             })
//         } else {
//             None
//         }
//     };

//     let mut dates = HashSet::new();

//     for entry in list_downloaded_files()? {
//         if let Ok(path) = entry?.into_os_string().into_string() {
//             extract_date(path).map(|date| dates.insert(date));
//         }
//     }

//     Ok(dates)
// }

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    extract_all().await
    // list_downloaded_files().unwrap().for_each(|x| println!("{:?}", x.unwrap().display()));

    // println!("{}", revert_galnet_date("01 SET 3301"));
    // Ok(())

    // for entry in list_downloaded_dates()? {
    //     println!("{:?}", entry);
    // }

    // for entry in list_downloaded_files().unwrap() {
    //     match entry {
    //         Ok(path) => println!("{:?}", path.display()),
    //         Err(e) => println!("{:?}", e),
    //     }
    // }
    // Ok(())

    // let resp = fetch_text("https://gist.githubusercontent.com/leodutra/6ce7397e0b8c20eb16f8949263e511c7/raw/galnet.html").await?;
    // let links = extract_date_links(&resp);
    // println!("{:#?}", links);
    // println!("{:#?}", extract_date_articles(&resp));
}
