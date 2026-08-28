//! Round-trip and mapping tests for the import/export layer.

use bib::config::ResolvedLibrary;
use bib::formats::{ExportFormat, bibtex, export, papis};
use bib::model::{Document, bridge};
use bib::store::Store;
use serde_yaml::Value;

const SAMPLE_BIB: &str = r#"
@article{einstein1905,
  author  = {Einstein, Albert},
  title   = {Zur Elektrodynamik bewegter K{\"o}rper},
  journal = {Annalen der Physik},
  volume  = {322},
  number  = {10},
  pages   = {891--921},
  year    = {1905},
  doi     = {10.1002/andp.19053221004}
}
@inproceedings{vaswani2017,
  author    = {Vaswani, Ashish and Shazeer, Noam},
  title     = {Attention Is All You Need},
  booktitle = {Advances in Neural Information Processing Systems},
  year      = {2017}
}
@book{knuth1997,
  author    = {Knuth, Donald E.},
  title     = {The Art of Computer Programming},
  publisher = {Addison-Wesley},
  address   = {Reading, MA},
  year      = {1997},
  isbn      = {0201896834}
}
"#;

fn docs_from_bib(source: &str) -> Vec<Document> {
    bibtex::from_bibtex(source)
        .expect("sample should parse")
        .into_iter()
        .map(|e| Document {
            citekey: e.key().to_owned(),
            dir: std::path::PathBuf::from("/nonexistent"),
            value: bridge::from_entry(&e).expect("entry should serialize"),
        })
        .collect()
}

#[test]
fn bibtex_imports_and_exports_as_hayagriva() {
    let docs = docs_from_bib(SAMPLE_BIB);
    assert_eq!(docs.len(), 3);

    let yaml = export(&docs, ExportFormat::Hayagriva, &[]).expect("should export");

    // Containment is modelled as a parent entry, not a flat `journal` field.
    assert!(yaml.contains("parent:"), "expected nested parent:\n{yaml}");
    assert!(yaml.contains("Annalen der Physik"));
    assert!(yaml.contains("10.1002/andp.19053221004"));
    // The export must be free of our extension key or Typst will reject it.
    assert!(!yaml.contains("x-bib"), "export leaked x-bib:\n{yaml}");

    // Re-parsing with hayagriva proves the output is actually valid.
    let reparsed = hayagriva::io::from_yaml_str(&yaml).expect("export should be valid hayagriva");
    assert_eq!(reparsed.len(), 3);
}

/// BibTeX distinguishes a conference paper from a journal article by entry
/// type; hayagriva distinguishes them by parent. The mapping has to reconstruct
/// that, including using `booktitle` rather than `journal`.
#[test]
fn inproceedings_survives_a_bibtex_round_trip() {
    let docs = docs_from_bib(SAMPLE_BIB);
    let out = export(&docs, ExportFormat::Bibtex, &[]).expect("should export");

    assert!(out.contains("@inproceedings{vaswani2017"), "got:\n{out}");
    assert!(
        out.contains("booktitle = {Advances in Neural"),
        "got:\n{out}"
    );
    assert!(out.contains("@article{einstein1905"), "got:\n{out}");
    assert!(
        out.contains("journal = {Annalen der Physik}"),
        "got:\n{out}"
    );
}

/// hayagriva nests location under the publisher, so a naive mapping drops it.
#[test]
fn publisher_location_becomes_bibtex_address() {
    let docs = docs_from_bib(SAMPLE_BIB);
    let out = export(&docs, ExportFormat::Bibtex, &[]).expect("should export");
    assert!(
        out.contains("address = {Reading, MA}"),
        "address was dropped:\n{out}"
    );
}

#[test]
fn biblatex_uses_iso_dates_where_bibtex_uses_year() {
    let docs = docs_from_bib(SAMPLE_BIB);
    let bibtex_out = export(&docs, ExportFormat::Bibtex, &[]).expect("should export");
    let biblatex_out = export(&docs, ExportFormat::Biblatex, &[]).expect("should export");

    assert!(bibtex_out.contains("year = {1905}"));
    assert!(biblatex_out.contains("date = {1905}"));
    assert!(
        !biblatex_out.contains("year = {1905}"),
        "biblatex should prefer date"
    );
}

const PAPIS: &str = r#"
ref: turing1950computing
type: article
title: Computing Machinery and Intelligence
author: Turing, Alan M.
author_list:
  - given: Alan M.
    family: Turing
journal: Mind
volume: "59"
number: "236"
pages: 433-460
year: 1950
month: 10
doi: 10.1093/mind/LIX.236.433
tags: ai philosophy
files:
  - turing.pdf
my_custom_field: something papis-specific
"#;

#[test]
fn papis_maps_onto_the_nested_hayagriva_model() {
    let source: Value = serde_yaml::from_str(PAPIS).unwrap();
    let body = papis::to_body(&source);

    let entry = bridge::to_entry("turing1950", &body).expect("mapped body should be valid");
    assert_eq!(
        entry.title().unwrap().to_string(),
        "Computing Machinery and Intelligence"
    );
    assert_eq!(entry.doi().unwrap(), "10.1093/mind/LIX.236.433");

    // A flat `journal` becomes a parent periodical.
    let parent = entry
        .parents()
        .first()
        .expect("journal should become a parent");
    assert_eq!(parent.title().unwrap().to_string(), "Mind");

    // year + month combine into one date.
    let date = entry.date().unwrap();
    assert_eq!(date.year, 1950);
    assert_eq!(date.month, Some(9), "hayagriva months are zero-based");
}

/// An import that silently discarded unrecognised keys would destroy data on a
/// round trip, so they are kept rather than dropped.
#[test]
fn papis_preserves_keys_the_mapping_does_not_understand() {
    let source: Value = serde_yaml::from_str(PAPIS).unwrap();
    let meta = papis::to_meta(&source);

    let custom = meta.get("papis").and_then(|p| p.get("my_custom_field"));
    assert_eq!(
        custom.and_then(|v| v.as_str()),
        Some("something papis-specific")
    );

    // A space-separated tag string becomes a list.
    let tags = meta
        .get("tags")
        .and_then(|t| t.as_sequence())
        .expect("tags should be a list");
    assert_eq!(tags.len(), 2);
}

#[test]
fn store_creates_reads_and_removes_documents() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(ResolvedLibrary {
        name: "test".into(),
        dir: temp.path().to_path_buf(),
    });

    let body: Value =
        serde_yaml::from_str("type: book\ntitle: Ulysses\nauthor: [\"Joyce, James\"]\n").unwrap();
    let doc = store
        .create("joyce1922", std::path::Path::new("joyce1922"), &body)
        .unwrap();
    assert!(doc.info_path().is_file());

    let found = store.get("joyce1922").expect("should be found");
    assert_eq!(
        found.entry().unwrap().title().unwrap().to_string(),
        "Ulysses"
    );

    // Nested folder templates are discovered at any depth.
    store
        .create("kafka1925", std::path::Path::new("1925/kafka1925"), &body)
        .unwrap();
    assert_eq!(store.documents().unwrap().0.len(), 2);

    store.remove(&found).unwrap();
    assert!(store.get("joyce1922").is_err());
}

/// A document that fails to parse must not take the whole library down with it.
#[test]
fn a_malformed_document_is_reported_without_hiding_the_good_ones() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(ResolvedLibrary {
        name: "test".into(),
        dir: temp.path().to_path_buf(),
    });

    let good: Value = serde_yaml::from_str("type: book\ntitle: Fine\n").unwrap();
    store
        .create("good", std::path::Path::new("good"), &good)
        .unwrap();

    std::fs::create_dir_all(temp.path().join("broken")).unwrap();
    std::fs::write(
        temp.path().join("broken/info.yml"),
        "type: notarealtype\ntitle: Bad\n",
    )
    .unwrap();

    let (docs, errors) = store.documents().unwrap();
    assert_eq!(docs.len(), 1, "the good document should still load");
    assert_eq!(errors.len(), 1, "the broken one should be reported");
    assert!(errors[0].path.ends_with("info.yml"));
}

