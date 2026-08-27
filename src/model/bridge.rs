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

use anyhow::{Context, Result, anyhow};
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
