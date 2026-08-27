//! `bib open` — hand an attachment to the configured viewer.
//!
//! One of the four things a launcher does when a result is activated, and the
//! only one that needs a process: the cite key and the identifier are already
//! in the result payload, so those actions cost nothing.

use crate::config::{self, Config};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::path::Path;
use std::process::{Command, Stdio};

/// Stands in for the file path while the command line is split into arguments.
///
/// Substituting the path *after* splitting is what keeps a path containing
/// spaces a single argument, however the user wrote their template — and means
/// no shell is involved, so there is no quoting or injection surface at all.
const FILE_SENTINEL: &str = "\u{1}bib-file\u{1}";

#[derive(Debug, Args)]
pub struct OpenArgs {
    pub citekey: String,

    /// Which attachment, when a document has several (1-based).
    #[arg(long, default_value_t = 1)]
    pub file: usize,

    /// Print the command instead of running it.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: OpenArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let doc = store.get(&args.citekey)?;

    let files = doc.files();
    if files.is_empty() {
        bail!("`{}` has no attachments", args.citekey);
    }
    let index = args
        .file
        .checked_sub(1)
        .filter(|i| *i < files.len())
        .with_context(|| {
            format!(
                "`{}` has {} attachment(s); --file must be 1..={}",
                args.citekey,
                files.len(),
                files.len()
            )
        })?;
    let path = &files[index];
    if !path.is_file() {
        bail!("{} is missing from disk", path.display());
    }

    let argv = command_for(&loaded.config, path)?;
    if args.dry_run {
        println!("{}", argv.join(" "));
        return Ok(());
    }

    // Detached: a launcher must not be left waiting on a PDF viewer, and the
    // viewer must outlive us.
    Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not run `{}`", argv[0]))?;
    Ok(())
}

/// Build the argument vector for opening `path`.
pub fn command_for(config: &Config, path: &Path) -> Result<Vec<String>> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();

    let Some(template) = config.open.get(&extension) else {
        // No template configured: hand it to the desktop, which is what a user
        // who has not configured anything means.
        return Ok(vec![
            "xdg-open".to_owned(),
            path.to_string_lossy().into_owned(),
        ]);
    };

    let env = crate::model::citekey::template_env();
    let rendered = env
        .render_str(template, minijinja::context! { file => FILE_SENTINEL })
        .with_context(|| format!("rendering [open].{extension}"))?;

    let argv: Vec<String> = split(&rendered)
        .into_iter()
        .map(|arg| arg.replace(FILE_SENTINEL, &path.to_string_lossy()))
        .collect();
    if argv.is_empty() {
        bail!("[open].{extension} rendered an empty command");
    }
    Ok(argv)
}

/// Split a command line into arguments, honouring single and double quotes.
///
/// Not a shell: no expansion, no globbing, no substitution. The only reason
/// quotes are understood at all is so a user can write
/// `sh -c 'something {{ file }}'` if they really want a shell — in which case
/// they have chosen it explicitly.
fn split(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                // An empty quoted string is still an argument.
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn config_with(extension: &str, template: &str) -> Config {
        let mut open = BTreeMap::new();
        open.insert(extension.to_owned(), template.to_owned());
        Config {
            open,
            ..Config::default()
        }
    }

    #[test]
    fn the_configured_template_supplies_the_command() {
        let config = config_with("pdf", "zathura --page 1 {{ file }}");
        let argv = command_for(&config, Path::new("/library/a/paper.pdf")).unwrap();
        assert_eq!(argv, ["zathura", "--page", "1", "/library/a/paper.pdf"]);
    }

    /// The reason the path is substituted after splitting: a path with spaces
    /// must stay one argument whether or not the template quotes it.
    #[test]
    fn a_path_containing_spaces_stays_one_argument() {
        let config = config_with("pdf", "zathura {{ file }}");
        let argv = command_for(&config, Path::new("/library/my paper/the file.pdf")).unwrap();
        assert_eq!(argv, ["zathura", "/library/my paper/the file.pdf"]);
    }

    /// A path is data, never a command. Nothing in it can add an argument.
    #[test]
    fn a_path_cannot_inject_extra_arguments() {
        let config = config_with("pdf", "zathura {{ file }}");
        let argv = command_for(&config, Path::new("/tmp/x.pdf; rm -rf ~")).unwrap();
        assert_eq!(argv.len(), 2, "got {argv:?}");
        assert_eq!(argv[1], "/tmp/x.pdf; rm -rf ~");
    }

    #[test]
    fn an_unconfigured_extension_falls_back_to_the_desktop() {
        let argv = command_for(&Config::default(), Path::new("/a/b.epub")).unwrap();
        assert_eq!(argv, ["xdg-open", "/a/b.epub"]);
    }

    /// Extensions are matched case-insensitively; `PAPER.PDF` is a PDF.
    #[test]
    fn extension_matching_ignores_case() {
        let config = config_with("pdf", "zathura {{ file }}");
        let argv = command_for(&config, Path::new("/a/PAPER.PDF")).unwrap();
        assert_eq!(argv[0], "zathura");
    }

    #[test]
    fn quoted_arguments_are_kept_together() {
        assert_eq!(split(r#"sh -c 'open a file'"#), ["sh", "-c", "open a file"]);
        assert_eq!(split(r#"a "b c" d"#), ["a", "b c", "d"]);
        assert_eq!(split("  spaced   out  "), ["spaced", "out"]);
        assert_eq!(split(""), Vec::<String>::new());
    }
}
