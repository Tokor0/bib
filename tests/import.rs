//! Bringing a library in from papis — the documents as well as the metadata.
//!
//! A papis library is a directory per document holding an `info.yaml` *and the
//! PDF*. Importing only the first half leaves records that name attachments the
//! library does not have, which is worse than recording none: `bib fetch` reads
//! that list, reports "already has an attachment", and skips exactly the
//! documents that are missing one.

use std::process::{Command, Output};

struct Library {
    _temp: tempfile::TempDir,
    config: std::path::PathBuf,
    dir: std::path::PathBuf,
    source: std::path::PathBuf,
}

fn library() -> Library {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("lib");
    let source = temp.path().join("papis");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let config_path = temp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "default_library = \"t\"\n[libraries.t]\ndir = {:?}\n",
            dir.to_string_lossy()
        ),
    )
    .unwrap();

    Library {
        dir,
        source,
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

/// One papis document: an `info.yaml` naming `attachment`, and that file on
/// disk only if `present`.
fn papis_document(library: &Library, folder: &str, attachment: &str, present: bool) {
    let dir = library.source.join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("info.yaml"),
        format!(
            "ref: {folder}\ntype: article\ntitle: Random Quantum Circuits\n\
             author: Harrow, Aram W.\nyear: '2009'\nfiles:\n- {attachment}\n"
        ),
    )
    .unwrap();
    if present {
        std::fs::write(dir.join(attachment), b"%PDF-1.7\nnot really a paper\n").unwrap();
    }
}

fn imported_info(library: &Library, citekey: &str) -> String {
    std::fs::read_to_string(library.dir.join(citekey).join("info.yml"))
        .expect("the document should have been created")
}

#[test]
fn a_papis_import_brings_the_files_with_it() {
    let lib = library();
    papis_document(&lib, "harrow-2009-random", "harrow-2009-random.pdf", true);

    let out = bib(&lib, &["import", "papis", &lib.source.to_string_lossy()]);
    assert!(
        out.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let copied = lib
        .dir
        .join("harrow-2009-random")
        .join("harrow-2009-random.pdf");
    assert!(copied.is_file(), "the PDF was not copied to {copied:?}");
    assert!(
        std::fs::read(&copied).unwrap().starts_with(b"%PDF-"),
        "the copy is not the file that was there"
    );
    assert!(imported_info(&lib, "harrow-2009-random").contains("harrow-2009-random.pdf"));

    // The source library is left exactly as it was: this is a copy, and the
    // user's old library stays usable until they decide otherwise.
    assert!(
        lib.source
            .join("harrow-2009-random")
            .join("harrow-2009-random.pdf")
            .is_file()
    );
}

/// The case that produced silent breakage: a record naming a file the source no
/// longer has. The name must not come across, or the document looks complete
/// and `bib fetch` will pass it over.
#[test]
fn a_file_the_source_does_not_have_is_dropped_from_the_record() {
    let lib = library();
    papis_document(&lib, "gone-2020-missing", "gone-2020-missing.pdf", false);

    let out = bib(&lib, &["import", "papis", &lib.source.to_string_lossy()]);
    assert!(
        out.status.success(),
        "a missing file must not fail the import"
    );

    let info = imported_info(&lib, "gone-2020-missing");
    assert!(
        !info.contains("gone-2020-missing.pdf"),
        "the record still claims a file it does not have:\n{info}"
    );
    // Silently is not good enough — the user has to know the PDF did not come.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gone-2020-missing.pdf"), "{stderr}");
}

#[test]
fn a_dry_run_copies_nothing() {
    let lib = library();
    papis_document(&lib, "harrow-2009-random", "harrow-2009-random.pdf", true);

    let out = bib(
        &lib,
        &[
            "import",
            "papis",
            &lib.source.to_string_lossy(),
            "--dry-run",
        ],
    );
    assert!(out.status.success());
    assert!(
        !lib.dir.join("harrow-2009-random").exists(),
        "a dry run wrote to the library"
    );
}
