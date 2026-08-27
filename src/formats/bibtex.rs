//! BibTeX and BibLaTeX.
//!
//! Import goes through hayagriva, which already understands `.bib` and, unlike
//! most parsers, interprets field *contents* (names, dates, `$math$`).
//!
//! Export is the other direction, and hayagriva does not provide it, so the
//! hayagriva -> `biblatex::Entry` mapping is written here. Fields are set
//! through the generic `set`/`set_as` API rather than the typed per-field
//! setters: BibTeX values are ultimately text, and going through one path
//! keeps the mapping readable.

use anyhow::{Result, anyhow};
use biblatex::{Bibliography, Chunk, Spanned};
use hayagriva::Library;
use hayagriva::types::EntryType as HgType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Bibtex,
    Biblatex,
}

/// Parse a `.bib` file into hayagriva entries.
pub fn from_bibtex(source: &str) -> Result<Library> {
    hayagriva::io::from_biblatex_str(source).map_err(|errors| {
        // A badly broken file can produce hundreds of errors; the first few are
        // what identify the problem.
        let shown: Vec<String> = errors.iter().take(5).map(|e| format!("{e:?}")).collect();
        let more = errors.len().saturating_sub(shown.len());
        let suffix = if more > 0 {
            format!("\n  … and {more} more")
        } else {
            String::new()
        };
        anyhow!("could not parse BibTeX:\n  {}{suffix}", shown.join("\n  "))
    })
}

/// Convert hayagriva entries into a `.bib` document.
pub fn to_bibtex(library: &Library, flavour: Flavour) -> Result<String> {
    let mut bibliography = Bibliography::new();

    for entry in library {
        let kind = bibtex_type(entry);
        let mut out = biblatex::Entry::new(entry.key().to_owned(), kind.clone());

        if let Some(title) = entry.title() {
            out.set("title", chunks(&title.to_string()));
        }
        if let Some(authors) = entry.authors() {
            out.set_as("author", &persons(authors));
        }
        if let Some(editors) = entry.editors() {
            out.set_as("editor", &persons(editors));
        }
        if let Some(date) = entry.date() {
            match flavour {
                // BibLaTeX prefers an ISO `date`; BibTeX only knows year/month.
                Flavour::Biblatex => out.set("date", chunks(&iso_date(date))),
                Flavour::Bibtex => {
                    out.set("year", chunks(&date.year.to_string()));
                    if let Some(month) = date.month {
                        // hayagriva months are zero-based.
                        out.set("month", chunks(&(month + 1).to_string()));
                    }
                }
            }
        }
        if let Some(doi) = entry.doi() {
            out.set("doi", chunks(doi));
        }
        if let Some(isbn) = entry.isbn() {
            out.set("isbn", chunks(isbn));
        }
        if let Some(issn) = entry.issn() {
            out.set("issn", chunks(issn));
        }
        if let Some(url) = entry.url() {
            out.set("url", chunks(url.value.as_str()));
        }
        if let Some(pages) = entry.page_range() {
            out.set("pages", chunks(&pages.to_string()));
        }
        if let Some(volume) = entry.volume() {
            out.set("volume", chunks(&volume.to_string()));
        }
        if let Some(publisher) = entry.publisher().and_then(|p| p.name()) {
            out.set("publisher", chunks(&publisher.to_string()));
        }
        // hayagriva nests location under the publisher; BibTeX wants a
        // top-level `address`.
        let location = entry.location().map(|l| l.to_string()).or_else(|| {
            entry
                .publisher()
                .and_then(|p| p.location())
                .map(|l| l.to_string())
        });
        if let Some(location) = location {
            out.set("address", chunks(&location));
        }
        if let Some(note) = entry.note() {
            out.set("note", chunks(&note.to_string()));
        }

        // The containing work becomes `journal` for an article and `booktitle`
        // otherwise, which is what BibTeX styles expect.
        if let Some(parent) = entry.parents().first() {
            if let Some(title) = parent.title() {
                // Follows the resolved BibTeX type, not the hayagriva one: an
                //  takes `booktitle` even though hayagriva calls
                // it an article.
                let field = if kind == biblatex::EntryType::Article {
                    "journal"
                } else {
                    "booktitle"
                };
                out.set(field, chunks(&title.to_string()));
            }
            // A journal's own volume/issue lives on the parent in hayagriva.
            if let Some(volume) = parent.volume() {
                out.set("volume", chunks(&volume.to_string()));
            }
            if let Some(issue) = parent.issue() {
                out.set("number", chunks(&issue.to_string()));
            }
        }

        bibliography.insert(out);
    }

    Ok(match flavour {
        Flavour::Bibtex => bibliography.to_bibtex_string(),
        Flavour::Biblatex => bibliography.to_biblatex_string(),
    })
}

/// Map hayagriva's rich type set onto BibTeX's much smaller one.
///
/// Lossy by nature: BibTeX has no concept of, say, a podcast episode, so
/// anything without a natural counterpart lands on `misc`.
fn bibtex_type(entry: &hayagriva::Entry) -> biblatex::EntryType {
    use biblatex::EntryType as B;

    // The container decides between the "in-" types: BibTeX distinguishes a
    // conference paper from a journal article by entry type, where hayagriva
    // distinguishes them by what the parent is.
    let parent_type = entry.parents().first().map(|p| p.entry_type());
    match (entry.entry_type(), parent_type) {
        (HgType::Article, Some(HgType::Proceedings)) => return B::InProceedings,
        (HgType::Article, Some(HgType::Anthology)) => return B::InCollection,
        (HgType::Chapter, Some(HgType::Anthology)) => return B::InCollection,
        _ => {}
    }

    match entry.entry_type() {
        HgType::Article | HgType::Blog | HgType::Newspaper | HgType::Periodical => B::Article,
        HgType::Book | HgType::Anthology => B::Book,
        HgType::Chapter | HgType::Entry => B::InBook,
        HgType::Proceedings => B::Proceedings,
        HgType::Thesis => B::Thesis,
        HgType::Report => B::Report,
        HgType::Manuscript => B::Unpublished,
        HgType::Web | HgType::Misc => B::Online,
        HgType::Patent => B::Patent,
        _ => B::Misc,
    }
}

fn persons(list: &[hayagriva::types::Person]) -> Vec<biblatex::Person> {
    list.iter()
        .map(|p| biblatex::Person {
            name: p.name.clone(),
            given_name: p.given_name.clone().unwrap_or_default(),
            prefix: p.prefix.clone().unwrap_or_default(),
            suffix: p.suffix.clone().unwrap_or_default(),
            // hayagriva models none of these; let biblatex derive them.
            id: None,
            prefix_initials: None,
            given_initials: None,
            use_prefix: None,
        })
        .collect()
}

/// hayagriva stores month and day zero-based; ISO 8601 is one-based.
fn iso_date(date: &hayagriva::types::Date) -> String {
    match (date.month, date.day) {
        (Some(m), Some(d)) => format!("{:04}-{:02}-{:02}", date.year, m + 1, d + 1),
        (Some(m), None) => format!("{:04}-{:02}", date.year, m + 1),
        _ => format!("{:04}", date.year),
    }
}

fn chunks(text: &str) -> Vec<Spanned<Chunk>> {
    vec![Spanned::zero(Chunk::Normal(text.to_owned()))]
}
