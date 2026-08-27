//! The boundary between "run a PDF tool" and "understand its output".
//!
//! Everything above this module works on strings that *look like* poppler
//! output, and never spawns a process. That split is what makes the great
//! majority of identification tests hermetic: [`Fixture`] replays recorded
//! output, so the tier logic, the regexes and the scoring can all be tested
//! without poppler, without a PDF, and in microseconds — while still exercising
//! the same parsers the real backend feeds.
//!
//! Every failure here is *soft*. A missing tool, a timeout or a nonzero exit
//! means the caller moves to the next tier; identification degrades rather than
//! aborting the add.

use crate::config::PdfConfig;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One extraction request.
///
/// Modelled as data rather than as trait methods so a backend has a single
/// entry point, and so each request has a stable string key usable for both the
/// on-disk cache and fixture lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Standard document info: `Title`, `Pages`, `Page size`, dates.
    Info,
    /// The info dictionary including publisher-defined custom keys.
    ///
    /// A separate invocation because `-custom` prints *only* the dictionary:
    /// it omits the computed fields, `Pages` among them. Reading page count
    /// from `-custom` silently yields 1, which disables every tier that needs
    /// to know how long the document is.
    InfoCustom,
    /// The XMP metadata packet, as XML.
    Xmp,
    /// Link-annotation URIs for pages `1..=last`.
    Urls { last: usize },
    /// Extracted text for pages `first..=last`.
    Text {
        first: usize,
        last: usize,
        layout: bool,
    },
    /// Layout extraction with per-line bounding boxes, as XHTML.
    BBox { page: usize },
    /// Rasterise and OCR pages `first..=last`.
    Ocr { first: usize, last: usize },
}

impl Op {
    /// Stable identifier for caching and fixtures.
    pub fn key(&self) -> String {
        match self {
            Self::Info => "info".into(),
            Self::InfoCustom => "info-custom".into(),
            Self::Xmp => "xmp".into(),
            Self::Urls { last } => format!("urls-{last}"),
            Self::Text {
                first,
                last,
                layout,
            } => {
                let suffix = if *layout { "-layout" } else { "" };
                format!("text-{first}-{last}{suffix}")
            }
            Self::BBox { page } => format!("bbox-{page}"),
            Self::Ocr { first, last } => format!("ocr-{first}-{last}"),
        }
    }

    /// Which binary serves this op, for error messages.
    fn tool(&self) -> &'static str {
        match self {
            Self::Info | Self::InfoCustom | Self::Xmp | Self::Urls { .. } => "pdfinfo",
            Self::Text { .. } | Self::BBox { .. } => "pdftotext",
            Self::Ocr { .. } => "tesseract",
        }
    }
}

#[derive(Debug)]
pub enum PdfError {
    /// The binary is not installed. Fatal only for the required poppler tools;
    /// tesseract is optional and its absence is reported, not raised.
    ToolMissing { tool: String },
    /// Killed after exceeding the configured per-invocation timeout.
    Timeout { tool: String, after: Duration },
    /// Ran, but exited nonzero — a damaged or encrypted PDF, usually.
    Failed { tool: String, message: String },
    Io {
        tool: String,
        source: std::io::Error,
    },
    /// The fixture backend was asked for output it does not have.
    NoFixture { key: String },
}

impl PdfError {
    pub fn is_tool_missing(&self) -> bool {
        matches!(self, Self::ToolMissing { .. })
    }
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolMissing { tool } => write!(f, "`{tool}` is not installed"),
            Self::Timeout { tool, after } => {
                write!(f, "`{tool}` timed out after {}s", after.as_secs())
            }
            Self::Failed { tool, message } => write!(f, "`{tool}` failed: {message}"),
            Self::Io { tool, source } => write!(f, "could not run `{tool}`: {source}"),
            Self::NoFixture { key } => write!(f, "no recorded output for `{key}`"),
        }
    }
}

impl std::error::Error for PdfError {}

pub trait PdfBackend {
    fn run(&self, pdf: &Path, op: &Op) -> Result<String, PdfError>;
}

// ------------------------------------------------------------------ poppler

pub struct Poppler {
    config: PdfConfig,
    /// Where extracted text is cached. Process spawn dominates the cost of
    /// identification, so repeated `add`/`update`/`doctor` runs over the same
    /// file should not pay it twice.
    cache: Option<PathBuf>,
}

impl Poppler {
    pub fn new(config: PdfConfig) -> Self {
        Self {
            config,
            cache: None,
        }
    }

