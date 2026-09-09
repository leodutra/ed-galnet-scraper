//! Terminal progress reporting for the fetch phases.
//!
//! Both indicators draw to stderr and hide automatically when output is not a
//! terminal, so the `println!` summaries on stdout stay clean for pipes. The
//! fetchers take a borrowed [`ProgressBar`]; this module is terminal glue
//! only, with no domain logic. Constructor failures surface as
//! [`TemplateError`] via `?` rather than panicking.

use indicatif::{ProgressBar, ProgressStyle, style::TemplateError};
use std::time::Duration;

/// Spinner for the zaonce_cms JSON:API walk. The collection paginates via
/// `links.next` with no total, so a determinate bar is impossible — the
/// message carries `page N · M articles` instead.
pub(crate) fn cms_spinner() -> Result<ProgressBar, TemplateError> {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(ProgressStyle::with_template(
        "{spinner:.green} cms.zaonce.net {msg} [{elapsed_precise}]",
    )?);
    spinner.set_message("connecting…");
    spinner.enable_steady_tick(Duration::from_millis(100));
    Ok(spinner)
}

/// Determinate bar for the galnet_site date-page fetch; `total` is the todo
/// count. The message carries the current page slug and its outcome.
pub(crate) fn site_bar(total: u64) -> Result<ProgressBar, TemplateError> {
    let bar = ProgressBar::new(total);
    bar.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}",
    )?);
    bar.set_message("starting");
    bar.enable_steady_tick(Duration::from_millis(100));
    Ok(bar)
}
