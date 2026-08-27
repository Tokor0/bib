//! Layered configuration.
//!
//! Precedence, lowest to highest:
//! built-in defaults -> `$XDG_CONFIG_HOME/bib/config.toml` -> `<library>/.bib/config.toml`
//! -> `BIB_*` environment -> CLI flags (applied by the caller).

mod model;

pub use model::*;

use anyhow::{Context, Result, anyhow};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use std::path::{Path, PathBuf};

/// Directory name used for per-library state inside a library root.
pub const LIBRARY_STATE_DIR: &str = ".bib";

/// The effective configuration plus the provenance needed by `bib config path`.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// User-level config file. May not exist yet; `bib config set` creates it.
    pub user_path: PathBuf,
    /// Per-library config file, if the resolved library has one on disk.
    pub library_path: Option<PathBuf>,
    pub library: ResolvedLibrary,
}

/// A library selected by name (or by the `default_library` setting).
#[derive(Debug, Clone)]
pub struct ResolvedLibrary {
    pub name: String,
    pub dir: PathBuf,
}

impl ResolvedLibrary {
    /// `<library>/.bib`, holding the index, caches and per-library config.
    pub fn state_dir(&self) -> PathBuf {
        self.dir.join(LIBRARY_STATE_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.state_dir().join("config.toml")
    }
}

/// Path to the user-level config file, honouring `BIB_CONFIG`.
pub fn user_config_path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("BIB_CONFIG") {
        return Ok(PathBuf::from(explicit));
    }
    let dirs = directories::ProjectDirs::from("", "", "bib")
        .ok_or_else(|| anyhow!("could not determine a config directory for this platform"))?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Load the effective configuration.
///
/// `library_override` corresponds to the global `--library` flag and wins over
/// `default_library`.
pub fn load(library_override: Option<&str>) -> Result<Loaded> {
    load_from(&user_config_path()?, library_override)
}

/// [`load`] against an explicit user-config path, so tests need not rely on
/// process-wide environment state.
pub fn load_from(user_path: &Path, library_override: Option<&str>) -> Result<Loaded> {
    // Pass 1: everything except the per-library file, which we cannot locate
    // until we know which library was selected.
    let partial: Config = base_figment(user_path)
        .extract()
        .with_context(|| format!("invalid configuration (see {})", user_path.display()))?;

    let library = resolve_library(&partial, library_override)?;
    let library_config = library.config_path();
    let library_path = library_config.exists().then(|| library_config.clone());

    // Pass 2: splice the per-library file in beneath the environment, so that
    // `BIB_*` still wins over it.
    let config: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(user_path))
        .merge(Toml::file(&library_config))
        .merge(env_provider())
        .extract()
        .with_context(|| format!("invalid configuration for library `{}`", library.name))?;

    Ok(Loaded {
        config,
        user_path: user_path.to_path_buf(),
        library_path,
        library,
    })
}

fn base_figment(user_path: &Path) -> Figment {
    Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(user_path))
        .merge(env_provider())
}

/// Check that a candidate config file is valid, without touching real state.
/// Used by `bib config set` so an invalid write is refused up front rather than
/// breaking every later command.
pub fn validate_toml(text: &str) -> Result<Config> {
    Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::string(text))
        .extract()
        .map_err(Into::into)
}

/// `BIB_PDF__OCR=never` sets `pdf.ocr`; the double underscore separates levels
/// because single underscores already appear inside key names.
///
/// Namespace rule: **every** `BIB_*` variable is read as a configuration key,
/// and `deny_unknown_fields` turns an unrecognised one into a hard failure of
/// every command. A variable that controls the process rather than naming a
/// setting must therefore either be listed in [`RESERVED_ENV`] or live outside
/// the `BIB_` prefix (test-only knobs use `BIBTEST_`).
fn env_provider() -> Env {
    Env::prefixed("BIB_").split("__").ignore(RESERVED_ENV)
}

/// `BIB_*` variables that steer the process instead of naming a setting.
/// See the namespace rule on [`env_provider`] before adding to this list.
pub const RESERVED_ENV: &[&str] = &["CONFIG", "LOG"];

/// Pick the library to operate on, expanding `~` in its configured directory.
pub fn resolve_library(config: &Config, override_name: Option<&str>) -> Result<ResolvedLibrary> {
    let name = override_name
        .map(str::to_owned)
        .or_else(|| config.default_library.clone())
        .unwrap_or_else(|| DEFAULT_LIBRARY_NAME.to_owned());

    let entry = config.libraries.get(&name).ok_or_else(|| {
        let known: Vec<&str> = config.libraries.keys().map(String::as_str).collect();
        if known.is_empty() {
            anyhow!(
                "no library named `{name}` is configured, and no libraries are defined.\n\
                 Define one with:  bib config set libraries.{name}.dir ~/Documents/library"
            )
        } else {
            anyhow!(
                "no library named `{name}` is configured (known: {})",
                known.join(", ")
            )
        }
    })?;

    Ok(ResolvedLibrary {
        name,
        dir: expand_tilde(&entry.dir),
    })
}