/// A cite key that Typst cannot reference must never reach the library.
///
/// The motivating case is real: DBLP exports keys like
/// `DBLP:conf/nips/VaswaniSPUJGKP17`, and `bib import bibtex` keeps source keys
/// unless `--rekey`. Before this check the slashes created nested directories
/// and `citekey_from_dir` read the key back as `VaswaniSPUJGKP17` — silently a
/// different entry from the one imported.
#[test]
fn the_store_refuses_keys_typst_cannot_reference() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(ResolvedLibrary {
        name: "test".into(),
        dir: temp.path().to_path_buf(),
    });
    let body: Value =
        serde_yaml::from_str("type: book\ntitle: Ulysses\nauthor: [\"Joyce, James\"]\n").unwrap();

    for bad in ["DBLP:conf/nips/Vaswani17", "smith2020.", "a b", ""] {
        let err = store
            .create(bad, std::path::Path::new("somewhere"), &body)
            .expect_err(&format!("{bad:?} should be rejected"));
        assert!(
            format!("{err:#}").contains("cite key"),
            "{bad:?} was rejected, but not as a cite-key problem: {err:#}"
        );
    }

    // Nothing was written for any of them.
    assert!(!temp.path().join("somewhere").exists());
    assert!(!temp.path().join("DBLP:conf").exists());
}
