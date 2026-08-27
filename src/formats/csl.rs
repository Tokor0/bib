//! CSL-JSON, the interchange format between providers and `info.yml`.
//!
//! Every provider normalises to [`CslItem`], and this is the single place that
//! turns one into a hayagriva entry body. That mapping has to be written by
//! hand: **hayagriva 0.10 has no CSL-JSON input** — `io` offers only
//! `from_yaml_str` and `from_biblatex*` — so there is no library to delegate to.
//!
//! DOI content negotiation would also serve `application/x-bibtex`, which would
//! have reused the tested BibTeX path and needed no mapper at all. It was
//! rejected on the evidence: Crossref's BibTeX for `10.1002/andp.19053221004`
//! renders the page range with a U+2013 en-dash rather than a parseable range,
//! collapses the date to a bare `month=Jan`, and drops the licence, the precise
//! work type and the abstract — all of which the CSL-JSON for the same DOI
//! carries correctly.

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

/// The CSL-JSON fields worth mapping.
///
/// Unknown fields are ignored rather than rejected: CSL-JSON is large, provider
/// dialects differ, and a new key upstream must not break an import.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CslItem {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<Flexible>,
    #[serde(default, rename = "container-title")]
    pub container_title: Option<Flexible>,
    #[serde(default, rename = "collection-title")]
    pub collection_title: Option<Flexible>,
    #[serde(default)]
    pub author: Vec<CslName>,
    #[serde(default)]
    pub editor: Vec<CslName>,
    #[serde(default)]
    pub translator: Vec<CslName>,
    #[serde(default)]
    pub issued: Option<CslDate>,
    #[serde(default)]
    pub volume: Option<Flexible>,
    #[serde(default)]
    pub issue: Option<Flexible>,
    #[serde(default)]
    pub page: Option<Flexible>,
    #[serde(default)]
    pub edition: Option<Flexible>,
    #[serde(default)]
    pub publisher: Option<Flexible>,
    #[serde(default, rename = "publisher-place")]
    pub publisher_place: Option<Flexible>,
    #[serde(default, rename = "DOI")]
    pub doi: Option<String>,
    #[serde(default, rename = "ISBN")]
    pub isbn: Option<Flexible>,
    #[serde(default, rename = "ISSN")]
    pub issn: Option<Flexible>,
    #[serde(default, rename = "PMID")]
    pub pmid: Option<Flexible>,
    #[serde(default, rename = "URL")]
    pub url: Option<String>,
    #[serde(default, rename = "abstract")]
    pub abstract_: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Not CSL: how often the literature cites this work.
    ///
    /// The one signal that reliably separates a canonical paper from the
    /// several identically-titled works that quote its title. Crossref calls
    /// it `is-referenced-by-count`; OpenAlex `cited_by_count`.
    #[serde(default, rename = "is-referenced-by-count")]
    pub cited_by: Option<i64>,
    /// Not CSL: an open-access location, when a provider reports one.
    /// Advisory only — see `providers::fetch` for why it is never trusted.
    #[serde(default, skip)]
    pub oa_url: Option<String>,
    /// Not CSL: where this item came from, recorded so a merge can attribute
    /// each field.
    #[serde(default, skip)]
    pub source: String,
}

/// A CSL value that may arrive as a string, a number, or an array of either.
///
/// Providers are inconsistent about this in ways that matter: Crossref sends
/// `volume` as a string, OpenAlex as a number, and `ISSN` as an array.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Flexible {
    Text(String),
    Number(f64),
    List(Vec<Flexible>),
}

