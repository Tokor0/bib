//! `bib config` — inspect and modify configuration.

use crate::config;
use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print one setting, or the whole effective configuration.
    Get {
        /// Dotted key, e.g. `citekey.max_length` or `libraries.main.dir`.
        key: Option<String>,
    },
    /// Set a value in the user-level config file.
    Set {
        /// Dotted key, e.g. `libraries.main.dir`.
        key: String,
        /// Value, parsed as TOML where possible and otherwise as a string.
        value: String,
    },
    /// Print the paths configuration is read from.
    Path,
    /// Open the user-level config file in `$VISUAL` or `$EDITOR`.
    Edit,
}

impl ConfigAction {
    pub fn run(self, library: Option<&str>) -> Result<()> {
        match self {
            Self::Get { key } => get(library, key.as_deref()),
            Self::Set { key, value } => set(&key, &value),
            Self::Path => path(library),
            Self::Edit => edit(),
        }
    }
}

fn get(library: Option<&str>, key: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let doc = toml::Value::try_from(&loaded.config)
        .context("could not serialize the effective configuration")?;

    match key {
        None => print!("{}", toml::to_string_pretty(&doc)?),
        Some(key) => {
            let found =
                lookup(&doc, key).ok_or_else(|| anyhow!("no such configuration key: `{key}`"))?;
            println!("{}", render_scalar(found)?);
        }
    }
    Ok(())
}

/// Walk a dotted path through a TOML value.
fn lookup<'v>(root: &'v toml::Value, key: &str) -> Option<&'v toml::Value> {
    key.split('.')
        .try_fold(root, |current, segment| current.get(segment))
}

/// Render a looked-up value: scalars bare, tables and arrays as TOML.
fn render_scalar(v: &toml::Value) -> Result<String> {
    Ok(match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(_) | toml::Value::Array(_) => toml::to_string_pretty(v)
            .context("could not render value")?
            .trim_end()
            .to_owned(),
        other => other.to_string(),
    })
}

fn set(key: &str, raw: &str) -> Result<()> {
    let path = config::user_config_path()?;
    let mut doc = read_document(&path)?;

    let segments: Vec<&str> = key.split('.').collect();
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| anyhow!("empty configuration key"))?;
    if last.is_empty() || parents.iter().any(|s| s.is_empty()) {
        bail!("malformed configuration key: `{key}`");
    }

    // Descend, creating intermediate tables as needed.
    let mut table = doc.as_table_mut();
    for segment in parents {
        let entry = table
            .entry(segment)
            .or_insert_with(|| Item::Table(Table::new()));
        table = entry.as_table_mut().ok_or_else(|| {
            anyhow!("cannot set `{key}`: `{segment}` is already a value, not a table")
        })?;
    }
    table[last] = parse_value(raw);

    // Validate before writing: a config file that fails to load is worse than a
    // rejected `set`, since it breaks every subsequent command.
    validate(&doc, key)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("could not write {}", path.display()))?;
    println!("{key} = {raw}  ({})", path.display());
    Ok(())
}

/// Parse a value as TOML (so `3`, `true` and `["a", "b"]` keep their types),
/// falling back to a string for bare input like `~/Documents/library`.
fn parse_value(raw: &str) -> Item {
    let probe = format!("v = {raw}");
    match probe.parse::<DocumentMut>() {
        Ok(doc) => match doc.get("v") {
            Some(item) => item.clone(),
            None => value(raw),
        },
        Err(_) => value(raw),
    }
}

/// Re-parse an edited document through the real config model so typos and type
/// errors surface at `set` time rather than on the next command.
fn validate(doc: &DocumentMut, key: &str) -> Result<()> {
    config::validate_toml(&doc.to_string())
        .map(|_| ())
        .with_context(|| format!("`{key}` is not a valid setting"))
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .with_context(|| format!("{} is not valid TOML", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

fn path(library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let mark = |p: &Path| {
        if p.exists() {
            ""
        } else {
            "  (does not exist yet)"
        }
    };

    println!(
        "user     {}{}",
        loaded.user_path.display(),
        mark(&loaded.user_path)
    );
    match &loaded.library_path {
        Some(p) => println!("library  {}", p.display()),
        None => {
            let absent = loaded.library.config_path();
            println!("library  {}  (does not exist yet)", absent.display());
        }
    }
    println!(
        "root     {}{}",
        loaded.library.dir.display(),
        mark(&loaded.library.dir)
    );
    Ok(())
}

fn edit() -> Result<()> {
    let path = config::user_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let editor = std::env::var_os("VISUAL")
        .or_else(|| std::env::var_os("EDITOR"))
        .ok_or_else(|| anyhow!("neither $VISUAL nor $EDITOR is set"))?;

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("could not launch {}", editor.to_string_lossy()))?;
    if !status.success() {
        bail!("{} exited with {status}", editor.to_string_lossy());
    }
    Ok(())
}
