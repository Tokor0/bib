//! Retrieving the document itself, where it is legitimately available.
//!
//! **An advertised PDF URL is very often not a PDF.** Checked against the live
//! services while planning this:
//!
//! | URL | Result |
//! |---|---|
//! | `arxiv.org/pdf/1706.03762` | `application/pdf`, starts `%PDF-` |
//! | OpenAlex `oa_url` for a PLOS paper (a `doi.org` link) | `text/html` |
//! | OpenAlex **`pdf_url`** for Einstein 1905 (Wiley `pdfdirect`) | HTTP **200**, `text/html` |
//!
//! The last is the one that matters: OpenAlex labels the field `pdf_url`, the
//! server answers 200, and the body is a consent interstitial. Trusting either
//! the field name or the status code means writing HTML into `paper.pdf` and
//! then failing identification in a way that looks like our own bug. So the
//! first bytes are read and the `%PDF-` magic is required before anything is
//! saved.
//!
//! **No attempt is made to get past a paywall or a bot interstitial** — no
//! browser user-agent spoofing, no scraping landing pages for hidden links. A
//! plain GET either yields a PDF or it does not, and the failure is reported.
//! In practice this succeeds for arXiv and genuine open-access repositories and
//! fails for much "bronze" access; that is the state of the world, not a defect
//! to engineer around.

use super::{Http, ProviderError};
use crate::identify::patterns::Identifier;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// PDF magic. Some files carry leading junk before it, which readers tolerate,
/// so it is searched for within the first block rather than required at offset
/// zero.
const PDF_MAGIC: &[u8] = b"%PDF-";

/// How much of the head to inspect before committing to a download.
const SNIFF_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfSource {
    pub url: String,
    /// Which mechanism suggested this URL, for `x-bib` and for diagnostics.
    pub origin: &'static str,
}

