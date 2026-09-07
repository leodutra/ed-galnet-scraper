use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    error::Error,
    fmt::{self, Debug, Display, Formatter},
    fs::OpenOptions,
    hash::{Hash, Hasher},
};

pub(crate) const EXTRACTED_FILES_LOCATION: &str = "./galnet/files";
pub(crate) const SYNC_STATE_FILE: &str = "./galnet/zaonce-sync.json";

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
