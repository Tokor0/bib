//! The search index: SQLite + FTS5.
//!
//! **The index is disposable.** Every row is derived from an `info.yml`, and
//! `bib index --rebuild` reconstructs the whole thing from the files. No state
//! lives only here, so a corrupt or stale database is a performance problem
//! rather than data loss — which is why a schema mismatch simply drops
//! everything and starts over instead of migrating.

pub mod query;
pub mod sql;

use crate::model::Document;
use crate::store::Store;
use crate::util::fnv1a;
use anyhow::{Context, Result};
use hayagriva::Entry;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use query::Query;

/// The schema identity, derived from the DDL itself rather than maintained by
/// hand.
///
/// A hand-bumped constant is a standing invitation to change the schema and
/// forget (which is exactly what happened once here: `mtime` became `hash` and
/// every existing index kept claiming to be current, then failed on the first
/// query). Hashing the DDL makes the two impossible to get out of step. A
/// comment-only edit also invalidates the index, which costs one rebuild of a
/// cache that is disposable by design.
fn schema_version() -> i32 {
    (fnv1a(SCHEMA.as_bytes()) as i32).saturating_abs()
}

const SCHEMA: &str = r#"
CREATE TABLE documents (
    citekey      TEXT PRIMARY KEY,
    dir          TEXT NOT NULL,
    type         TEXT,
    title        TEXT,
    year         INTEGER,
    publisher    TEXT,
    parent_title TEXT,
    added        TEXT,
    -- Content fingerprint, so a reindex only parses what actually changed.
    hash         INTEGER NOT NULL,
    size         INTEGER NOT NULL
);

CREATE TABLE persons (
    citekey  TEXT NOT NULL REFERENCES documents(citekey) ON DELETE CASCADE,
    role     TEXT NOT NULL,
    position INTEGER NOT NULL,
    name     TEXT NOT NULL
);
CREATE INDEX persons_by_doc ON persons(citekey);

CREATE TABLE tags (
    citekey TEXT NOT NULL REFERENCES documents(citekey) ON DELETE CASCADE,
    tag     TEXT NOT NULL
);
CREATE INDEX tags_by_doc ON tags(citekey);

CREATE TABLE files (
    citekey TEXT NOT NULL REFERENCES documents(citekey) ON DELETE CASCADE,
    path    TEXT NOT NULL
);
CREATE INDEX files_by_doc ON files(citekey);

-- First-class lookup columns, not just FTS text: milestone 5's duplicate
-- detection is an exact-match query on (kind, value) and must not table-scan.
CREATE TABLE serials (
    citekey TEXT NOT NULL REFERENCES documents(citekey) ON DELETE CASCADE,
    kind    TEXT NOT NULL,
    value   TEXT NOT NULL
);
CREATE INDEX serials_by_doc ON serials(citekey);
CREATE UNIQUE INDEX serials_lookup ON serials(kind, value, citekey);

CREATE VIRTUAL TABLE fts USING fts5(citekey UNINDEXED, text);
"#;

pub struct Index {
    conn: Connection,
    path: PathBuf,
}

/// What a sync actually did, so callers can report it honestly.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// Documents that could not be parsed. They are left out of the index but
    /// never silently: `bib doctor` and `--problems` surface them.
    pub failed: Vec<(PathBuf, anyhow::Error)>,
}

impl SyncReport {
    pub fn changed(&self) -> bool {
        self.indexed > 0 || self.removed > 0
    }
}

impl Index {
    /// Open (creating if needed) the index for a library.
    pub fn open(store: &Store) -> Result<Self> {
        let state = store.library.state_dir();
        std::fs::create_dir_all(&state)
            .with_context(|| format!("could not create {}", state.display()))?;
        let path = state.join("index.sqlite");

        let conn = Connection::open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;

        let mut index = Self { conn, path };
        index.ensure_schema()?;
        Ok(index)
    }