#[derive(Debug)]
pub enum FetchError {
    /// The response was not a PDF — overwhelmingly the common failure.
    NotAPdf {
        url: String,
        looked_like: String,
    },
    TooLarge {
        url: String,
        limit: u64,
    },
    Http {
        url: String,
        source: ProviderError,
    },
    Io(std::io::Error),
    /// Nothing to try.
    NoSource,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPdf { url, looked_like } => {
                write!(f, "{url} returned {looked_like}, not a PDF")
            }
            Self::TooLarge { url, limit } => {
                write!(f, "{url} is larger than the {limit} byte limit")
            }
            Self::Http { url, source } => write!(f, "{url}: {source}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::NoSource => write!(f, "no open-access location is known"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Where a PDF for this record might be, best first.
pub fn candidates(
    id: Option<&Identifier>,
    oa_url: Option<&str>,
    url: Option<&str>,
) -> Vec<PdfSource> {
    let mut out = Vec::new();

    // arXiv first: deterministic, always a real PDF, no interstitial.
    if let Some(Identifier::ArXiv(arxiv)) = id {
        out.push(PdfSource {
            url: format!("https://arxiv.org/pdf/{arxiv}"),
            origin: "arxiv",
        });
    }
    if let Some(oa_url) = oa_url.filter(|u| !u.trim().is_empty()) {
        out.push(PdfSource {
            url: oa_url.to_owned(),
            origin: "openalex",
        });
    }
    // The record's own URL is a long shot — usually a landing page — so it is
    // tried last rather than not at all.
    if let Some(url) = url.filter(|u| !u.trim().is_empty())
        && !out.iter().any(|s| s.url == *url)
    {
        out.push(PdfSource {
            url: url.to_owned(),
            origin: "url",
        });
    }
    out
}

/// Try each source in turn; the first that yields a real PDF wins.
///
/// Returns the source that worked, so it can be recorded in `x-bib` and a later
/// re-fetch can be traced.
pub fn download_first(
    http: &Http,
    sources: &[PdfSource],
    dest: &Path,
    max_bytes: u64,
    timeout: Duration,
) -> Result<PdfSource, Vec<FetchError>> {
    if sources.is_empty() {
        return Err(vec![FetchError::NoSource]);
    }
    let mut failures = Vec::new();
    for source in sources {
        match download(http, &source.url, dest, max_bytes, timeout) {
            Ok(()) => return Ok(source.clone()),
            Err(e) => failures.push(e),
        }
    }
    Err(failures)
}

/// Download one URL to `dest`, verifying it really is a PDF.
pub fn download(
    http: &Http,
    url: &str,
    dest: &Path,
    max_bytes: u64,
    timeout: Duration,
) -> Result<(), FetchError> {
    let mut reader = http
        .get_reader(url, "application/pdf", timeout)
        .map_err(|source| FetchError::Http {
            url: url.to_owned(),
            source,
        })?;

    // Sniff before committing: an interstitial is HTML with a 200 status and
    // frequently `content-type: application/pdf`, so only the bytes decide.
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut filled = 0;
    while filled < head.len() {
        match reader.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(FetchError::Io(e)),
        }
    }
    head.truncate(filled);

    if !head.windows(PDF_MAGIC.len()).any(|w| w == PDF_MAGIC) {
        return Err(FetchError::NotAPdf {
            url: url.to_owned(),
            looked_like: describe(&head),
        });
    }

    // Written to a temporary beside the destination and renamed, so an
    // interrupted download cannot leave a half-file that looks like an
    // attachment.
    let parent = dest.parent().unwrap_or(Path::new("."));
    let temp = tempfile::NamedTempFile::new_in(parent).map_err(FetchError::Io)?;
    let mut file = std::io::BufWriter::new(temp.reopen().map_err(FetchError::Io)?);
    std::io::Write::write_all(&mut file, &head).map_err(FetchError::Io)?;

    let mut written = head.len() as u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(FetchError::Io)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > max_bytes {
            // Refused mid-stream rather than after: the point of a cap is not
            // to fill the disk first and complain afterwards.
            return Err(FetchError::TooLarge {
                url: url.to_owned(),
                limit: max_bytes,
            });
        }
        std::io::Write::write_all(&mut file, &buffer[..n]).map_err(FetchError::Io)?;
    }
    std::io::Write::flush(&mut file).map_err(FetchError::Io)?;
    drop(file);

    temp.persist(dest).map_err(|e| FetchError::Io(e.error))?;
    Ok(())
}

/// A short description of what arrived instead of a PDF.
fn describe(head: &[u8]) -> String {
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start();
    if trimmed.len() >= 5 && trimmed[..5.min(trimmed.len())].eq_ignore_ascii_case("<html")
        || trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
    {
        return "an HTML page".to_owned();
    }
    if head.is_empty() {
        return "an empty response".to_owned();
    }
    format!("{} bytes of something else", head.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arxiv_is_tried_before_anything_else() {
        let sources = candidates(
            Some(&Identifier::ArXiv("1706.03762".into())),
            Some("https://example.org/oa.pdf"),
            Some("https://example.org/landing"),
        );
        assert_eq!(sources[0].origin, "arxiv");
        assert_eq!(sources[0].url, "https://arxiv.org/pdf/1706.03762");
        assert_eq!(sources.len(), 3);
    }

    #[test]
    fn a_duplicate_url_is_not_tried_twice() {
        let sources = candidates(
            None,
            Some("https://example.org/x.pdf"),
            Some("https://example.org/x.pdf"),
        );
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn nothing_known_yields_no_sources() {
        assert!(candidates(None, None, None).is_empty());
        assert!(candidates(None, Some("  "), None).is_empty());
    }

    #[test]
    fn html_is_described_as_html() {
        assert_eq!(describe(b"<!DOCTYPE html><html>"), "an HTML page");
        assert_eq!(describe(b"  <html lang=\"en\">"), "an HTML page");
        assert_eq!(describe(b""), "an empty response");
    }
}