    pub fn with_cache(mut self, dir: PathBuf) -> Self {
        self.cache = Some(dir);
        self
    }

    /// Resolve a tool: explicit config path, else the bare name on `PATH`.
    ///
    /// The env-var route the Nix wrapper uses (`BIB_PDF__PDFTOTEXT`) needs no
    /// code here — figment already folds `BIB_*` into the config, so an
    /// absolute store path arrives as `config.pdftotext`.
    fn tool(&self, name: &str) -> PathBuf {
        let configured = match name {
            "pdftotext" => &self.config.pdftotext,
            "pdfinfo" => &self.config.pdfinfo,
            "pdftoppm" => &self.config.pdftoppm,
            "tesseract" => &self.config.tesseract,
            _ => &None,
        };
        configured.clone().unwrap_or_else(|| PathBuf::from(name))
    }

    fn cache_path(&self, pdf: &Path, op: &Op) -> Option<PathBuf> {
        let dir = self.cache.as_ref()?;
        // Keyed on the file's identity plus *the exact command that produced
        // the output*. Keying on the op name alone is not enough: changing what
        // `Op::Info` invokes then keeps serving the previous output forever,
        // which is precisely how the `-custom` page-count bug survived its own
        // fix. Deriving the key from the arguments makes the two impossible to
        // desynchronise.
        //
        // Identity is path + size + mtime rather than a content hash: a PDF in
        // a library is effectively immutable once filed, and hashing tens of
        // megabytes to save one subprocess would be a poor trade.
        let meta = std::fs::metadata(pdf).ok()?;
        let stamp = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let fingerprint = crate::util::fnv1a_str(&format!(
            "{}|{}|{stamp}|{}",
            pdf.to_string_lossy(),
            meta.len(),
            self.invocation(pdf, op),
        ));
        Some(dir.join(format!("{fingerprint:016x}-{}.txt", op.key())))
    }

    /// A stable description of what will actually be run, for cache keying.
    fn invocation(&self, pdf: &Path, op: &Op) -> String {
        match op {
            // OCR is a pipeline, not one command, and its output depends on the
            // configured languages.
            Op::Ocr { first, last } => {
                format!("ocr|{first}|{last}|{}", self.config.ocr_languages.join("+"))
            }
            _ => self
                .args(pdf, op)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn args(&self, pdf: &Path, op: &Op) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        let mut push = |s: &str| args.push(OsString::from(s));
        match op {
            Op::Info => push("-isodates"),
            Op::InfoCustom => {
                push("-custom");
                push("-isodates");
            }
            Op::Xmp => push("-meta"),
            Op::Urls { last } => {
                push("-url");
                push("-f");
                push("1");
                push("-l");
                push(&last.to_string());
            }
            Op::Text {
                first,
                last,
                layout,
            } => {
                push("-q");
                push("-enc");
                push("UTF-8");
                push("-f");
                push(&first.to_string());
                push("-l");
                push(&last.to_string());
                if *layout {
                    push("-layout");
                }
            }
            Op::BBox { page } => {
                push("-q");
                push("-enc");
                push("UTF-8");
                push("-bbox-layout");
                push("-f");
                push(&page.to_string());
                push("-l");
                push(&page.to_string());
            }
            Op::Ocr { .. } => unreachable!("OCR is a multi-step pipeline, handled separately"),
        }
        args.push(pdf.as_os_str().to_owned());
        // pdftotext writes to a file unless told otherwise; `-` is stdout.
        if matches!(op, Op::Text { .. } | Op::BBox { .. }) {
            args.push(OsString::from("-"));
        }
        args
    }
}

impl PdfBackend for Poppler {
    fn run(&self, pdf: &Path, op: &Op) -> Result<String, PdfError> {
        if let Some(path) = self.cache_path(pdf, op)
            && let Ok(cached) = std::fs::read_to_string(&path)
        {
            return Ok(cached);
        }

        let output = match op {
            Op::Ocr { first, last } => self.ocr(pdf, *first, *last)?,
            _ => {
                let tool = self.tool(op.tool());
                run_command(&tool, op.tool(), &self.args(pdf, op), self.config.timeout)?
            }
        };

        if let Some(path) = self.cache_path(pdf, op) {
            // A cache that cannot be written is not an error: it only costs
            // time on the next run.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &output);
        }
        Ok(output)
    }
}

impl Poppler {
    /// Rasterise with `pdftoppm`, then OCR each page with `tesseract`.
    ///
    /// Two tools rather than one, so a missing tesseract is reported as
    /// tesseract missing even though `pdftoppm` succeeded.
    fn ocr(&self, pdf: &Path, first: usize, last: usize) -> Result<String, PdfError> {
        let dir = tempfile::tempdir().map_err(|source| PdfError::Io {
            tool: "pdftoppm".into(),
            source,
        })?;
        let prefix = dir.path().join("page");

        let args: Vec<OsString> = [
            "-r",
            "300",
            "-png",
            "-f",
            &first.to_string(),
            "-l",
            &last.to_string(),
        ]
        .iter()
        .map(OsString::from)
        .chain([pdf.as_os_str().to_owned(), prefix.as_os_str().to_owned()])
        .collect();
        run_command(
            &self.tool("pdftoppm"),
            "pdftoppm",
            &args,
            self.config.timeout,
        )?;

        let mut images: Vec<PathBuf> = std::fs::read_dir(dir.path())
            .map_err(|source| PdfError::Io {
                tool: "pdftoppm".into(),
                source,
            })?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "png"))
            .collect();
        images.sort();