/// Expand a leading `~` or `~/…` against `$HOME`. Bare `~user` is left alone —
/// resolving other users' homes needs NSS and is not worth the dependency.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    // `figment::Jail`'s closure must return `Result<(), figment::Error>`, which
    // is large. The type is figment's, so the lint has no actionable fix here.
    #![allow(clippy::result_large_err)]

    use super::*;
    use figment::Jail;

    /// Write a library root with an optional `.bib/config.toml`, returning its path.
    fn library_at(jail: &Jail, name: &str, config: Option<&str>) -> PathBuf {
        let dir = jail.directory().join(name);
        if let Some(text) = config {
            std::fs::create_dir_all(dir.join(LIBRARY_STATE_DIR)).unwrap();
            std::fs::write(dir.join(LIBRARY_STATE_DIR).join("config.toml"), text).unwrap();
        } else {
            std::fs::create_dir_all(&dir).unwrap();
        }
        dir
    }

    fn user_config(jail: &Jail, text: &str) -> PathBuf {
        let path = jail.directory().join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn defaults_apply_when_no_file_exists() {
        Jail::expect_with(|jail| {
            let missing = jail.directory().join("absent.toml");
            let loaded = load_from(&missing, None).unwrap();
            assert_eq!(loaded.config.citekey.max_length, 48);
            assert_eq!(loaded.library.name, DEFAULT_LIBRARY_NAME);
            assert!(loaded.library_path.is_none());
            Ok(())
        });
    }

    #[test]
    fn user_file_overrides_defaults() {
        Jail::expect_with(|jail| {
            let dir = library_at(jail, "lib", None);
            let path = user_config(
                jail,
                &format!(
                    "[libraries.main]\ndir = \"{}\"\n[citekey]\nmax_length = 32\n",
                    dir.display()
                ),
            );
            assert_eq!(
                load_from(&path, None).unwrap().config.citekey.max_length,
                32
            );
            Ok(())
        });
    }

    #[test]
    fn library_file_overrides_user_file() {
        Jail::expect_with(|jail| {
            let dir = library_at(jail, "lib", Some("[citekey]\nmax_length = 99\n"));
            let path = user_config(
                jail,
                &format!(
                    "[libraries.main]\ndir = \"{}\"\n[citekey]\nmax_length = 32\n",
                    dir.display()
                ),
            );
            let loaded = load_from(&path, None).unwrap();
            assert_eq!(loaded.config.citekey.max_length, 99);
            assert!(
                loaded.library_path.is_some(),
                "library config should be reported"
            );
            Ok(())
        });
    }

    #[test]
    fn env_overrides_library_file() {
        Jail::expect_with(|jail| {
            let dir = library_at(jail, "lib", Some("[citekey]\nmax_length = 99\n"));
            let path = user_config(
                jail,
                &format!("[libraries.main]\ndir = \"{}\"\n", dir.display()),
            );
            jail.set_env("BIB_CITEKEY__MAX_LENGTH", "7");
            assert_eq!(load_from(&path, None).unwrap().config.citekey.max_length, 7);
            Ok(())
        });
    }

    /// Regression: reserved control variables must not be parsed as settings,
    /// or `deny_unknown_fields` fails every command. Covers the whole list, so
    /// adding a name to `RESERVED_ENV` without handling it fails here.
    #[test]
    fn reserved_env_vars_are_not_treated_as_settings() {
        Jail::expect_with(|jail| {
            let dir = library_at(jail, "lib", None);
            let path = user_config(
                jail,
                &format!("[libraries.main]\ndir = \"{}\"\n", dir.display()),
            );
            jail.set_env("BIB_CONFIG", path.display().to_string());
            jail.set_env("BIB_LOG", "bib=debug");
            assert!(
                load_from(&path, None).is_ok(),
                "reserved vars must be ignored"
            );
            Ok(())
        });
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = validate_toml("[citekey]\nmaxlength = 10\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("maxlength"),
            "error should name the offending key, got: {err:#}"
        );
    }

    #[test]
    fn unknown_library_name_lists_known_ones() {
        Jail::expect_with(|jail| {
            let dir = library_at(jail, "lib", None);
            let path = user_config(
                jail,
                &format!("[libraries.main]\ndir = \"{}\"\n", dir.display()),
            );
            let err = load_from(&path, Some("nope")).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("nope") && msg.contains("main"), "got: {msg}");
            Ok(())
        });
    }

    #[test]
    fn tilde_expands_against_home() {
        Jail::expect_with(|jail| {
            jail.set_env("HOME", "/home/example");
            assert_eq!(
                expand_tilde(Path::new("~/Documents/library")),
                PathBuf::from("/home/example/Documents/library")
            );
            // Absolute and relative paths are untouched.
            assert_eq!(
                expand_tilde(Path::new("/srv/lib")),
                PathBuf::from("/srv/lib")
            );
            Ok(())
        });
    }
}
