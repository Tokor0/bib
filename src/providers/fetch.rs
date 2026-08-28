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
//!
//! What *is* worth doing is asking for the right URL in the first place. A
//! record's stored URL is nearly always a **landing page** — `arxiv.org/abs/…`
//! rather than `arxiv.org/pdf/…` — because that is the link publishers and
//! importers record. Rewriting the handful of landing-page shapes whose PDF
//! location is mechanical ([`pdf_form`]) is the difference between "no PDF
//! available" and the file being one request away, and it is not scraping: no
//! page is read to find the link, the URL is derived from the identifier the
//! record already holds.

use super::{Http, ProviderError};
use crate::identify::patterns::arxiv_in_url;
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
///
/// `arxiv` is the ID from the record's serial numbers, its URL or its note —
/// wherever it was recorded — and is passed separately from the DOI on purpose:
/// a published paper with a preprint has both, the DOI resolves to a publisher
/// landing page, and the arXiv copy is the one that is actually a PDF.
pub fn candidates(arxiv: Option<&str>, oa_url: Option<&str>, url: Option<&str>) -> Vec<PdfSource> {
    let mut out: Vec<PdfSource> = Vec::new();
    let mut push = |url: String, origin: &'static str| {
        if !url.trim().is_empty() && !out.iter().any(|s| s.url == url) {
            out.push(PdfSource { url, origin });
        }
    };

    // arXiv first: deterministic, always a real PDF, no interstitial.
    if let Some(arxiv) = arxiv.filter(|a| !a.trim().is_empty()) {
        push(format!("https://arxiv.org/pdf/{arxiv}"), "arxiv");
    }
    if let Some(oa_url) = oa_url {
        push(oa_url.to_owned(), "openalex");
    }
    // The record's own URL is a long shot — usually a landing page — so it is
    // tried last rather than not at all. Where that page's PDF sits at a known
    // address, the derived URL is tried first and the page itself kept as the
    // fallback: the rewrite is a guess, and a guess must not remove an option.
    if let Some(url) = url {
        if let Some(derived) = pdf_form(url) {
            push(derived, "url");
        }
        push(url.to_owned(), "url");
    }
    out
}

/// The PDF address for a landing page whose layout is mechanical.
///
/// Deliberately a short list. Each entry is a documented, stable URL scheme
/// where the file location follows from the page location with no lookup —
/// anything requiring the page to be parsed belongs to the "no scraping" rule
/// above, not here. A wrong guess costs nothing: the download sniffs for `%PDF-`
/// and the original URL is still tried.
fn pdf_form(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // arxiv.org/abs/ID -> arxiv.org/pdf/ID.
    if let Some(id) = arxiv_in_url(trimmed) {
        return Some(format!("https://arxiv.org/pdf/{id}"));
    }
    // openreview.net/forum?id=X -> openreview.net/pdf?id=X.
    if let Some(rest) = after_host(trimmed, "openreview.net")
        && let Some(id) = rest.strip_prefix("/forum?id=")
    {
        return Some(format!("https://openreview.net/pdf?id={id}"));
    }
    // bioRxiv and medRxiv append `.full.pdf` to the article path.
    for host in ["biorxiv.org", "medrxiv.org"] {
        if let Some(rest) = after_host(trimmed, host)
            && rest.starts_with("/content/")
            && !rest.ends_with(".pdf")
        {
            return Some(format!(
                "https://www.{host}{}.full.pdf",
                rest.trim_end_matches('/')
            ));
        }
    }
    None
}

/// The path of `url` when it is served by `host`, ignoring scheme and `www.`.
fn after_host<'a>(url: &'a str, host: &str) -> Option<&'a str> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let rest = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    rest.strip_prefix(host).filter(|r| r.starts_with('/'))
}

/// The bounds one download runs under.
///
/// Grouped rather than passed as three positional arguments: two of them are
/// `Duration`s meaning different things, and a call site that swaps a timeout
/// for a rate limit would compile.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Refused mid-stream once exceeded.
    pub max_bytes: u64,
    /// Ceiling on a single request.
    pub timeout: Duration,
    /// Minimum gap between requests to the same host.
    pub rate: Duration,
}

