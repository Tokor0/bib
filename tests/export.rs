//! What ends up in the bibliography file.
//!
//! Driven through the binary because the property being checked is a
//! *configured* default: `export.exclude` reaching the rendered file is the
//! whole feature, and library-level tests pass the list in by hand, so they
//! cannot see it. The abstract is the case that matters — no citation style
//! renders one, and for a library of a few dozen papers it is most of the file.

use std::process::{Command, Output};

struct Library {
    _temp: tempfile::TempDir,
    config: std::path::PathBuf,
    dir: std::path::PathBuf,
}

/// A library plus a config file, with `settings` appended verbatim so a test
/// can state the export configuration it is about.
fn library(settings: &str) -> Library {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("lib");
    std::fs::create_dir_all(&dir).unwrap();

    let config = format!(
        "default_library = \"t\"\n[libraries.t]\ndir = {:?}\n{settings}",
        dir.to_string_lossy()
    );
    let config_path = temp.path().join("config.toml");
    std::fs::write(&config_path, config).unwrap();

    Library {
        dir,
        config: config_path,
        _temp: temp,
    }
}

fn bib(library: &Library, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bib"))
        .args(args)
        .env("BIB_CONFIG", &library.config)
        .output()
        .expect("bib should run")
}

fn add_document(library: &Library, citekey: &str, yaml: &str) {
    let dir = library.dir.join(citekey);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("info.yml"), yaml).unwrap();
}

const HARROW: &str = r#"
type: article
title: Random Quantum Circuits are Approximate 2-designs
author: ["Harrow, Aram W."]
date: 2009
abstract: Given a universal gate set on two qubits, it is well known that…
note: arXiv:0802.1919 [quant-ph]
page-range: 257-302
serial-number: {doi: 10.1007/s00220-009-0873-6}
parent: {type: periodical, title: Communications in Mathematical Physics, volume: 291}
"#;

fn export(library: &Library, args: &[&str]) -> String {
    let mut argv = vec!["export"];
    argv.extend_from_slice(args);
    let out = bib(library, &argv);
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_abstract_is_left_out_by_default() {
    let lib = library("");
    add_document(&lib, "harrow2009random", HARROW);

    let yaml = export(&lib, &[]);
    assert!(!yaml.contains("abstract"), "{yaml}");
    // Everything a citation needs is still there.
    assert!(yaml.contains("Random Quantum Circuits"), "{yaml}");
    assert!(yaml.contains("10.1007/s00220-009-0873-6"), "{yaml}");
    assert!(
        yaml.contains("Communications in Mathematical Physics"),
        "{yaml}"
    );
    assert!(yaml.contains("257-302"), "{yaml}");
}

#[test]
fn all_fields_brings_the_excluded_ones_back() {
    let lib = library("");
    add_document(&lib, "harrow2009random", HARROW);

    let yaml = export(&lib, &["--all-fields"]);
    assert!(yaml.contains("abstract"), "{yaml}");
}

/// Configuration is where a project states this once, rather than every
/// invocation having to remember it.
#[test]
fn configuration_decides_what_is_dropped() {
    let lib = library("[export]\nexclude = [\"abstract\", \"note\", \"url\"]\n");
    add_document(&lib, "harrow2009random", HARROW);

    let yaml = export(&lib, &[]);
    assert!(!yaml.contains("abstract"), "{yaml}");
    assert!(!yaml.contains("arXiv:0802.1919"), "{yaml}");
    assert!(yaml.contains("Harrow"), "{yaml}");
}

/// An empty list is how a user says "everything", and it must not fall back to
/// the default.
#[test]
fn an_empty_exclude_list_exports_everything() {
    let lib = library("[export]\nexclude = []\n");
    add_document(&lib, "harrow2009random", HARROW);

    assert!(export(&lib, &[]).contains("abstract"));
}

#[test]
fn the_flag_replaces_the_configured_list() {
    let lib = library("[export]\nexclude = [\"abstract\"]\n");
    add_document(&lib, "harrow2009random", HARROW);

    let yaml = export(&lib, &["--exclude", "note"]);
    assert!(
        yaml.contains("abstract"),
        "the flag should replace, not add:\n{yaml}"
    );
    assert!(!yaml.contains("arXiv:0802.1919"), "{yaml}");
}

/// A misspelled field would otherwise silently exclude nothing, and the first
/// sign of it would be the field appearing in a rendered bibliography.
#[test]
fn a_field_that_does_not_exist_is_an_error() {
    let lib = library("");
    add_document(&lib, "harrow2009random", HARROW);

    let out = bib(&lib, &["export", "--exclude", "abstrct"]);
    assert!(!out.status.success(), "a typo should not be a silent no-op");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("abstrct"), "{stderr}");
    assert!(
        stderr.contains("abstract"),
        "the hint should list the real fields: {stderr}"
    );
    assert!(out.stdout.is_empty(), "nothing should be written");
}

/// BibTeX is a bibliography too: what belongs in one does not change with the
/// file extension.
#[test]
fn the_exclusion_applies_to_bibtex_as_well() {
    let lib = library("");
    add_document(&lib, "harrow2009random", HARROW);

    let out = export(&lib, &["--format", "bibtex"]);
    assert!(out.contains("@article"), "{out}");
    assert!(!out.contains("abstract"), "{out}");
}
