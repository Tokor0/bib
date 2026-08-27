//! The result shape shared by local and web search.
//!
//! A launcher wants instant library hits *and* slower web hits in one list,
//! which means two invocations whose output it merges. If `bib search --json`
//! and `bib find --json` emitted different objects the caller would need two
//! mappings and a fiddly merge, so both emit this.
//!
//! Every field a launcher needs to *act* is present, so activating a row costs
//! no second process spawn: `cite` for the Typst clipboard action, `id` for the
//! identifier one, `files` for opening the document.

use crate::formats::csl;
use crate::identify::patterns::Identifier;
use crate::model::Document;
use crate::providers::search::Candidate;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Stable, self-contained handle: `doi:…`, `arxiv:…`, `isbn:…`, or the
    /// cite key for a library entry with no identifier.
    ///
    /// Actions key off this and never off a list position — by the time a
    /// launcher activates a row, the result set may have been re-queried.
    pub id: String,
    /// `library`, or the provider that supplied the record.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citekey: Option<String>,
    /// `@citekey`, ready for the clipboard. Absent for results not in the
    /// library, which have no cite key yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<String>,
    pub title: String,
    /// One line of context: authors, year, venue.
    pub subtitle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PathBuf>,
    pub in_library: bool,
}

impl SearchResult {
    /// A document already in the library.
    pub fn from_document(doc: &Document) -> anyhow::Result<Self> {
        let entry = doc.entry()?;
        let title = entry
            .title()
            .map(|t| t.to_string())
            .unwrap_or_else(|| doc.citekey.clone());
        let authors: Vec<String> = entry
            .authors()
            .unwrap_or_default()
            .iter()
            // Family names only: a launcher row is one line, and the surname is
            // what a reader scans for.
            .map(|p| p.name.clone())
            .collect();
        let year = entry.date().map(|d| d.year as i64);
        let container = entry
            .parents()
            .first()
            .and_then(|p| p.title())
            .map(|t| t.to_string());

        // Prefer a real identifier so a library hit and a web hit for the same
        // paper carry the same `id`, which is what lets a caller spot the
        // overlap without comparing titles.
        let id = entry
            .doi()
            .and_then(Identifier::parse_doi)
            .map(|i| i.to_string())
            .or_else(|| entry.arxiv().map(|a| format!("arxiv:{a}")))
            .unwrap_or_else(|| doc.citekey.clone());

        Ok(Self {
            id,
            source: "library".into(),
            citekey: Some(doc.citekey.clone()),
            cite: Some(format!("@{}", doc.citekey)),
            subtitle: subtitle(&authors, year, container.as_deref()),
            title,
            year,
            authors,
            container,
            tags: doc.meta().tags,
            files: doc.files(),
            in_library: true,
        })
    }

    /// A candidate from a web search.
    pub fn from_candidate(candidate: &Candidate, citekey: Option<String>) -> Option<Self> {
        let id = candidate.id.as_ref()?.to_string();
        let item = &candidate.item;
        let title = candidate.title()?;
        let authors: Vec<String> = item
            .author
            .iter()
            .filter_map(|a| {
                a.literal
                    .clone()
                    .or_else(|| a.family.clone())
                    .filter(|n| !n.trim().is_empty())
            })
            .collect();
        let year = item
            .issued
            .as_ref()
            .and_then(|d| d.to_iso())
            .and_then(|iso| iso.get(..4).and_then(|y| y.parse().ok()));
        let container = item
            .container_title
            .as_ref()
            .and_then(csl::Flexible::as_text);

        Some(Self {
            id,
            source: item.source.clone(),
            cite: citekey.as_ref().map(|k| format!("@{k}")),
            in_library: citekey.is_some(),
            citekey,
            subtitle: subtitle(&authors, year, container.as_deref()),
            title,
            year,
            authors,
            container,
            tags: Vec::new(),
            files: Vec::new(),
        })
    }
}

/// `Einstein, Shazeer · 1905 · Annalen der Physik`, trimmed of empty parts.
fn subtitle(authors: &[String], year: Option<i64>, container: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !authors.is_empty() {
        // Three names then "et al." — a launcher row has one line.
        let shown: Vec<&str> = authors.iter().take(3).map(String::as_str).collect();
        let mut names = shown.join(", ");
        if authors.len() > 3 {
            names.push_str(" et al.");
        }
        parts.push(names);
    }
    if let Some(year) = year {
        parts.push(year.to_string());
    }
    if let Some(container) = container.filter(|c| !c.trim().is_empty()) {
        parts.push(container.to_owned());
    }
    // Newlines would break both a `--format` line and a launcher row, and a
    // hand-edited `info.yml` can contain them.
    parts.join(" · ").replace(['\n', '\r'], " ")
}

/// Serialize a result set as JSON.
///
/// An empty set is `[]` and still succeeds: "nothing matched" is an answer, and
/// a caller should not have to distinguish it from a failure by exit code.
pub fn to_json(results: &[SearchResult]) -> anyhow::Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(results)?))
}