    /// Open an in-memory index. Used by tests, and by `--no-index` runs that
    /// still want the query language.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut index = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        index.ensure_schema()?;
        Ok(index)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_schema(&mut self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == schema_version() {
            return Ok(());
        }
        // Disposable by design: rebuilding is cheaper to write and to reason
        // about than a migration, and cannot corrupt anything.
        self.reset()
    }

    /// Drop everything and recreate the schema.
    pub fn reset(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        for table in ["fts", "serials", "files", "tags", "persons", "documents"] {
            tx.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
        }
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", schema_version())?;
        tx.commit()?;
        Ok(())
    }

    /// Bring the index in line with the files, parsing only what changed.
    pub fn sync(&mut self, store: &Store) -> Result<SyncReport> {
        let paths = store.document_paths()?;
        let known = self.fingerprints()?;
        let mut report = SyncReport::default();

        let tx = self.conn.transaction()?;
        let mut seen = Vec::with_capacity(paths.len());

        for path in &paths {
            seen.push(path.citekey.clone());
            let fingerprint = fingerprint(&path.info);
            if let (Some(current), Some(previous)) = (fingerprint, known.get(&path.citekey))
                && current == *previous
            {
                report.unchanged += 1;
                continue;
            }

            match store.load(&path.dir) {
                Ok(doc) => {
                    let (hash, size) = fingerprint.unwrap_or((0, 0));
                    insert_document(&tx, &doc, hash, size)?;
                    report.indexed += 1;
                }
                Err(e) => {
                    // A document that no longer parses must not keep a stale
                    // row: the index would then answer with data the file no
                    // longer contains.
                    delete_document(&tx, &path.citekey)?;
                    report.failed.push((path.info.clone(), e));
                }
            }
        }

        // Anything indexed but no longer on disk.
        let mut stale: Vec<String> = known.keys().cloned().collect();
        stale.retain(|k| !seen.contains(k));
        for citekey in &stale {
            delete_document(&tx, citekey)?;
            report.removed += 1;
        }

        tx.commit()?;
        Ok(report)
    }