        let languages = if self.config.ocr_languages.is_empty() {
            "eng".to_owned()
        } else {
            self.config.ocr_languages.join("+")
        };

        let mut text = String::new();
        for image in images {
            let args: Vec<OsString> = [
                image.as_os_str().to_owned(),
                OsString::from("-"),
                OsString::from("-l"),
                OsString::from(&languages),
            ]
            .into();
            text.push_str(&run_command(
                &self.tool("tesseract"),
                "tesseract",
                &args,
                self.config.timeout,
            )?);
            // Page separator, matching what pdftotext emits.
            text.push('\u{c}');
        }
        Ok(text)
    }
}

/// Run a command with a hard timeout, killing the child if it overruns.
///
/// Output goes to temporary files rather than pipes: with pipes, a child that
/// produces more than the pipe buffer blocks on write while we are blocked
/// waiting for it to exit — a deadlock that a timeout would then paper over as
/// a hang. Files have no such limit, so `try_wait` polling is safe.
fn run_command(
    program: &Path,
    tool: &str,
    args: &[OsString],
    timeout: Duration,
) -> Result<String, PdfError> {
    let scratch = tempfile::tempdir().map_err(|source| PdfError::Io {
        tool: tool.into(),
        source,
    })?;
    let out_path = scratch.path().join("stdout");
    let err_path = scratch.path().join("stderr");

    let open = |path: &Path| {
        std::fs::File::create(path).map_err(|source| PdfError::Io {
            tool: tool.into(),
            source,
        })
    };

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(open(&out_path)?))
        .stderr(Stdio::from(open(&err_path)?))
        .spawn()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                PdfError::ToolMissing { tool: tool.into() }
            } else {
                PdfError::Io {
                    tool: tool.into(),
                    source,
                }
            }
        })?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PdfError::Timeout {
                        tool: tool.into(),
                        after: timeout,
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(source) => {
                return Err(PdfError::Io {
                    tool: tool.into(),
                    source,
                });
            }
        }
    };

    if !status.success() {
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        let message = stderr
            .lines()
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("exited with {status}"));
        return Err(PdfError::Failed {
            tool: tool.into(),
            message,
        });
    }

    // Lossy on purpose: poppler can emit stray bytes from damaged PDFs, and
    // losing a character is better than losing the whole extraction.
    let bytes = std::fs::read(&out_path).map_err(|source| PdfError::Io {
        tool: tool.into(),
        source,
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ------------------------------------------------------------------ fixture

/// A backend that replays recorded output, keyed by [`Op::key`].
#[derive(Debug, Default, Clone)]
pub struct Fixture {
    responses: BTreeMap<String, Result<String, String>>,
}

impl Fixture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, op: &Op, output: impl Into<String>) -> Self {
        self.responses.insert(op.key(), Ok(output.into()));
        self
    }

    /// Record a tier that fails, so the fallback path can be tested too.
    pub fn failing(mut self, op: &Op, message: impl Into<String>) -> Self {
        self.responses.insert(op.key(), Err(message.into()));
        self
    }
}

impl PdfBackend for Fixture {
    fn run(&self, _pdf: &Path, op: &Op) -> Result<String, PdfError> {
        match self.responses.get(&op.key()) {
            Some(Ok(text)) => Ok(text.clone()),
            Some(Err(message)) => Err(PdfError::Failed {
                tool: op.tool().into(),
                message: message.clone(),
            }),
            // An unrecorded op is a missing tier, not a crash: tests describe
            // only the tiers they care about.
            None => Err(PdfError::NoFixture { key: op.key() }),
        }
    }
}