impl Limits {
    /// Unpaced: one download, with nothing before or after it to be polite to.
    pub fn new(max_bytes: u64, timeout: Duration) -> Self {
        Self {
            max_bytes,
            timeout,
            rate: Duration::ZERO,
        }
    }

    pub fn paced(mut self, rate: Duration) -> Self {
        self.rate = rate;
        self
    }
}

/// Try each source in turn; the first that yields a real PDF wins.
///
/// Returns the source that worked, so it can be recorded in `x-bib` and a later
/// re-fetch can be traced.
pub fn download_first(
    http: &Http,
    sources: &[PdfSource],
    dest: &Path,
    limits: Limits,
) -> Result<PdfSource, Vec<FetchError>> {
    if sources.is_empty() {
        return Err(vec![FetchError::NoSource]);
    }
    let mut failures = Vec::new();
    for source in sources {
        match download(http, &source.url, dest, limits) {
            Ok(()) => return Ok(source.clone()),
            Err(e) => failures.push(e),
        }
    }
    Err(failures)
}

/// Download one URL to `dest`, verifying it really is a PDF.
pub fn download(http: &Http, url: &str, dest: &Path, limits: Limits) -> Result<(), FetchError> {
    let mut reader = http
        .get_reader(url, "application/pdf", limits.timeout, limits.rate)
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
        if written > limits.max_bytes {
            // Refused mid-stream rather than after: the point of a cap is not
            // to fill the disk first and complain afterwards.
            return Err(FetchError::TooLarge {
                url: url.to_owned(),
                limit: limits.max_bytes,
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
            Some("1706.03762"),
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

    /// The landing page for a record whose arXiv ID was never recorded as one:
    /// the rewrite is the only thing standing between this and an HTML page
    /// saved as `paper.pdf`.
    #[test]
    fn an_abs_link_is_rewritten_to_the_pdf_and_the_page_kept_as_fallback() {
        let sources = candidates(None, None, Some("http://arxiv.org/abs/0802.1919"));
        assert_eq!(
            sources.iter().map(|s| s.url.as_str()).collect::<Vec<_>>(),
            [
                "https://arxiv.org/pdf/0802.1919",
                "http://arxiv.org/abs/0802.1919"
            ]
        );
    }

    /// The same paper reached two ways must not be downloaded twice.
    #[test]
    fn a_rewritten_url_does_not_duplicate_the_arxiv_source() {
        let sources = candidates(
            Some("0802.1919"),
            None,
            Some("https://arxiv.org/abs/0802.1919"),
        );
        assert_eq!(sources.len(), 2, "{sources:?}");
        assert_eq!(sources[0].url, "https://arxiv.org/pdf/0802.1919");
        assert_eq!(sources[1].url, "https://arxiv.org/abs/0802.1919");
    }

    #[test]
    fn known_landing_pages_are_rewritten() {
        assert_eq!(
            pdf_form("https://openreview.net/forum?id=abc123").as_deref(),
            Some("https://openreview.net/pdf?id=abc123")
        );
        assert_eq!(
            pdf_form("https://www.biorxiv.org/content/10.1101/2020.01.01.000001v1").as_deref(),
            Some("https://www.biorxiv.org/content/10.1101/2020.01.01.000001v1.full.pdf")
        );
    }

    /// Anything whose PDF location is not mechanical is left alone rather than
    /// guessed at — a publisher landing page is tried as itself, and fails
    /// honestly.
    #[test]
    fn an_unknown_landing_page_is_not_rewritten() {
        assert!(pdf_form("https://doi.org/10.1038/s41586-019-1666-5").is_none());
        assert!(pdf_form("https://link.aps.org/doi/10.1103/PRXQuantum.3.020365").is_none());
        assert!(pdf_form("https://example.org/openreview.net/forum?id=x").is_none());
        assert!(pdf_form("").is_none());
    }

    #[test]
    fn nothing_known_yields_no_sources() {
        assert!(candidates(None, None, None).is_empty());
        assert!(candidates(None, Some("  "), None).is_empty());
        assert!(candidates(Some(" "), None, Some("")).is_empty());
    }

    #[test]
    fn html_is_described_as_html() {
        assert_eq!(describe(b"<!DOCTYPE html><html>"), "an HTML page");
        assert_eq!(describe(b"  <html lang=\"en\">"), "an HTML page");
        assert_eq!(describe(b""), "an empty response");
    }
}
