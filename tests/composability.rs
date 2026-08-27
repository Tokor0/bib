//! The contract a launcher plugin is written against.
//!
//! These drive the real binary rather than library functions, because the
//! properties being checked are properties of the *process*: what lands on
//! stdout, what lands on stderr, and what the exit code says. A regression in
//! any of them is invisible to every other test in this repository and fatal to
//! every consumer.

use httpmock::prelude::*;
use serde::Deserialize;
use std::process::{Command, Output};

/// The fields a launcher maps onto its own result type. Deserializing *both*
/// `bib search --json` and `bib find --json` into this one struct is the
/// property that lets a plugin keep a single field mapping.
#[derive(Debug, Deserialize)]
// Fields exist to assert the schema, not to be read: `deny_unknown_fields`
// plus this declaration is the test. A field added to `SearchResult` without
// being added here fails every case in this file, which is the intent — the
// contract should not drift silently.
#[allow(dead_code)]
#[serde(deny_unknown_fields)]
struct LauncherRow {
    id: String,
    source: String,
    #[serde(default)]
    citekey: Option<String>,
    #[serde(default)]
    cite: Option<String>,
    title: String,
    subtitle: String,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    in_library: bool,
}

struct Library {
    _temp: tempfile::TempDir,
    config: std::path::PathBuf,
    dir: std::path::PathBuf,
}

fn library(provider_base: Option<&str>) -> Library {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("lib");
    std::fs::create_dir_all(&dir).unwrap();

    let mut config = format!(
        "default_library = \"t\"\n[libraries.t]\ndir = {:?}\n",
        dir.to_string_lossy()
    );
    if let Some(base) = provider_base {
        // One provider, pointed at the mock: these tests must never touch the
        // network.
        config.push_str(&format!(
            "[providers]\norder = [\"crossref\"]\n[providers.crossref]\nbase_url = {base:?}\n"
        ));
    }
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

const EINSTEIN: &str = r#"
type: article
title: Zur Elektrodynamik bewegter Körper
author: ["Einstein, Albert"]
date: 1905
serial-number: {doi: 10.1002/andp.19053221004}
parent: {type: periodical, title: Annalen der Physik}
"#;

/// The single most important property: a plugin writes one mapping.
#[test]
fn local_and_web_results_share_one_schema() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(
            r#"{"message":{"items":[{
                 "type":"journal-article",
                 "title":["Zur Elektrodynamik bewegter Körper"],
                 "container-title":["Annalen der Physik"],
                 "author":[{"given":"A.","family":"Einstein"}],
                 "issued":{"date-parts":[[1905,1]]},
                 "DOI":"10.1002/andp.19053221004"}]}}"#,
        );
    });

    let lib = library(Some(&server.base_url()));
    add_document(&lib, "einstein1905zur", EINSTEIN);

    let local = bib(&lib, &["search", "einstein", "--json"]);
    assert!(local.status.success(), "search failed: {}", stderr(&local));
    let local_rows: Vec<LauncherRow> = serde_json::from_slice(&local.stdout).unwrap_or_else(|e| {
        panic!(
            "search --json is not the shared schema: {e}\n{}",
            stdout(&local)
        )
    });

    let web = bib(
        &lib,
        &["find", "Zur Elektrodynamik bewegter Körper", "--json"],
    );
    assert!(web.status.success(), "find failed: {}", stderr(&web));
    let web_rows: Vec<LauncherRow> = serde_json::from_slice(&web.stdout).unwrap_or_else(|e| {
        panic!(
            "find --json is not the shared schema: {e}\n{}",
            stdout(&web)
        )
    });

    assert_eq!(local_rows.len(), 1);
    assert_eq!(web_rows.len(), 1);
    assert_eq!(local_rows[0].source, "library");
    assert_eq!(web_rows[0].source, "crossref");
    // The same paper from both sides carries the same handle, which is how a
    // caller spots the overlap without comparing titles.
    assert_eq!(local_rows[0].id, web_rows[0].id);
    // A library row can be acted on with no further process spawn.
    assert_eq!(local_rows[0].cite.as_deref(), Some("@einstein1905zur"));
    assert!(local_rows[0].in_library);
}

