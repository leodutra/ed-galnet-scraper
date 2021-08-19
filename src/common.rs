use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashSet,
    error::Error,
    fmt::{self, Debug, Display, Formatter},
    fs::OpenOptions,
    hash::{Hash, Hasher},
};

pub(crate) const EXTRACT_LOCATION: &str = "./galnet";

lazy_static! {
    // FILES
    pub(crate) static ref DOWNLOADED_PAGES_FILE: String = String::from(EXTRACT_LOCATION) + "/successful-pages.json";
    pub(crate) static ref FAILED_PAGES_FILE: String = String::from(EXTRACT_LOCATION) + "/failed-pages.json";
    pub(crate) static ref EXTRACTED_FILES_LOCATION: String = String::from(EXTRACT_LOCATION) + "/files";
}

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

#[derive(Default, Debug, Eq)]
pub(crate) struct GalnetDate {
    pub(crate) day: String,
    pub(crate) month: String,
    pub(crate) year: String,
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

pub(crate) fn list_downloaded_pages() -> Result<HashSet<String>, Box<dyn Error>> {
    Ok(deserialize_from_file(&DOWNLOADED_PAGES_FILE)?.unwrap_or_default())
}
