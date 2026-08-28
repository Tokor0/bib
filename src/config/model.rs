//! Serde model for `config.toml`.
//!
//! Every struct is `#[serde(default)]` so that a partial file is valid, and
//! `deny_unknown_fields` so that a typo is an error rather than a silently
//! ignored setting.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_LIBRARY_NAME: &str = "main";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Library used when `--library` is not given.
    pub default_library: Option<String>,
    pub libraries: BTreeMap<String, LibraryConfig>,
    pub citekey: CitekeyConfig,
    pub folder: FolderConfig,
    pub providers: ProvidersConfig,
    pub pdf: PdfConfig,
    pub export: ExportConfig,
    pub fetch: FetchConfig,
    /// Commands used to open attachments, keyed by extension (`pdf`, `epub`, …).
    /// The value is a minijinja template receiving `file`.
    pub open: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut libraries = BTreeMap::new();
        libraries.insert(
            DEFAULT_LIBRARY_NAME.to_owned(),
            LibraryConfig {
                dir: PathBuf::from("~/Documents/library"),
            },
        );
        Self {
            default_library: Some(DEFAULT_LIBRARY_NAME.to_owned()),
            libraries,
            citekey: CitekeyConfig::default(),
            folder: FolderConfig::default(),
            providers: ProvidersConfig::default(),
            pdf: PdfConfig::default(),
            export: ExportConfig::default(),
            fetch: FetchConfig::default(),
            open: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryConfig {
    /// Library root. A leading `~` is expanded against `$HOME`.
    pub dir: PathBuf,
}

// ---------------------------------------------------------------- cite keys

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CitekeyConfig {
    /// Candidate templates, tried in order; the first that renders without an
    /// undefined value wins.
    pub templates: Vec<String>,
    pub on_collision: CollisionPolicy,
    pub max_length: usize,
    pub normalize: Normalize,
}

impl Default for CitekeyConfig {
    fn default() -> Self {
        Self {
            templates: vec![
                "{{ author[0].family | ascii | lower }}{{ date.year }}\
                 {{ title | nostop | words(1) | ascii | lower }}"
                    .to_owned(),
                "{{ editor[0].family | ascii | lower }}{{ date.year }}".to_owned(),
                "{{ title | nostop | slug | truncate(24) }}{{ date.year }}".to_owned(),
            ],
            on_collision: CollisionPolicy::SuffixAlpha,
            max_length: 48,
            normalize: Normalize::Nfkd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CollisionPolicy {
    /// `smith2020a`, `smith2020b`, …
    SuffixAlpha,
    /// `smith2020-2`, `smith2020-3`, …
    SuffixNumeric,
    /// Refuse to add a document whose key already exists.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Normalize {
    Nfc,
    Nfkc,
    Nfd,
    Nfkd,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FolderConfig {
    /// Template for a document's directory name, relative to the library root.
    /// May contain `/` to nest, e.g. `{{ date.year }}/{{ citekey }}`.
    pub template: String,
}

impl Default for FolderConfig {
    fn default() -> Self {
        Self {
            template: "{{ citekey }}".to_owned(),
        }
    }
}

// ---------------------------------------------------------------- providers

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    /// Merge priority: earlier providers win field-by-field.
    pub order: Vec<String>,
    /// Contact address sent in the User-Agent, which puts Crossref requests in
    /// their "polite pool".
    pub mailto: Option<String>,
    /// Per-provider tuning, e.g. `[providers.arxiv] rate_limit = "3s"`.
    #[serde(flatten)]
    pub tuning: BTreeMap<String, ProviderTuning>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        let mut tuning = BTreeMap::new();
        // arXiv's terms of use: at most one request every three seconds.
        tuning.insert(
            "arxiv".to_owned(),
            ProviderTuning {
                rate_limit: Some(Duration::from_secs(3)),
                enabled: true,
                base_url: None,
            },
        );
        Self {
            order: [
                "crossref",
                "openalex",
                "arxiv",
                "openlibrary",
                "google-books",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
            mailto: None,
            tuning,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderTuning {
    /// Minimum interval between requests to this provider.
    #[serde(with = "humantime_serde::option")]
    pub rate_limit: Option<Duration>,
    pub enabled: bool,
    /// Override the service host — for a mirror, a proxy, or a test double.
    pub base_url: Option<String>,
}

impl Default for ProviderTuning {
    fn default() -> Self {
        Self {
            rate_limit: None,
            enabled: true,
            base_url: None,
        }
    }
}

// --------------------------------------------------------------------- pdf

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PdfConfig {
    pub ocr: OcrMode,
    /// OCR languages, passed to tesseract as `-l a+b`.
    ///
    /// Must be a subset of the languages the tesseract on PATH actually has:
    /// `pkgs.tesseract` ships none unless `enableLanguages` names them.
    pub ocr_languages: Vec<String>,
    /// Per-invocation timeout; malformed PDFs can hang poppler indefinitely.
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    /// Upper bound on pages read during a full-document identifier scan.
    pub max_scan_pages: usize,
    /// Explicit binary paths. Normally left unset and supplied by the Nix
    /// wrapper via `BIB_PDF__PDFTOTEXT` and friends.
    pub pdftotext: Option<PathBuf>,
    pub pdfinfo: Option<PathBuf>,
    pub pdftoppm: Option<PathBuf>,
    pub tesseract: Option<PathBuf>,
}

impl Default for PdfConfig {
    fn default() -> Self {
        Self {
            ocr: OcrMode::Auto,
            ocr_languages: vec!["eng".to_owned()],
            timeout: Duration::from_secs(20),
            max_scan_pages: 40,
            pdftotext: None,
            pdfinfo: None,
            pdftoppm: None,
            tesseract: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OcrMode {
    /// OCR only when a page yields essentially no extractable text.
    Auto,
    Never,
    Always,
}

// ------------------------------------------------------------------ export

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportConfig {
    /// Entry fields left out of an exported bibliography.
    ///
    /// `abstract` by default. No citation style renders one, so in the file
    /// Typst reads it is weight and nothing else — for a library of a few dozen
    /// papers it is most of the bytes, and it turns every regenerated
    /// bibliography into a diff nobody can read. The abstract stays in the
    /// library either way: this setting is about the bibliography, not the
    /// record. Set to `[]` to export everything.
    pub exclude: Vec<String>,
    pub hayagriva: HayagrivaExportConfig,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            exclude: vec!["abstract".to_owned()],
            hayagriva: HayagrivaExportConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HayagrivaExportConfig {
    /// Written to by `bib export` when `-o` is omitted.
    pub default_path: PathBuf,
}

impl Default for HayagrivaExportConfig {
    fn default() -> Self {
        Self {
            default_path: PathBuf::from("bibliography.yml"),
        }
    }
}

// ------------------------------------------------------------------- fetch

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FetchConfig {
    /// Download the PDF on every `bib add`, without being asked.
    ///
    /// Off by default: adding a document should not reach out to a publisher
    /// unless that is what was wanted, and for most records it would fail.
    pub auto: bool,
    /// Refused mid-stream once exceeded.
    #[serde(with = "human_size")]
    pub max_size: u64,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    /// Minimum interval between downloads from the same host.
    ///
    /// Fetching a whole library is one client asking arxiv.org for every paper
    /// in it, back to back; arXiv asks for three seconds between requests and
    /// blocks callers who do not leave them. The gap is only ever waited when
    /// two downloads go to the same host in a row, so a single `bib add
    /// --fetch` pays nothing.
    #[serde(with = "humantime_serde")]
    pub rate_limit: Duration,
    /// Which mechanisms may supply a URL, in order.
    pub sources: Vec<String>,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            auto: false,
            max_size: 100 * 1024 * 1024,
            timeout: Duration::from_secs(60),
            rate_limit: Duration::from_secs(3),
            sources: ["arxiv", "openalex", "url"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

/// `"100MB"` in the file, bytes in memory.
mod human_size {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}MB", bytes / (1024 * 1024)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Size {
            Text(String),
            Bytes(u64),
        }
        match Size::deserialize(d)? {
            Size::Bytes(bytes) => Ok(bytes),
            Size::Text(text) => parse(&text).map_err(serde::de::Error::custom),
        }
    }

    pub fn parse(text: &str) -> Result<u64, String> {
        let trimmed = text.trim();
        let digits: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let number: f64 = digits
            .parse()
            .map_err(|_| format!("`{text}` is not a size"))?;
        let unit = trimmed[digits.len()..].trim().to_ascii_uppercase();
        let scale: u64 = match unit.as_str() {
            "" | "B" => 1,
            "K" | "KB" | "KIB" => 1024,
            "M" | "MB" | "MIB" => 1024 * 1024,
            "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
            other => return Err(format!("unknown size unit `{other}`")),
        };
        Ok((number * scale as f64) as u64)
    }

    #[cfg(test)]
    mod tests {
        use super::parse;

        #[test]
        fn human_sizes_parse() {
            assert_eq!(parse("100MB").unwrap(), 100 * 1024 * 1024);
            assert_eq!(parse("2 GB").unwrap(), 2 * 1024 * 1024 * 1024);
            assert_eq!(parse("512K").unwrap(), 512 * 1024);
            assert_eq!(parse("1024").unwrap(), 1024);
            assert_eq!(parse("1.5MB").unwrap(), 1024 * 1024 * 3 / 2);
        }

        #[test]
        fn a_bad_size_is_an_error_not_a_default() {
            assert!(parse("lots").is_err());
            assert!(parse("10 furlongs").is_err());
        }
    }
}
