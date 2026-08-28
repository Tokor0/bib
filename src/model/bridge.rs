//! The `info.yml` <-> hayagriva bridge.
//!
//! `info.yml` holds a hayagriva entry body plus a reserved [`META_KEY`] block.
//! Rather than mirroring hayagriva's schema in our own structs — a permanent
//! maintenance tax, since it has 30+ entry types and nested parents — we keep
//! the parsed YAML as the source of truth and let hayagriva do the validating.
//!
//! `Entry` deliberately does not implement `Deserialize` (its key lives outside
//! the entry body), but `Library` does. So converting one document means
//! wrapping its body in a one-entry mapping and deserializing that. Because we
//! use the same `serde_yaml` version hayagriva does, this goes through
//! `from_value` directly, with no string round-trip to introduce quoting or
//! type-inference drift.

use anyhow::{Context, Result, anyhow, bail};
use hayagriva::{Entry, Library};
use serde_yaml::{Mapping, Value};

/// Reserved key in `info.yml` holding data that is ours, not hayagriva's.
/// Chosen with an `x-` prefix so it reads as an extension.
pub const META_KEY: &str = "x-bib";

/// Parse one document body into a validated hayagriva [`Entry`].
///
/// `value` is the whole `info.yml` mapping; the [`META_KEY`] block is stripped
/// before handing it over, since hayagriva would reject the unknown field.
pub fn to_entry(citekey: &str, value: &Value) -> Result<Entry> {
    let mut body = value.clone();
    if let Value::Mapping(map) = &mut body {
        map.remove(META_KEY);
    }

    let mut wrapper = Mapping::with_capacity(1);
    wrapper.insert(Value::String(citekey.to_owned()), body);

    let library: Library = serde_yaml::from_value(Value::Mapping(wrapper))
        .with_context(|| format!("`{citekey}` is not a valid hayagriva entry"))?;

    library
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("`{citekey}` produced no entry"))
}

/// Serialize a hayagriva [`Entry`] back to an `info.yml` body.
///
/// The result excludes the cite key, which lives in the directory name and the
/// library index rather than in the file.
pub fn from_entry(entry: &Entry) -> Result<Value> {
    serde_yaml::to_value(entry).context("could not serialize entry")
}

/// Read the [`META_KEY`] block, if present.
pub fn meta_of(value: &Value) -> Option<&Value> {
    value.get(META_KEY)
}

/// Insert or replace the [`META_KEY`] block, creating the mapping if needed.
pub fn set_meta(value: &mut Value, meta: Value) -> Result<()> {
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("document body is not a mapping"))?;
    map.insert(Value::String(META_KEY.to_owned()), meta);
    Ok(())
}

/// Every field an exported entry may carry, as hayagriva spells it.
///
/// Taken from hayagriva 0.10's `entry!` macro, plus `parent`, which the macro
/// adds by hand. It exists so that a name nobody recognises is refused rather
/// than silently matching nothing: an exclusion that quietly does not apply is
/// a bibliography with the wrong content in it, discovered at proofreading
/// time. `type` is deliberately absent — see [`prune`].
pub const EXPORTABLE_FIELDS: &[&str] = &[
    "abstract",
    "affiliated",
    "archive",
    "archive-location",
    "author",
    "call-number",
    "chapter",
    "date",
    "edition",
    "editor",
    "genre",
    "issue",
    "language",
    "location",
    "note",
    "organization",
    "page-range",
    "page-total",
    "parent",
    "publisher",
    "runtime",
    "serial-number",
    "time-range",
    "title",
    "url",
    "volume",
    "volume-total",
];

/// Check that every name in `fields` is one an entry can actually have.
pub fn check_fields(fields: &[String]) -> Result<()> {
    for field in fields {
        if EXPORTABLE_FIELDS.contains(&field.as_str()) {
            continue;
        }
        // `type` is the one field whose removal produces an entry that will not
        // parse at all, so it gets its own answer rather than "no such field".
        if field == "type" {
            bail!("`type` cannot be excluded: an entry without one is not a bibliography entry");
        }
        bail!(
            "`{field}` is not an entry field\nhint: one of {}",
            EXPORTABLE_FIELDS.join(", ")
        );
    }
    Ok(())
}

/// Drop `fields` from a document body, parents included.
///
/// Applied on the way out of the library rather than to what is stored: the
/// record keeps everything it was given, and the bibliography carries only what
/// a reader of it needs. Parents are pruned too, so `--exclude abstract` means
/// the same thing wherever an abstract happens to sit.
pub fn prune(value: &Value, fields: &[String]) -> Value {
    let Value::Mapping(map) = value else {
        return value.clone();
    };
    let mut out = Mapping::with_capacity(map.len());
    for (key, child) in map {
        if key
            .as_str()
            .is_some_and(|name| fields.iter().any(|f| f == name))
        {
            continue;
        }
        let child = match child {
            Value::Mapping(_) => prune(child, fields),
            // A parent may be a single mapping or a list of them.
            Value::Sequence(items) => {
                Value::Sequence(items.iter().map(|item| prune(item, fields)).collect())
            }
            other => other.clone(),
        };
        out.insert(key.clone(), child);
    }
    Value::Mapping(out)
}

/// Collect documents into a hayagriva [`Library`], ready for `to_yaml_str`.
pub fn library_from<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Library {
    let mut library = Library::new();
    for entry in entries {
        library.push(entry);
    }
    library
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayagriva::types::EntryType;

    fn parse(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).expect("test fixture should be valid YAML")
    }

    const ARTICLE: &str = r#"
type: article
title: On the Electrodynamics of Moving Bodies
author: ["Einstein, Albert"]
date: 1905-06-30
page-range: 891-921
serial-number:
  doi: 10.1002/andp.19053221004