    fn fingerprints(&self) -> Result<HashMap<String, (i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT citekey, hash, size FROM documents")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// Run a query, returning matching cite keys in a stable order.
    pub fn search(&self, query: &Query) -> Result<Vec<Hit>> {
        let compiled = sql::compile(query);
        let sql = format!(
            "SELECT d.citekey, d.dir, d.title, d.year, d.type \
             FROM documents d WHERE {} ORDER BY d.citekey",
            compiled.where_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(compiled.params), |r| {
            Ok(Hit {
                citekey: r.get(0)?,
                dir: PathBuf::from(r.get::<_, String>(1)?),
                title: r.get(2)?,
                year: r.get(3)?,
                entry_type: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Documents carrying a given serial number, for duplicate detection.
    pub fn by_serial(&self, kind: &str, value: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT citekey FROM serials WHERE kind = ? AND value = ?")?;
        let rows = stmt.query_map(params![kind, value], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// One search result, with enough to print a listing without touching disk.
#[derive(Debug, Clone)]
pub struct Hit {
    pub citekey: String,
    pub dir: PathBuf,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub entry_type: Option<String>,
}

/// A cheap content fingerprint of `info.yml`, or `None` when it cannot be read
/// — which forces a reparse rather than trusting a possibly stale row.
///
/// Hashing the bytes rather than stat'ing `(mtime, size)` costs one small read
/// per document and is *correct*: mtime has second granularity on some
/// filesystems, so two same-size writes in the same second are indistinguishable
/// by timestamp. Reading a few hundred bytes is still an order of magnitude
/// cheaper than the YAML parse and hayagriva validation it lets us skip.
fn fingerprint(info: &Path) -> Option<(i64, i64)> {
    let bytes = std::fs::read(info).ok()?;
    Some((fnv1a(&bytes) as i64, bytes.len() as i64))
}

fn delete_document(tx: &rusqlite::Transaction<'_>, citekey: &str) -> Result<()> {
    // FTS5 has no foreign keys, so its row is removed explicitly.
    tx.execute("DELETE FROM fts WHERE citekey = ?", params![citekey])?;
    tx.execute("DELETE FROM documents WHERE citekey = ?", params![citekey])?;
    Ok(())
}

fn insert_document(
    tx: &rusqlite::Transaction<'_>,
    doc: &Document,
    hash: i64,
    size: i64,
) -> Result<()> {
    delete_document(tx, &doc.citekey)?;

    let entry = doc.entry()?;
    let meta = doc.meta();
    let title = entry.title().map(|t| t.to_string());
    let parent = entry.parents().first();
    let parent_title = parent.and_then(|p| p.title()).map(|t| t.to_string());
    let publisher = entry
        .publisher()
        .and_then(publisher_name)
        .or_else(|| parent.and_then(|p| p.publisher()).and_then(publisher_name));

    tx.execute(
        "INSERT INTO documents \
         (citekey, dir, type, title, year, publisher, parent_title, added, hash, size) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            doc.citekey,
            doc.dir.to_string_lossy(),
            entry_type_name(&entry),
            title,
            entry.date().map(|d| d.year as i64),
            publisher,
            parent_title,
            meta.added,
            hash,
            size,
        ],
    )?;

    let mut people = Vec::new();
    for (role, list) in [("author", entry.authors()), ("editor", entry.editors())] {
        for (position, person) in list.unwrap_or_default().iter().enumerate() {
            let name = person.name_first(true, false);
            tx.execute(
                "INSERT INTO persons (citekey, role, position, name) VALUES (?, ?, ?, ?)",
                params![doc.citekey, role, position as i64, name],
            )?;
            people.push(name);
        }
    }

    for tag in &meta.tags {
        tx.execute(
            "INSERT INTO tags (citekey, tag) VALUES (?, ?)",
            params![doc.citekey, tag],
        )?;
    }

    for file in &meta.files {
        tx.execute(
            "INSERT INTO files (citekey, path) VALUES (?, ?)",
            params![doc.citekey, file.to_string_lossy()],
        )?;
    }

    for (kind, value) in serials(&entry) {
        // A document can legitimately repeat a serial number across parents;
        // the unique index makes that idempotent rather than an error.
        tx.execute(
            "INSERT OR IGNORE INTO serials (citekey, kind, value) VALUES (?, ?, ?)",
            params![doc.citekey, kind, value],
        )?;
    }

    let mut haystack = vec![doc.citekey.clone()];
    haystack.extend(title);
    haystack.extend(parent_title);
    haystack.extend(people);
    haystack.extend(meta.tags.iter().cloned());
    haystack.extend(entry.abstract_().map(|a| a.to_string()));
    tx.execute(
        "INSERT INTO fts (citekey, text) VALUES (?, ?)",
        params![doc.citekey, haystack.join(" ")],
    )?;

    Ok(())
}

/// Serial numbers, flattened from the entry and its parents.
fn serials(entry: &Entry) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |kind: &str, value: Option<String>| {
        if let Some(v) = value {
            let v = v.trim().to_lowercase();
            if !v.is_empty() {
                out.push((kind.to_owned(), v));
            }
        }
    };
    push("doi", entry.doi().map(str::to_owned));
    push("arxiv", entry.arxiv().map(str::to_owned));
    push("isbn", entry.isbn().map(str::to_owned));
    push("issn", entry.issn().map(str::to_owned));
    push("pmid", entry.pmid().map(str::to_owned));
    for parent in entry.parents() {
        push("isbn", parent.isbn().map(str::to_owned));
        push("issn", parent.issn().map(str::to_owned));
    }
    out
}

/// `EntryType` has no `Display`, only `Serialize`, so the serialized form is
/// the one name that stays in step with the schema.
fn entry_type_name(entry: &Entry) -> Option<String> {
    serde_yaml::to_value(entry.entry_type())
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
}

/// `Publisher` is a name plus an optional location and has no `Display`; only
/// the name is worth indexing, since the location is not what people search by.
fn publisher_name(publisher: &hayagriva::types::Publisher) -> Option<String> {
    publisher.name().map(|n| n.to_string())
}