/// A found document already in the library must say so, or a launcher offers
/// to add what is already there.
#[test]
fn a_web_result_already_in_the_library_is_marked() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(
            r#"{"message":{"items":[{"type":"journal-article",
                 "title":["Zur Elektrodynamik bewegter Körper"],
                 "DOI":"10.1002/andp.19053221004"}]}}"#,
        );
    });
    let lib = library(Some(&server.base_url()));
    add_document(&lib, "einstein1905zur", EINSTEIN);
    // Populate the index, which is what the marker consults.
    bib(&lib, &["index"]);

    let out = bib(&lib, &["find", "Zur Elektrodynamik", "--json"]);
    let rows: Vec<LauncherRow> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(rows[0].in_library, "the marker did not fire: {rows:?}");
    assert_eq!(rows[0].cite.as_deref(), Some("@einstein1905zur"));
}

/// Progress and warnings belong on stderr. A single stray `println!` upstream
/// would break every JSON consumer, and nothing else would notice.
#[test]
fn machine_readable_stdout_carries_only_the_payload() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(r#"{"message":{"items":[]}}"#);
    });
    let lib = library(Some(&server.base_url()));
    add_document(&lib, "einstein1905zur", EINSTEIN);

    for args in [
        vec!["search", "einstein", "--json"],
        vec!["find", "anything", "--json"],
    ] {
        let out = bib(&lib, &args);
        let text = stdout(&out);
        serde_json::from_str::<serde_json::Value>(&text).unwrap_or_else(|e| {
            panic!(
                "`bib {}` put non-JSON on stdout: {e}\n{text}",
                args.join(" ")
            )
        });
        // The diagnostics still happen — they are simply on the other stream.
        assert!(
            !text.contains("crossref:"),
            "provider notes leaked onto stdout:\n{text}"
        );
    }
}

/// "Nothing matched" is an answer, not a failure. A plugin must be able to tell
/// it from "bib is misconfigured" by exit code alone.
#[test]
fn zero_results_is_success_with_an_empty_array() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(r#"{"message":{"items":[]}}"#);
    });
    let lib = library(Some(&server.base_url()));

    for args in [
        vec!["search", "nothingmatchesthis", "--json"],
        vec!["find", "nothingmatchesthis", "--json"],
    ] {
        let out = bib(&lib, &args);
        assert!(
            out.status.success(),
            "`bib {}` exited {:?} for an empty result",
            args.join(" "),
            out.status.code()
        );
        assert_eq!(stdout(&out).trim(), "[]");
    }
}

/// A genuine failure must be distinguishable from an empty result.
#[test]
fn a_real_error_exits_nonzero() {
    let lib = library(None);
    let out = bib(&lib, &["search", "year:notayear", "--json"]);
    assert!(!out.status.success(), "a bad query should not exit 0");
    assert!(stdout(&out).trim().is_empty(), "an error wrote to stdout");
}

/// `wofi` and friends treat one line as one item.
#[test]
fn format_output_is_one_line_per_result() {
    let lib = library(None);
    add_document(
        &lib,
        "multiline",
        "type: article\ntitle: \"A title\\nwith a newline\"\ndate: 2020\n",
    );
    add_document(
        &lib,
        "plain",
        "type: article\ntitle: Ordinary\ndate: 2021\n",
    );

    // A literal backslash and `t`, exactly as a shell hands it over — not
    // Rust's "\t", which would already be a tab and prove nothing.
    let out = bib(&lib, &["list", "--format", r"{{ id }}\t{{ title }}"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "a title newline split a row:\n{text}");
    for line in lines {
        assert!(
            line.contains('\t'),
            "missing the id/title separator: {line:?}"
        );
    }
}

/// The wofi recipe: a line carries its own id, so the selection maps back to an
/// action without re-running the search.
#[test]
fn a_formatted_line_carries_the_id_needed_to_act_on_it() {
    let lib = library(None);
    add_document(&lib, "einstein1905zur", EINSTEIN);

    let out = bib(
        &lib,
        &["list", "--format", r"{{ id }}\t{{ cite }}\t{{ title }}"],
    );
    let text = stdout(&out);
    let mut fields = text.trim().split('\t');
    assert_eq!(fields.next(), Some("doi:10.1002/andp.19053221004"));
    assert_eq!(fields.next(), Some("@einstein1905zur"));
}

/// `--keys` is the simplest pipeline form and must stay pure.
#[test]
fn keys_output_is_bare_cite_keys() {
    let lib = library(None);
    add_document(&lib, "einstein1905zur", EINSTEIN);
    let out = bib(&lib, &["list", "--keys"]);
    assert_eq!(stdout(&out).trim(), "einstein1905zur");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
