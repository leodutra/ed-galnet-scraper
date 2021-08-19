#[macro_use]
extern crate lazy_static;

use std::error::Error;

mod common;
// mod cms_scraper;
mod cmtypage_scraper;

use cmtypage_scraper::extract_all_pages;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    extract_all_pages(true).await
}
