//! Typst conformance.
//!
//! This test does **not** exist to produce a PDF — the deliverable is the
//! `.yml`, and rendering a document is the user's build step, not ours. It
//! exists because two properties are observable only by running Typst, and
//! neither is implied by our own hayagriva round-trip:
//!
//! 1. **Every cite key we generate is resolvable as `@key`.** Parsing with
//!    hayagriva cannot show this: hayagriva accepts any string as a mapping
//!    key, while Typst's lexer does not. `src/model/label.rs` encodes our
//!    reading of that lexer rule; this is the check that the reading is right.
//! 2. **Version skew.** Typst bundles its own hayagriva. A future release
//!    tightening the schema would break our export while our vendored
//!    `hayagriva` still accepts it — visible only here.
//!
//! Skipped when `typst` is absent so `cargo test` works outside the Nix shell;
//! the flake check sets `BIBTEST_REQUIRE_TYPST` so it cannot pass vacuously.

use bib::config::CitekeyConfig;
use bib::formats::{ExportFormat, bibtex, export};
use bib::model::{Document, bridge, citekey::KeyMaker, label};
use std::process::Command;
use unicode_normalization::UnicodeNormalization;

/// Deliberately awkward: an en-dash page range, a `²` in a title, a `Dr.`
/// whose period would land at the end of a key, and a non-ASCII surname.
const SAMPLE_BIB: &str = r#"
@article{a,
  author  = {Einstein, Albert},
  title   = {Zur Elektrodynamik bewegter K{\"o}rper},
  journal = {Annalen der Physik},
  volume  = {322},
  number  = {10},
  pages   = {891--921},
  year    = {1905},
  doi     = {10.1002/andp.19053221004}
}
@inproceedings{b,
  author    = {Vaswani, Ashish and Shazeer, Noam},
  title     = {Attention Is All You Need},
  booktitle = {Advances in Neural Information Processing Systems},
  year      = {2017}
}
@book{c,
  author    = {Knuth, Donald E.},
  title     = {The Art of Computer Programming},
  publisher = {Addison-Wesley},
  address   = {Reading, MA},
  year      = {1997}
}
@article{d,
  author  = {Nakagawa, Shinichi},
  title   = {R² Statistics for Mixed Models},
  journal = {Methods in Ecology and Evolution},
  year    = {2013}
}
@article{e,
  author  = {M{\"u}ller, Hans},
  title   = {Dr. Strangelove Reconsidered},
  journal = {Film Quarterly},
  year    = {1964}
}
"#;

fn typst_available() -> bool {
    Command::new("typst")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Keys are generated the way `bib add` generates them, with a template that
/// preserves unicode — the configuration under which the label rule actually
/// has to do work.
fn documents() -> Vec<Document> {
    let config = CitekeyConfig {
        templates: vec![
            "{{ author[0].family | lower }}{{ date.year }}\
             {{ title | nostop | words(1) | lower }}"
                .to_owned(),
        ],
        ..CitekeyConfig::default()
    };
    let maker = KeyMaker::new(&config);

    bibtex::from_bibtex(SAMPLE_BIB)
        .expect("sample should parse")
        .into_iter()
        .map(|e| {
            let citekey = maker.render(&e).expect("every fixture should render a key");
            assert!(
                label::is_valid(&citekey),
                "generated key {citekey:?} is not a valid Typst label"
            );
            Document {
                citekey,
                dir: std::path::PathBuf::from("/nonexistent"),
                value: bridge::from_entry(&e).expect("entry should serialize"),
            }
        })
        .collect()
}

#[test]
fn every_generated_cite_key_resolves_in_typst() {
    if !typst_available() {
        assert!(
            std::env::var_os("BIBTEST_REQUIRE_TYPST").is_none(),
            "BIBTEST_REQUIRE_TYPST is set but `typst` is not on PATH"
        );
        eprintln!("skipping: `typst` is not on PATH");
        return;
    }

    let docs = documents();
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("bibliography.yml"),
        export(&docs, ExportFormat::Hayagriva).unwrap(),
    )
    .unwrap();

    // Every key is cited, so a key Typst cannot lex — or an entry it cannot
    // resolve — fails the build. Two details make this catch more than the
    // happy path:
    //   * each reference is followed by a period, the case that catches a key
    //     ending in punctuation `ref_marker` would strip;
    //   * the citation is written in NFC, as a user's editor would emit it.
    //     Typst compares labels bytewise, so a decomposed key would not match
    //     even though the two render identically.
    let citations: String = docs
        .iter()
        .map(|d| format!("@{}.\n\n", d.citekey.nfc().collect::<String>()))
        .collect();
    let doc = temp.path().join("paper.typ");
    std::fs::write(
        &doc,
        format!("= Test\n\n{citations}#bibliography(\"bibliography.yml\")\n"),
    )
    .unwrap();

    // The PDF is a byproduct we never look at; only the exit status and a
    // silent stderr are the assertions.
    let output = Command::new("typst")
        .args([
            "compile",
            doc.to_str().unwrap(),
            temp.path().join("out.pdf").to_str().unwrap(),
        ])
        .output()
        .expect("typst should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "typst rejected the exported bibliography:\n{stderr}"
    );
    // An unresolved `@key` is an error, but a schema drift Typst merely warns
    // about is exactly the version-skew signal this test is here to catch.
    assert!(
        stderr.trim().is_empty(),
        "typst compiled with diagnostics:\n{stderr}"
    );
}