/// Render results with a minijinja template, one line each.
///
/// The context carries the result fields (`id`, `cite`, `source`, …) *and*,
/// for a library row, the entry fields cite-key templates use (`author`,
/// `date`, `page-range`, …). Without the first set, `--format` and `--json`
/// would disagree about what a result even has — and a launcher building a
/// `wofi` line needs `{{ id }}` to map the selection back to an action.
///
/// Newlines in the rendered line are collapsed: a line-oriented consumer treats
/// one line as one item, and a hand-edited `info.yml` title can contain
/// anything.
pub fn render_results(
    results: &[SearchResult],
    entries: &[Option<hayagriva::Entry>],
    template: &str,
) -> anyhow::Result<String> {
    use anyhow::Context;
    let env = crate::model::citekey::template_env();
    let template = unescape_template(template);
    let mut out = String::new();

    for (index, result) in results.iter().enumerate() {
        let context = context_for(result, entries.get(index).and_then(Option::as_ref));
        let line = env
            .render_str(&template, context)
            .with_context(|| format!("rendering --format for `{}`", result.id))?;
        out.push_str(&line.replace(['\n', '\r'], " "));
        out.push('\n');
    }
    Ok(out)
}

/// Interpret backslash escapes in a `--format` template.
///
/// A shell passes `--format '{{ id }}\t{{ title }}'` through as a literal
/// backslash and `t`, so without this the documented `… | cut -f1` recipes
/// silently do not work. `\n` is deliberately *not* interpreted: one line per
/// result is the contract a line-oriented consumer relies on, and the rendered
/// output has its newlines collapsed anyway.
fn unescape_template(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            // Anything else is left as written, so a stray backslash in a
            // template is not silently eaten.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Merge the result fields over the entry fields.
///
/// Result fields win on a collision: `title` as a plain string is what a
/// one-line template wants, where the entry's is a formatted structure.
fn context_for(result: &SearchResult, entry: Option<&hayagriva::Entry>) -> minijinja::Value {
    let mut map: std::collections::BTreeMap<String, minijinja::Value> = Default::default();

    if let Some(entry) = entry {
        let base = crate::model::citekey::context_for(entry, result.citekey.as_deref());
        if let Ok(keys) = base.try_iter() {
            for key in keys {
                if let Ok(value) = base.get_item(&key) {
                    map.insert(key.to_string(), value);
                }
            }
        }
    }

    let overlay = minijinja::Value::from_serialize(result);
    if let Ok(keys) = overlay.try_iter() {
        for key in keys {
            if let Ok(value) = overlay.get_item(&key) {
                map.insert(key.to_string(), value);
            }
        }
    }
    // Absent optionals must still resolve, or a template naming `cite` fails on
    // every web result rather than rendering an empty column.
    for key in ["citekey", "cite", "container"] {
        map.entry(key.to_owned())
            .or_insert(minijinja::Value::from(""));
    }
    minijinja::Value::from(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(citekey: &str, yaml: &str) -> Document {
        Document {
            citekey: citekey.to_owned(),
            dir: PathBuf::from("/library").join(citekey),
            value: serde_yaml::from_str(yaml).expect("fixture should be valid YAML"),
        }
    }

    #[test]
    fn a_library_document_carries_everything_needed_to_act_on_it() {
        let doc = document(
            "einstein1905zur",
            r#"
type: article
title: Zur Elektrodynamik bewegter Körper
author: ["Einstein, Albert"]
date: 1905
serial-number: {doi: 10.1002/andp.19053221004}
parent: {type: periodical, title: Annalen der Physik}
x-bib: {files: [paper.pdf]}
"#,
        );
        let result = SearchResult::from_document(&doc).unwrap();

        assert_eq!(result.id, "doi:10.1002/andp.19053221004");
        assert_eq!(result.source, "library");
        // The two clipboard actions and "open the PDF" need no second spawn.
        assert_eq!(result.cite.as_deref(), Some("@einstein1905zur"));
        assert_eq!(
            result.files,
            [PathBuf::from("/library/einstein1905zur/paper.pdf")]
        );
        assert!(result.in_library);
        assert_eq!(result.subtitle, "Einstein · 1905 · Annalen der Physik");
    }

    /// Without an identifier the cite key is the handle, so every row is
    /// actionable.
    #[test]
    fn a_document_with_no_identifier_falls_back_to_its_cite_key() {
        let doc = document("smith2020", "type: article\ntitle: Untitled\n");
        assert_eq!(SearchResult::from_document(&doc).unwrap().id, "smith2020");
    }

    /// A launcher row is one line; a hand-edited title must not break it.
    #[test]
    fn subtitles_never_contain_newlines() {
        let line = subtitle(&["A".into()], Some(2020), Some("A Journal\nWith A Newline"));
        assert!(!line.contains('\n'), "got {line:?}");
    }

    #[test]
    fn long_author_lists_are_abbreviated() {
        let authors: Vec<String> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(subtitle(&authors, None, None), "A, B, C et al.");
    }

    #[test]
    fn an_empty_result_set_is_an_empty_json_array() {
        assert_eq!(to_json(&[]).unwrap().trim(), "[]");
    }
}

#[cfg(test)]
mod escape_tests {
    use super::unescape_template;

    /// A shell hands us a literal backslash and `t`; the documented
    /// `bib list --format '{{ id }}\t{{ title }}' | cut -f1` recipes depend on
    /// this becoming a real tab.
    #[test]
    fn tabs_and_nuls_are_interpreted() {
        assert_eq!(unescape_template(r"a\tb"), "a\tb");
        assert_eq!(unescape_template(r"a\0b"), "a\0b");
        assert_eq!(unescape_template(r"a\\b"), r"a\b");
    }

    /// One line per result is the contract, so `\n` stays literal rather than
    /// quietly producing output the caller cannot parse.
    #[test]
    fn newlines_are_not_interpreted() {
        assert_eq!(unescape_template(r"a\nb"), r"a\nb");
    }

    #[test]
    fn an_unknown_or_trailing_escape_is_left_alone() {
        assert_eq!(unescape_template(r"a\qb"), r"a\qb");
        assert_eq!(unescape_template(r"a\"), r"a\");
    }
}
