//! Reading a papis library.
//!
//! papis stores a flat, BibTeX-flavoured `info.yaml` per document. Mapping it
//! onto hayagriva's nested model is the one genuinely lossy direction in this
//! tool, so unmapped keys are preserved under `x-bib.papis` rather than
//! dropped.

use serde_yaml::{Mapping, Value};

/// papis keys consumed by the mapping below. Anything else is kept verbatim.
const MAPPED: &[&str] = &[
    "type",
    "title",
    "author",
    "author_list",
    "editor",
    "year",
    "month",
    "journal",
    "volume",
    "number",
    "pages",
    "publisher",
    "address",
    "doi",
    "isbn",
    "issn",
    "url",
    "abstract",
    "note",
    "tags",
    "files",
    "notes",
    "ref",
    "booktitle",
    "school",
    "institution",
];

/// Convert one papis `info.yaml` mapping into an `info.yml` body.
pub fn to_body(papis: &Value) -> Value {
    let mut out = Mapping::new();
    let get = |k: &str| papis.get(k);
    let text = |k: &str| get(k).and_then(|v| v.as_str()).map(str::to_owned);

    out.insert(
        Value::String("type".into()),
        Value::String(entry_type(text("type").as_deref())),
    );
    for (from, to) in [
        ("title", "title"),
        ("abstract", "abstract"),
        ("note", "note"),
        ("url", "url"),
        ("publisher", "publisher"),
        ("address", "location"),
        ("pages", "page-range"),
    ] {
        if let Some(v) = text(from) {
            out.insert(Value::String(to.into()), Value::String(v));
        }
    }

    if let Some(people) = persons(papis, "author_list", "author") {
        out.insert(Value::String("author".into()), people);
    }
    if let Some(people) = persons(papis, "editor_list", "editor") {
        out.insert(Value::String("editor".into()), people);
    }
    if let Some(date) = date(papis) {
        out.insert(Value::String("date".into()), Value::String(date));
    }

    let mut serial = Mapping::new();
    for key in ["doi", "isbn", "issn"] {
        if let Some(v) = text(key) {
            serial.insert(Value::String(key.into()), Value::String(v));
        }
    }
    if !serial.is_empty() {
        out.insert(
            Value::String("serial-number".into()),
            Value::Mapping(serial),
        );
    }

    // A journal or book title becomes a parent entry, which is how hayagriva
    // models containment.
    if let Some(container) = text("journal").or_else(|| text("booktitle")) {
        let is_journal = text("journal").is_some();
        let mut parent = Mapping::new();
        parent.insert(
            Value::String("type".into()),
            Value::String(
                if is_journal {
                    "periodical"
                } else {
                    "anthology"
                }
                .into(),
            ),
        );
        parent.insert(Value::String("title".into()), Value::String(container));
        if let Some(v) = text("volume") {
            parent.insert(Value::String("volume".into()), Value::String(v));
        }
        if let Some(v) = text("number") {
            parent.insert(Value::String("issue".into()), Value::String(v));
        }
        out.insert(Value::String("parent".into()), Value::Mapping(parent));
    }

    Value::Mapping(out)
}

/// The `x-bib` block for an imported papis document, carrying its files, tags
/// and any keys this mapping does not understand.
pub fn to_meta(papis: &Value) -> Value {
    let mut meta = Mapping::new();
    for (from, to) in [("files", "files"), ("tags", "tags"), ("notes", "notes")] {
        if let Some(v) = papis.get(from) {
            meta.insert(Value::String(to.into()), normalize_list(v, to != "notes"));
        }
    }

    // Anything unmapped is kept rather than silently lost, so an import can be
    // audited and nothing is destroyed by a round trip.
    if let Some(map) = papis.as_mapping() {
        let leftovers: Mapping = map
            .iter()
            .filter(|(k, _)| k.as_str().is_some_and(|k| !MAPPED.contains(&k)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !leftovers.is_empty() {
            meta.insert(Value::String("papis".into()), Value::Mapping(leftovers));
        }
    }
    Value::Mapping(meta)
}

/// papis writes `tags` as either a list or a space-separated string.
fn normalize_list(v: &Value, split_strings: bool) -> Value {
    match v {
        Value::String(s) if split_strings => Value::Sequence(
            s.split_whitespace()
                .map(|t| Value::String(t.into()))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Prefer papis's structured `*_list` if present, since it separates given and
/// family names; otherwise fall back to the flat string, which hayagriva parses.
fn persons(papis: &Value, list_key: &str, flat_key: &str) -> Option<Value> {
    if let Some(Value::Sequence(list)) = papis.get(list_key) {
        let names: Vec<Value> = list
            .iter()
            .filter_map(|p| {
                let family = p.get("family").and_then(|v| v.as_str())?;
                match p.get("given").and_then(|v| v.as_str()) {
                    Some(given) => Some(Value::String(format!("{family}, {given}"))),
                    None => Some(Value::String(family.to_owned())),
                }
            })
            .collect();
        if !names.is_empty() {
            return Some(Value::Sequence(names));
        }
    }
    // papis joins multiple authors with " and ", as BibTeX does.
    let flat = papis.get(flat_key)?.as_str()?;
    let names: Vec<Value> = flat
        .split(" and ")
        .map(|n| Value::String(n.trim().to_owned()))
        .collect();
    (!names.is_empty()).then_some(Value::Sequence(names))
}

fn date(papis: &Value) -> Option<String> {
    let year = papis.get("year").and_then(|v| {
        v.as_i64()
            .map(|n| n.to_string())
            .or_else(|| v.as_str().map(str::to_owned))
    })?;
    match papis.get("month").and_then(|v| v.as_i64()) {
        Some(m) if (1..=12).contains(&m) => Some(format!("{year}-{m:02}")),
        _ => Some(year),
    }
}

/// papis uses BibTeX type names; hayagriva has its own vocabulary.
fn entry_type(papis_type: Option<&str>) -> String {
    match papis_type.unwrap_or("article") {
        "article" | "misc" => "article",
        "book" => "book",
        "inbook" | "incollection" => "chapter",
        "inproceedings" | "conference" => "article",
        "proceedings" => "proceedings",
        "phdthesis" | "mastersthesis" | "thesis" => "thesis",
        "techreport" | "report" => "report",
        "unpublished" => "manuscript",
        "online" | "electronic" | "www" => "web",
        "patent" => "patent",
        _ => "misc",
    }
    .to_owned()
}