impl Flexible {
    /// Whitespace is collapsed as well as trimmed: providers send doubled
    /// spaces and line-wrapped text, and `The  art of computer programming`
    /// should not be what lands in the library.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Self::Text(t) if !t.trim().is_empty() => {
                Some(t.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            Self::Text(_) => None,
            // Whole numbers must not render as `322.0`.
            Self::Number(n) if n.fract() == 0.0 => Some(format!("{}", *n as i64)),
            Self::Number(n) => Some(n.to_string()),
            Self::List(items) => items.iter().find_map(Self::as_text),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CslName {
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub given: Option<String>,
    /// Corporate authors and anything the provider could not split.
    #[serde(default)]
    pub literal: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default, rename = "non-dropping-particle")]
    pub non_dropping_particle: Option<String>,
    #[serde(default, rename = "dropping-particle")]
    pub dropping_particle: Option<String>,
}

impl CslName {
    /// Render in hayagriva's `"Prefix Last, Suffix, First"` name form.
    pub fn to_hayagriva(&self) -> Option<String> {
        if let Some(literal) = self.literal.as_ref().filter(|l| !l.trim().is_empty()) {
            // Corporate names must not be split into given/family; hayagriva
            // reads a comma-free string as a single name.
            return Some(literal.trim().replace(',', ""));
        }
        let family = self.family.as_deref().unwrap_or("").trim();
        let given = self.given.as_deref().unwrap_or("").trim();
        if family.is_empty() && given.is_empty() {
            return None;
        }
        let particle = self.non_dropping_particle.as_deref().unwrap_or("").trim();
        let last = if particle.is_empty() {
            family.to_owned()
        } else {
            format!("{particle} {family}")
        };
        if given.is_empty() {
            return Some(last);
        }
        match self
            .suffix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(suffix) => Some(format!("{last}, {suffix}, {given}")),
            None => Some(format!("{last}, {given}")),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CslDate {
    #[serde(default, rename = "date-parts")]
    pub date_parts: Vec<Vec<Flexible>>,
    #[serde(default)]
    pub literal: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
}

impl CslDate {
    /// ISO-8601, truncated to the precision the provider actually gave.
    ///
    /// `[[2013]]` must become `2013`, not `2013-01-01`: inventing a month and
    /// day would make an approximate date look exact.
    pub fn to_iso(&self) -> Option<String> {
        if let Some(parts) = self.date_parts.first()
            && !parts.is_empty()
        {
            let numbers: Vec<i64> = parts
                .iter()
                .filter_map(|p| p.as_text())
                .filter_map(|t| t.parse::<i64>().ok())
                .collect();
            return match numbers.as_slice() {
                [year] => Some(format!("{year:04}")),
                [year, month] => Some(format!("{year:04}-{month:02}")),
                [year, month, day, ..] => Some(format!("{year:04}-{month:02}-{day:02}")),
                [] => None,
            };
        }
        let text = self.literal.as_ref().or(self.raw.as_ref())?;
        // Several providers send a plain ISO date rather than `date-parts`.
        // Passing it through keeps month and day, which the year-scraping
        // fallback below would silently discard.
        let iso: String = text.chars().take(10).collect();
        if is_iso_date(&iso) {
            return Some(iso);
        }
        let iso: String = text.chars().take(7).collect();
        if is_iso_date(&iso) {
            return Some(iso);
        }
        // Otherwise it is a printed date; keep the year if there is one.
        let year: String = text
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        (year.len() == 4).then_some(year)
    }
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, with plausible month and day values.
fn is_iso_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    let numeric =
        |s: &&str, width: usize| s.len() == width && s.chars().all(|c| c.is_ascii_digit());
    match parts.as_slice() {
        [year] => numeric(year, 4),
        [year, month] => numeric(year, 4) && numeric(month, 2) && *month <= "12" && *month >= "01",
        [year, month, day] => {
            numeric(year, 4)
                && numeric(month, 2)
                && numeric(day, 2)
                && *month <= "12"
                && *month >= "01"
                && *day >= "01"
                && *day <= "31"
        }
        _ => false,
    }
}

/// Map a CSL type onto `(entry type, parent type)`.
///
/// hayagriva models containment with a nested parent rather than a flat
/// `journal` field, so the container's *type* has to be chosen here too — it is
/// what makes a conference paper export as `@inproceedings` rather than
/// `@article`.
fn types(kind: Option<&str>) -> (&'static str, Option<&'static str>) {
    match kind.unwrap_or("article-journal") {
        "article-journal" | "article" | "review" | "review-book" => ("article", Some("periodical")),
        "article-magazine" => ("article", Some("periodical")),
        "article-newspaper" => ("article", Some("newspaper")),
        "paper-conference" | "speech" => ("article", Some("proceedings")),
        "chapter" | "entry" | "entry-dictionary" | "entry-encyclopedia" => {
            ("chapter", Some("anthology"))
        }
        "book" | "monograph" => ("book", None),
        "thesis" => ("thesis", None),
        "report" | "standard" => ("report", None),
        "webpage" | "post-weblog" | "post" => ("web", None),
        "patent" => ("patent", None),
        "manuscript" | "preprint" => ("manuscript", None),
        "dataset" | "software" => ("repository", None),
        "legislation" | "bill" => ("legislation", None),
        "legal_case" => ("case", None),
        "broadcast" | "motion_picture" => ("video", None),
        "song" | "musical_score" => ("audio", None),
        // An unrecognised type is better as a valid generic entry than as a
        // failed import.
        _ => ("misc", None),
    }
}

/// Convert a CSL item into an `info.yml` body.
pub fn to_body(item: &CslItem) -> Value {
    let (kind, parent_kind) = types(item.kind.as_deref());
    let mut out = Mapping::new();
    let mut set = |key: &str, value: Value| {
        out.insert(Value::String(key.to_owned()), value);
    };

    set("type", Value::String(kind.to_owned()));
    if let Some(title) = item.title.as_ref().and_then(Flexible::as_text) {
        set("title", Value::String(title));
    }
    if let Some(people) = names(&item.author) {
        set("author", people);
    }
    if let Some(people) = names(&item.editor) {
        set("editor", people);
    }
    if let Some(people) = names(&item.translator) {
        set("translator", people);
    }
    if let Some(date) = item.issued.as_ref().and_then(CslDate::to_iso) {
        set("date", Value::String(date));
    }
    if let Some(page) = item.page.as_ref().and_then(Flexible::as_text) {
        set("page-range", Value::String(normalize_range(&page)));
    }
    for (value, key) in [
        (&item.abstract_, "abstract"),
        (&item.note, "note"),
        (&item.url, "url"),
        (&item.language, "language"),
    ] {
        if let Some(text) = value.as_ref().filter(|t| !t.trim().is_empty()) {
            set(key, Value::String(text.trim().to_owned()));
        }
    }
    if let Some(edition) = item.edition.as_ref().and_then(Flexible::as_text) {
        set("edition", Value::String(edition));
    }

    // The publisher belongs on the work for a book and on the container for an
    // article, matching how hayagriva renders each.
    let publisher = publisher(item);
    if parent_kind.is_none()
        && let Some(publisher) = publisher.clone()
    {
        set("publisher", publisher);
    }

    let mut serial = Mapping::new();
    if let Some(doi) = item.doi.as_ref().filter(|d| !d.trim().is_empty()) {
        serial.insert(
            Value::String("doi".into()),
            Value::String(doi.trim().to_ascii_lowercase()),
        );
    }
    for (value, key) in [(&item.isbn, "isbn"), (&item.pmid, "pmid")] {
        if let Some(text) = value.as_ref().and_then(Flexible::as_text) {
            serial.insert(Value::String(key.into()), Value::String(text));
        }
    }
    if !serial.is_empty() {
        set("serial-number", Value::Mapping(serial));
    }

    // Volume and issue describe the container, not the article, so for an
    // article they move into the parent below.
    if parent_kind.is_none() {
        if let Some(volume) = item.volume.as_ref().and_then(Flexible::as_text) {
            set("volume", Value::String(volume));
        }
    }

    if let Some(parent_kind) = parent_kind {
        let mut parent = Mapping::new();
        parent.insert(
            Value::String("type".into()),
            Value::String(parent_kind.to_owned()),
        );
        if let Some(title) = item.container_title.as_ref().and_then(Flexible::as_text) {
            parent.insert(Value::String("title".into()), Value::String(title));
        }
        for (value, key) in [(&item.volume, "volume"), (&item.issue, "issue")] {
            if let Some(text) = value.as_ref().and_then(Flexible::as_text) {
                parent.insert(Value::String(key.into()), Value::String(text));
            }
        }
        if let Some(issn) = item.issn.as_ref().and_then(Flexible::as_text) {
            let mut serial = Mapping::new();
            serial.insert(Value::String("issn".into()), Value::String(issn));
            parent.insert(
                Value::String("serial-number".into()),
                Value::Mapping(serial),
            );
        }
        if let Some(publisher) = publisher {
            parent.insert(Value::String("publisher".into()), publisher);
        }
        // A parent carrying only a type is usually noise — except when that
        // type is what BibTeX export reads to choose `@inproceedings` or
        // `@incollection`. Dropping it there would silently demote every
        // conference paper to `@article`, so those two are kept even bare.
        let load_bearing = matches!(parent_kind, "proceedings" | "anthology");
        if parent.len() > 1 || load_bearing {
            set("parent", Value::Mapping(parent));
        }
    }

    Value::Mapping(out)
}

/// hayagriva models a publisher as a name plus an optional location.
fn publisher(item: &CslItem) -> Option<Value> {
    let name = item.publisher.as_ref().and_then(Flexible::as_text)?;
    match item.publisher_place.as_ref().and_then(Flexible::as_text) {
        Some(location) => {
            let mut map = Mapping::new();
            map.insert(Value::String("name".into()), Value::String(name));
            map.insert(Value::String("location".into()), Value::String(location));
            Some(Value::Mapping(map))
        }
        None => Some(Value::String(name)),
    }
}

fn names(people: &[CslName]) -> Option<Value> {
    let rendered: Vec<Value> = people
        .iter()
        .filter_map(CslName::to_hayagriva)
        .map(Value::String)
        .collect();
    (!rendered.is_empty()).then(|| Value::Sequence(rendered))
}

/// Normalise a page range to `first-last`.
///
/// Providers send en-dashes and doubled hyphens; hayagriva wants a plain range
/// so it can parse the endpoints as numbers.
fn normalize_range(raw: &str) -> String {
    let collapsed = raw
        .replace(
            ['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}'],
            "-",
        )
        .replace("--", "-");
    collapsed.split_whitespace().collect::<Vec<_>>().join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::bridge;

    fn parse(json: &str) -> CslItem {
        serde_json::from_str(json).expect("fixture should deserialize")
    }

    /// The real Crossref response for Einstein 1905, trimmed to the fields that
    /// matter. Recorded from `Accept: application/vnd.citationstyles.csl+json`.
    const EINSTEIN: &str = r#"{
      "type": "journal-article",
      "title": "Zur Elektrodynamik bewegter Körper",
      "container-title": "Annalen der Physik",
      "author": [{"given": "A.", "family": "Einstein", "sequence": "first"}],
      "issued": {"date-parts": [[1905, 1]]},
      "published-print": {"date-parts": [[1905, 1]]},
      "volume": "322",
      "issue": "10",
      "page": "891-921",
      "publisher": "Wiley",
      "DOI": "10.1002/andp.19053221004",
      "ISSN": ["1521-3889"],
      "reference-count": 4
    }"#;

    #[test]
    fn a_journal_article_becomes_a_nested_hayagriva_entry() {
        // Crossref says "journal-article"; CSL proper says "article-journal".
        let mut item = parse(EINSTEIN);
        item.kind = Some("article-journal".into());
        let body = to_body(&item);

        let entry = bridge::to_entry("einstein1905", &body).expect("should be a valid entry");
        assert_eq!(
            entry.title().unwrap().to_string(),
            "Zur Elektrodynamik bewegter Körper"
        );
        assert_eq!(entry.doi().unwrap(), "10.1002/andp.19053221004");

        // Container becomes a parent periodical, not a flat `journal` field.
        let parent = entry.parents().first().expect("should have a parent");
        assert_eq!(parent.title().unwrap().to_string(), "Annalen der Physik");

        let date = entry.date().unwrap();
        assert_eq!(date.year, 1905);
        assert_eq!(date.month, Some(0), "hayagriva months are zero-based");
    }

    /// Unknown fields must be ignored, or every provider extension breaks
    /// imports.
    #[test]
    fn unrecognised_csl_fields_are_ignored() {
        let item = parse(r#"{"type":"book","title":"T","brand-new-field":{"a":1}}"#);
        assert_eq!(item.title.and_then(|t| t.as_text()).as_deref(), Some("T"));
    }

    /// Providers disagree about whether numbers are numbers.
    #[test]
    fn numeric_and_string_values_both_work() {
        let item = parse(
            r#"{"type":"article-journal","volume":322,"issue":"10","ISSN":["1521-3889","0003-3804"]}"#,
        );
        assert_eq!(
            item.volume.as_ref().unwrap().as_text().as_deref(),
            Some("322")
        );
        assert_eq!(
            item.issue.as_ref().unwrap().as_text().as_deref(),
            Some("10")
        );
        // An array collapses to its first usable entry.
        assert_eq!(
            item.issn.as_ref().unwrap().as_text().as_deref(),
            Some("1521-3889")
        );
    }

    /// A whole number must not acquire a decimal point on the way through.
    #[test]
    fn whole_numbers_do_not_become_floats() {
        let item = parse(r#"{"volume":322}"#);
        assert_eq!(item.volume.unwrap().as_text().as_deref(), Some("322"));
    }

    #[test]
    fn dates_keep_the_precision_they_were_given() {
        let cases = [
            (r#"{"date-parts":[[2013]]}"#, "2013"),
            (r#"{"date-parts":[[1905,1]]}"#, "1905-01"),
            (r#"{"date-parts":[[1905,6,30]]}"#, "1905-06-30"),
        ];
        for (json, expected) in cases {
            let date: CslDate = serde_json::from_str(json).unwrap();
            assert_eq!(date.to_iso().as_deref(), Some(expected), "for {json}");
        }
    }

    /// A year-only date must not be inflated to January 1st.
    #[test]
    fn a_year_only_date_stays_a_year() {
        let date: CslDate = serde_json::from_str(r#"{"date-parts":[[2013]]}"#).unwrap();
        assert_eq!(date.to_iso().as_deref(), Some("2013"));
    }

    #[test]
    fn names_render_in_hayagriva_form() {
        let cases = [
            (
                r#"{"family":"Einstein","given":"Albert"}"#,
                "Einstein, Albert",
            ),
            (
                r#"{"family":"Waals","given":"Johannes","non-dropping-particle":"van der"}"#,
                "van der Waals, Johannes",
            ),
            (
                r#"{"family":"King","given":"Martin Luther","suffix":"Jr."}"#,
                "King, Jr., Martin Luther",
            ),
            (r#"{"family":"Aristotle"}"#, "Aristotle"),
            (
                r#"{"literal":"World Health Organization"}"#,
                "World Health Organization",
            ),
        ];
        for (json, expected) in cases {
            let name: CslName = serde_json::from_str(json).unwrap();
            assert_eq!(name.to_hayagriva().as_deref(), Some(expected), "for {json}");
        }
    }

    /// A corporate name containing a comma would otherwise be read as
    /// `family, given` and come out mangled.
    #[test]
    fn a_corporate_name_is_not_split_on_its_comma() {
        let name: CslName =
            serde_json::from_str(r#"{"literal":"Smith, Jones and Partners"}"#).unwrap();
        let rendered = name.to_hayagriva().unwrap();
        assert!(!rendered.contains(','), "got {rendered:?}");
    }

    /// The concrete reason CSL-JSON was chosen over Crossref's BibTeX.
    #[test]
    fn en_dash_page_ranges_are_normalised() {
        assert_eq!(normalize_range("891\u{2013}921"), "891-921");
        assert_eq!(normalize_range("891--921"), "891-921");
        assert_eq!(normalize_range("891 - 921"), "891-921");
    }

    #[test]
    fn a_conference_paper_gets_a_proceedings_parent() {
        let item = parse(
            r#"{"type":"paper-conference","title":"Attention Is All You Need",
                "container-title":"Advances in Neural Information Processing Systems",
                "author":[{"family":"Vaswani","given":"Ashish"}],
                "issued":{"date-parts":[[2017]]}}"#,
        );
        let body = to_body(&item);
        let entry = bridge::to_entry("vaswani2017", &body).unwrap();
        // This is what makes BibTeX export produce @inproceedings.
        assert_eq!(
            entry.parents().first().map(|p| p.entry_type()),
            Some(&hayagriva::types::EntryType::Proceedings)
        );
    }

    #[test]
    fn a_book_keeps_its_publisher_and_location() {
        let item = parse(
            r#"{"type":"book","title":"The Art of Computer Programming",
                "author":[{"family":"Knuth","given":"Donald E."}],
                "publisher":"Addison-Wesley","publisher-place":"Reading, MA",
                "ISBN":"0201896834","issued":{"date-parts":[[1997]]}}"#,
        );
        let body = to_body(&item);
        let entry = bridge::to_entry("knuth1997", &body).unwrap();
        assert!(entry.parents().is_empty(), "a book has no container");
        assert_eq!(entry.isbn().unwrap(), "0201896834");

        let bibtex = crate::formats::bibtex::to_bibtex(
            &crate::model::bridge::library_from([&entry]),
            crate::formats::bibtex::Flavour::Bibtex,
        )
        .unwrap();
        assert!(bibtex.contains("address = {Reading, MA}"), "got:\n{bibtex}");
    }

    /// An unknown type must still import.
    #[test]
    fn an_unknown_type_degrades_to_misc() {
        let item = parse(r#"{"type":"interview","title":"A Conversation"}"#);
        let body = to_body(&item);
        assert!(bridge::to_entry("x", &body).is_ok());
    }

    /// Regression: a conference paper whose container title is unknown still
    /// has to export as `@inproceedings`. Because BibTeX export reads the
    /// *parent's* type to decide that, dropping a type-only parent silently
    /// demoted every such paper to `@article`.
    #[test]
    fn a_conference_paper_without_a_container_still_exports_as_inproceedings() {
        let item = parse(
            r#"{"type":"paper-conference","title":"A Talk",
                             "author":[{"family":"Smith","given":"Jane"}],
                             "issued":{"date-parts":[[2020]]}}"#,
        );
        let entry = bridge::to_entry("smith2020", &to_body(&item)).unwrap();
        let bibtex = crate::formats::bibtex::to_bibtex(
            &crate::model::bridge::library_from([&entry]),
            crate::formats::bibtex::Flavour::Bibtex,
        )
        .unwrap();
        assert!(bibtex.contains("@inproceedings"), "got:\n{bibtex}");
    }

    /// …but an ordinary article with no journal name must not acquire a
    /// meaningless `parent: {type: periodical}`.
    #[test]
    fn an_article_without_a_journal_gets_no_empty_parent() {
        let item = parse(r#"{"type":"article-journal","title":"Untitled Venue"}"#);
        let body = to_body(&item);
        assert!(
            body.get("parent").is_none(),
            "an empty parent was emitted: {body:?}"
        );
    }
}
