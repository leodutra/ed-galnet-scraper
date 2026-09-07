mod common;
mod zaonce_api;

use std::error::Error;

use zaonce_api::extract_all_articles;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    extract_all_articles().await
}