parent:
  type: periodical
  title: Annalen der Physik
  volume: 322
x-bib:
  files: [paper.pdf]
  tags: [relativity]
"#;

    #[test]
    fn converts_a_document_body_to_an_entry() {
        let entry = to_entry("einstein1905", &parse(ARTICLE)).unwrap();
        assert_eq!(entry.key(), "einstein1905");
        assert_eq!(entry.entry_type(), &EntryType::Article);
        assert_eq!(entry.date().unwrap().year, 1905);
        assert_eq!(entry.doi().unwrap(), "10.1002/andp.19053221004");
    }

    /// The reserved block must not reach hayagriva, which would reject it as an
    /// unknown field. This is the whole reason the bridge exists.
    #[test]
    fn meta_block_is_stripped_before_validation() {
        let value = parse(ARTICLE);
        assert!(
            meta_of(&value).is_some(),
            "fixture should carry a meta block"
        );
        // Would error on the unknown `x-bib` field if it were not removed.
        assert!(to_entry("einstein1905", &value).is_ok());
    }

    #[test]
    fn pruning_removes_a_field_and_leaves_the_entry_valid() {
        let value = parse(ARTICLE);
        let pruned = prune(&value, &["page-range".to_owned()]);
        assert!(pruned.get("page-range").is_none());
        let entry = to_entry("einstein1905", &pruned).expect("pruned body should still parse");
        assert_eq!(
            entry.title().unwrap().to_string(),
            value["title"].as_str().unwrap()
        );
        assert!(entry.page_range().is_none());
    }

    /// A field means the same thing wherever it sits, so a container's copy
    /// goes too — otherwise an excluded field reappears under `parent:`.
    #[test]
    fn pruning_reaches_into_parents() {
        let value = parse(
            "type: article\ntitle: X\nabstract: outer\n\
             parent:\n  type: periodical\n  title: Y\n  abstract: inner\n",
        );
        let pruned = prune(&value, &["abstract".to_owned()]);
        let yaml = serde_yaml::to_string(&pruned).unwrap();
        assert!(!yaml.contains("outer"), "{yaml}");
        assert!(!yaml.contains("inner"), "{yaml}");
        assert!(
            yaml.contains("title: Y"),
            "parent itself was removed:\n{yaml}"
        );
    }

    #[test]
    fn pruning_nothing_changes_nothing() {
        let value = parse(ARTICLE);
        assert_eq!(prune(&value, &[]), value);
    }

    /// A misspelled field would otherwise exclude nothing at all, and the first
    /// sign of it would be an unwanted field in a rendered bibliography.
    #[test]
    fn an_unknown_field_name_is_refused() {
        assert!(check_fields(&["abstract".to_owned(), "note".to_owned()]).is_ok());

        let err = check_fields(&["abstrct".to_owned()]).unwrap_err();
        assert!(format!("{err}").contains("abstrct"), "{err}");

        // Removing the type leaves something that is not an entry at all.
        let err = check_fields(&["type".to_owned()]).unwrap_err();
        assert!(format!("{err}").contains("type"), "{err}");
    }

    /// The list is transcribed from hayagriva, so it can drift on an upgrade.
    /// `has` answers for names hayagriva knows, which pins the transcription
    /// for every field a real entry carries.
    #[test]
    fn the_field_list_matches_hayagriva() {
        let entry = to_entry("einstein1905", &parse(ARTICLE)).unwrap();
        for field in ["title", "author", "date", "page-range", "serial-number"] {
            assert!(entry.has(field), "hayagriva does not know `{field}`");
            assert!(
                EXPORTABLE_FIELDS.contains(&field),
                "`{field}` is missing from EXPORTABLE_FIELDS"
            );
        }
        assert!(!entry.has("journal"), "`journal` is not a hayagriva field");
    }

    /// A body with no meta block is equally valid — the key is optional.
    #[test]
    fn meta_block_is_optional() {
        let value = parse("type: book\ntitle: Ulysses\nauthor: [\"Joyce, James\"]\n");
        assert!(meta_of(&value).is_none());
        assert_eq!(
            to_entry("joyce", &value).unwrap().entry_type(),
            &EntryType::Book
        );
    }

    #[test]
    fn round_trips_through_hayagriva_without_losing_fields() {
        let entry = to_entry("einstein1905", &parse(ARTICLE)).unwrap();
        let back = from_entry(&entry).unwrap();
        let again = to_entry("einstein1905", &back).unwrap();

        assert_eq!(entry, again, "entry -> value -> entry must be stable");
        assert_eq!(again.doi().unwrap(), "10.1002/andp.19053221004");
        assert_eq!(
            again
                .parents()
                .first()
                .unwrap()
                .title()
                .unwrap()
                .to_string(),
            "Annalen der Physik"
        );
    }

    #[test]
    fn invalid_entry_type_is_rejected_with_the_key_named() {
        let err = to_entry("bad", &parse("type: notathing\ntitle: X\n")).unwrap_err();
        assert!(
            format!("{err:#}").contains("bad"),
            "error should name the key: {err:#}"
        );
    }

    #[test]
    fn builds_a_library_for_export() {
        let a = to_entry("einstein1905", &parse(ARTICLE)).unwrap();
        let b = Entry::new("knuth1997", EntryType::Book);
        let library = library_from([&a, &b]);
        assert_eq!(library.len(), 2);

        let yaml = hayagriva::io::to_yaml_str(&library).unwrap();
        assert!(yaml.contains("einstein1905"));
        assert!(yaml.contains("knuth1997"));
        // Exported YAML must be free of our extension key so Typst can read it.
        assert!(
            !yaml.contains(META_KEY),
            "export leaked {META_KEY}:\n{yaml}"
        );
    }
}
