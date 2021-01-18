#[macro_use]
extern crate lazy_static;

use futures::future::join_all;

use scraper::{ElementRef, Html, Selector};

lazy_static! {
    static ref ARTICLE_TITLE_SELECTOR: Selector = Selector::parse("h3").unwrap();
    static ref ARTICLE_DATE_SELECTOR: Selector = Selector::parse("div > p").unwrap();
    static ref ARTICLE_URL_SELECTOR: Selector = Selector::parse("h3 > a").unwrap();
    static ref ARTICLE_CONTENT_SELECTOR: Selector = Selector::parse(":scope > p").unwrap();
}

const ELITE_DANGEROUS_COMMUNITY_SITE: &'static str = "https://community.elitedangerous.com";

#[derive(Debug)]
struct Article {
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

async fn extract_link_articles(url: &str) -> Result<Vec<Article>, Box<dyn std::error::Error>> {
    println!("Fetching link \"{}\"", url);
    let html = fetch_link(&url).await?;
    println!("Fetched link \"{}\"", url);
    Ok(extract_articles(&html))
}

fn extract_articles(html: &str) -> Vec<Article> {
    let document = Html::parse_document(html);
    let article_selector = Selector::parse(".article").unwrap();

    document
        .select(&article_selector)
        .map(|article| Article {
            title: get_element_text(&article.select(&ARTICLE_TITLE_SELECTOR).next().unwrap()),
            date: get_element_text(&article.select(&ARTICLE_DATE_SELECTOR).next().unwrap()),
            url: with_site_url(&get_element_url(
                &article.select(&ARTICLE_URL_SELECTOR).next().unwrap(),
            )),
            content: get_element_text(&article.select(&ARTICLE_CONTENT_SELECTOR).next().unwrap()),
        })
        .collect()
}

async fn extract_all() -> Result<Vec<Article>, Box<dyn std::error::Error>> {
    let html = fetch_link(ELITE_DANGEROUS_COMMUNITY_SITE).await?;
    let links = extract_date_links(&html);

    let extraction_results = join_all(links.iter().map(|link| extract_link_articles(&link))).await;
    let mut articles = vec![];
    for result in extraction_results {
        articles.append(&mut result?)
    }
    Ok(articles)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let articles = extract_all().await?;
    println!("{:#?}", articles);
    
    // let resp = fetch_link("https://gist.githubusercontent.com/leodutra/6ce7397e0b8c20eb16f8949263e511c7/raw/galnet.html").await?;
    // let links = extract_date_links(&resp);
    // println!("{:#?}", links);
    // println!("{:#?}", extract_date_articles(&resp));

    Ok(())
}
