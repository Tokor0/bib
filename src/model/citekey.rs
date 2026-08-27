//! Cite-key generation.
//!
//! Templates are minijinja, evaluated with strict undefined handling: a
//! template that references a field the entry lacks fails, and the next
//! candidate is tried. That is what makes `templates` an ordered fallback
//! chain rather than a single brittle pattern.

use crate::config::{CitekeyConfig, CollisionPolicy, Normalize};
use crate::model::label;
use anyhow::{Result, anyhow, bail};
use hayagriva::Entry;
use minijinja::{Environment, UndefinedBehavior, Value as JinjaValue};
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

/// Renders cite keys for a given configuration.
pub struct KeyMaker<'c> {
    env: Environment<'static>,
    config: &'c CitekeyConfig,
}

impl<'c> KeyMaker<'c> {
    pub fn new(config: &'c CitekeyConfig) -> Self {
        let mut env = Environment::new();
        // Strict undefined turns "this entry has no editor" into a template
        // error we can catch, which is how fallback templates are selected.
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        register_filters(&mut env);
        Self { env, config }
    }

    /// Render the first template that resolves against `entry`.
    pub fn render(&self, entry: &Entry) -> Result<String> {
        let context = context_for(entry, None);
        let mut failures = Vec::new();

        for (i, template) in self.config.templates.iter().enumerate() {
            match self.render_one(template, &context) {
                // Emptiness is checked *after* sanitizing: a template can
                // render non-empty and still leave nothing Typst can read
                // (`{{ title | word(1) }}` on a title starting "..."), and an
                // empty cite key must never reach the store.
                Ok(rendered) => match self.postprocess(&rendered) {
                    key if !key.is_empty() => return Ok(key),
                    _ => failures.push(format!(
                        "template {i} rendered {rendered:?}, which sanitizes to nothing"
                    )),
                },
                Err(e) => failures.push(format!("template {i}: {e}")),
            }
        }
        Err(anyhow!(
            "no cite-key template matched `{}`:\n  {}",
            entry.key(),
            failures.join("\n  ")
        ))
    }

    fn render_one(&self, template: &str, context: &JinjaValue) -> Result<String> {
        Ok(self.env.render_str(template, context)?)
    }

    /// Normalize, strip everything Typst cannot read inside `@key`, and
    /// enforce the length cap.
    ///
    /// The character rule is Typst's, not ours: see [`crate::model::label`].
    /// May return an empty string when nothing survives, which
    /// [`Self::render`] treats as a failed template rather than emitting an
    /// unusable key.
    fn postprocess(&self, raw: &str) -> String {
        let normalized: String = match self.config.normalize {
            Normalize::Nfc => raw.nfc().collect(),
            Normalize::Nfkc => raw.nfkc().collect(),
            Normalize::Nfd => raw.nfd().collect(),
            Normalize::Nfkd => raw.nfkd().collect(),
            Normalize::None => raw.to_owned(),
        };

        label::sanitize(&normalized, self.config.max_length)
    }

    /// Resolve a collision against keys already in use.
    ///
    /// `taken` is consulted rather than mutated; the caller owns the index.
    pub fn disambiguate(&self, base: &str, taken: &dyn Fn(&str) -> bool) -> Result<String> {
        if !taken(base) {
            return Ok(base.to_owned());
        }
        match self.config.on_collision {
            CollisionPolicy::Error => {
                bail!("cite key `{base}` is already taken (on_collision = \"error\")")
            }
            CollisionPolicy::SuffixAlpha => {
                // a..z, then aa, ab, … so the scheme never runs out.
                for n in 0..usize::MAX {
                    let candidate = format!("{base}{}", alpha_suffix(n));
                    if !taken(&candidate) {
                        return Ok(candidate);
                    }
                }
                unreachable!("alpha suffixes are unbounded")
            }
            CollisionPolicy::SuffixNumeric => {
                for n in 2..usize::MAX {
                    let candidate = format!("{base}-{n}");
                    if !taken(&candidate) {
                        return Ok(candidate);
                    }
                }
                unreachable!("numeric suffixes are unbounded")
            }
        }
    }
}

/// 0 -> "a", 25 -> "z", 26 -> "aa", 27 -> "ab", …
fn alpha_suffix(mut n: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'a' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii only")
}

/// Build the template context from an entry.
/// Build the template context for an entry. Public so `--format` renders
/// against exactly the same variables and filters as cite-key templates —
/// one templating syntax for the whole tool.
pub fn context_for(entry: &Entry, citekey: Option<&str>) -> JinjaValue {
    let mut ctx: BTreeMap<&str, JinjaValue> = BTreeMap::new();

    if let Some(kind) = entry_type_name(entry) {
        ctx.insert("type", JinjaValue::from(kind));
    }
    // Folder templates run after the key is known; cite-key templates
    // themselves are rendered without it.
    if let Some(key) = citekey {
        ctx.insert("citekey", JinjaValue::from(key));
    }

    if let Some(title) = entry.title() {
        ctx.insert("title", JinjaValue::from(title.to_string()));
    }
    if let Some(people) = people(entry.authors()) {
        ctx.insert("author", people);
    }
    if let Some(people) = people(entry.editors()) {
        ctx.insert("editor", people);
    }
    if let Some(date) = entry.date() {
        let mut d: BTreeMap<&str, JinjaValue> = BTreeMap::new();
        d.insert("year", JinjaValue::from(date.year));
        if let Some(month) = date.month {
            // hayagriva months are zero-based; humans expect 1-12.
            d.insert("month", JinjaValue::from(month + 1));
        }
        if let Some(day) = date.day {
            d.insert("day", JinjaValue::from(day + 1));
        }
        ctx.insert("date", JinjaValue::from_object(d));
    }
    if let Some(doi) = entry.doi() {
        ctx.insert("doi", JinjaValue::from(doi));
    }
    if let Some(isbn) = entry.isbn() {
        ctx.insert("isbn", JinjaValue::from(isbn));
    }
    if let Some(arxiv) = entry.arxiv() {
        ctx.insert("arxiv", JinjaValue::from(arxiv));
    }
    if let Some(publisher) = entry.publisher().and_then(|p| p.name()) {
        ctx.insert("publisher", JinjaValue::from(publisher.to_string()));
    }
    // The containing work: a journal for an article, a book for a chapter.
    if let Some(parent) = entry.parents().first().and_then(|p| p.title()) {
        let mut p: BTreeMap<&str, JinjaValue> = BTreeMap::new();
        p.insert("title", JinjaValue::from(parent.to_string()));
        ctx.insert("parent", JinjaValue::from_object(p));
    }

    JinjaValue::from_object(ctx)
}

fn people(list: Option<&[hayagriva::types::Person]>) -> Option<JinjaValue> {
    let list = list.filter(|l| !l.is_empty())?;
    let people: Vec<JinjaValue> = list
        .iter()
        .map(|p| {
            let mut m: BTreeMap<&str, JinjaValue> = BTreeMap::new();
            m.insert("family", JinjaValue::from(p.name.clone()));
            if let Some(given) = &p.given_name {
                m.insert("given", JinjaValue::from(given.clone()));
                m.insert("initial", JinjaValue::from(first_char(given)));
            }
            if let Some(prefix) = &p.prefix {
                m.insert("prefix", JinjaValue::from(prefix.clone()));
            }
            if let Some(suffix) = &p.suffix {
                m.insert("suffix", JinjaValue::from(suffix.clone()));
            }
            JinjaValue::from_object(m)
        })
        .collect();
    Some(JinjaValue::from(people))
}

fn first_char(s: &str) -> String {
    s.chars().next().map(String::from).unwrap_or_default()
}

/// Filters beyond minijinja's built-ins. `lower`, `upper`, `trim`, `replace`
/// and `truncate` already exist and are not redefined here.
/// A minijinja environment with our filters registered.
///
/// Shared by cite keys, folder names and `--format` so users learn one syntax.
pub fn template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    register_filters(&mut env);
    env
}

fn register_filters(env: &mut Environment<'static>) {
    // Transliterate to ASCII, so `Bjørn` keys as `bjorn` rather than losing
    // the character or emitting a non-ASCII path component.
    env.add_filter("ascii", |v: String| deunicode::deunicode(&v));

    env.add_filter("slug", |v: String| {
        let ascii = deunicode::deunicode(&v).to_lowercase();
        let mut out = String::with_capacity(ascii.len());
        let mut pending_sep = false;
        for c in ascii.chars() {
            if c.is_ascii_alphanumeric() {
                if pending_sep && !out.is_empty() {
                    out.push('-');
                }
                pending_sep = false;
                out.push(c);
            } else {
                pending_sep = true;
            }
        }
        out
    });

    // First `n` whitespace-separated words, concatenated.
    env.add_filter("words", |v: String, n: usize| {
        v.split_whitespace().take(n).collect::<Vec<_>>().join("")
    });

    // The `n`-th word, 1-indexed, so `word(1)` is the first.
    env.add_filter("word", |v: String, n: usize| {
        v.split_whitespace()
            .nth(n.saturating_sub(1))
            .unwrap_or_default()
            .to_owned()
    });

    // First `n` characters.
    env.add_filter("abbrev", |v: String, n: usize| {
        v.chars().take(n).collect::<String>()
    });

    // minijinja has no `truncate`, though Jinja2 does and the documented
    // default templates use it. Character-based, not word-based: a cite key
    // wants a hard length cap, not an ellipsis.
    env.add_filter("truncate", |v: String, n: usize| {
        v.chars().take(n).collect::<String>()
    });

    // Drop leading function words so the title word in a key is meaningful.
    env.add_filter("nostop", |v: String| strip_leading_stopwords(&v));

    env.add_filter("alnum", |v: String| {
        v.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    });

    env.add_filter("nospace", |v: String| {
        v.chars().filter(|c| !c.is_whitespace()).collect::<String>()
    });

    env.add_filter("titlecase", |v: String| {
        v.split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    });
}

/// `EntryType` implements `Serialize` (kebab-case) but not `Display`, so the
/// canonical spelling comes from serde rather than `Debug`.
fn entry_type_name(entry: &Entry) -> Option<String> {
    serde_yaml::to_value(entry.entry_type())
        .ok()?
        .as_str()
        .map(str::to_owned)
}

impl KeyMaker<'_> {
    /// Render a document's directory path from the folder template. The result
    /// may contain `/`, so a template like `{{ date.year }}/{{ citekey }}`
    /// nests by year.
    pub fn render_folder(
        &self,
        template: &str,
        entry: &Entry,
        citekey: &str,
    ) -> Result<std::path::PathBuf> {
        let context = context_for(entry, Some(citekey));
        let rendered = self.env.render_str(template, context)?;

        let mut path = std::path::PathBuf::new();
        for component in rendered.split('/').filter(|s| !s.is_empty()) {
            // A template that emitted `..` or an absolute segment would escape
            // the library root; treat that as a configuration error.
            if component == ".." || component == "." {
                bail!("folder template produced an unsafe path component: `{component}`");
            }
            path.push(component);
        }
        if path.as_os_str().is_empty() {
            bail!("folder template `{template}` rendered empty");
        }
        Ok(path)
    }
}

/// Leading words skipped by the `nostop` filter.
///
/// Only English function words, and only ever stripped from the *front* of a
/// title: the goal is to reach the first word that actually identifies the
/// work, so `On the Electrodynamics of Moving Bodies` keys on
/// `electrodynamics` rather than `on`.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "on", "of", "in", "at", "for", "to", "and", "or", "but", "with", "from",
    "by", "into", "over", "under", "as", "via", "about", "toward", "towards", "is", "are", "be",
    "this", "that", "these", "those", "its", "it",
];

fn strip_leading_stopwords(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let first_significant = words.iter().position(|w| {
        let bare: String = w
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        !bare.is_empty() && !STOPWORDS.contains(&bare.as_str())
    });
    match first_significant {
        // A title that is nothing but stopwords keeps its original text rather
        // than rendering empty and failing the template.
        None => text.to_owned(),
        Some(i) => words[i..].join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CitekeyConfig, CollisionPolicy};
    use crate::model::bridge;

    fn entry(yaml: &str) -> Entry {
        let value = serde_yaml::from_str(yaml).expect("fixture should be valid YAML");
        bridge::to_entry("fixture", &value).expect("fixture should be a valid entry")
    }

    fn key_of(yaml: &str) -> String {
        let config = CitekeyConfig::default();
        KeyMaker::new(&config)
            .render(&entry(yaml))
            .expect("should render")
    }

    const EINSTEIN: &str = r#"
type: article
title: On the Electrodynamics of Moving Bodies
author: ["Einstein, Albert"]
date: 1905-06-30
"#;

    #[test]
    fn default_template_uses_author_year_and_first_significant_word() {
        assert_eq!(key_of(EINSTEIN), "einstein1905electrodynamics");
    }

    /// Leading function words are skipped, or every English title starting with
    /// "The" would collapse to the same key.
    #[test]
    fn leading_stopwords_are_skipped() {
        let yaml = r#"
type: book
title: The Art of Computer Programming
author: ["Knuth, Donald E."]
date: 1997
"#;
        assert_eq!(key_of(yaml), "knuth1997art");
    }

    /// A title made only of stopwords must still produce a key rather than
    /// rendering empty and failing every template.
    #[test]
    fn all_stopword_title_falls_back_to_the_original_text() {
        let yaml = r#"
type: book
title: The On Of
author: ["Smith, Jane"]
date: 2020
"#;
        assert_eq!(key_of(yaml), "smith2020the");
    }

    #[test]
    fn non_ascii_names_are_transliterated() {
        let yaml = r#"
type: book
title: Ægir and the Ømega
author: ["Bjørnson, Bjørnstjerne"]
date: 2001
"#;
        assert_eq!(key_of(yaml), "bjornson2001aegir");
    }

    /// Strict undefined handling is what makes `templates` an ordered fallback
    /// chain: the author template fails, so the editor one is used.
    #[test]
    fn falls_back_to_the_editor_template_when_there_is_no_author() {
        let yaml = r#"
type: book
title: A Collected Volume
editor: ["Dahl, Roald"]
date: 1980
"#;
        assert_eq!(key_of(yaml), "dahl1980");
    }

    #[test]
    fn falls_back_to_the_title_template_when_there_is_no_person() {
        let yaml = r#"
type: report
title: Annual Review of Widgets
date: 2019
"#;
        assert_eq!(key_of(yaml), "annual-review-of-widgets2019");
    }

    #[test]
    fn reports_every_failed_template_when_none_match() {
        // No author, no editor, no title, and no date for the fallbacks.
        let config = CitekeyConfig::default();
        let err = KeyMaker::new(&config)
            .render(&entry("type: misc\n"))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("template 0") && msg.contains("template 2"),
            "got: {msg}"
        );
    }

    #[test]
    fn truncates_on_a_character_boundary() {
        let config = CitekeyConfig {
            max_length: 12,
            ..CitekeyConfig::default()
        };
        let key = KeyMaker::new(&config).render(&entry(EINSTEIN)).unwrap();
        assert_eq!(key, "einstein1905");
        assert_eq!(key.chars().count(), 12);
    }

    #[test]
    fn alpha_suffixes_continue_past_z() {
        assert_eq!(alpha_suffix(0), "a");
        assert_eq!(alpha_suffix(25), "z");
        assert_eq!(alpha_suffix(26), "aa");
        assert_eq!(alpha_suffix(27), "ab");
    }

    #[test]
    fn collisions_get_an_alpha_suffix() {
        let config = CitekeyConfig::default();
        let maker = KeyMaker::new(&config);
        let taken = |k: &str| matches!(k, "smith2020" | "smith2020a");
        assert_eq!(
            maker.disambiguate("smith2020", &taken).unwrap(),
            "smith2020b"
        );
        // An unused key is returned untouched.
        assert_eq!(
            maker.disambiguate("jones2020", &taken).unwrap(),
            "jones2020"
        );
    }

    #[test]
    fn numeric_collision_policy_starts_at_two() {
        let config = CitekeyConfig {
            on_collision: CollisionPolicy::SuffixNumeric,
            ..Default::default()
        };
        let maker = KeyMaker::new(&config);
        assert_eq!(
            maker
                .disambiguate("smith2020", &|k: &str| k == "smith2020")
                .unwrap(),
            "smith2020-2"
        );
    }

    #[test]
    fn error_collision_policy_refuses() {
        let config = CitekeyConfig {
            on_collision: CollisionPolicy::Error,
            ..Default::default()
        };
        let maker = KeyMaker::new(&config);
        assert!(
            maker
                .disambiguate("smith2020", &|k: &str| k == "smith2020")
                .is_err()
        );
    }

    #[test]
    fn folder_template_can_nest_by_year() {
        let config = CitekeyConfig::default();
        let maker = KeyMaker::new(&config);
        let folder = maker
            .render_folder(
                "{{ date.year }}/{{ citekey }}",
                &entry(EINSTEIN),
                "einstein1905",
            )
            .unwrap();
        assert_eq!(folder, std::path::PathBuf::from("1905/einstein1905"));
    }

    /// A template that escapes the library root is a configuration error, not
    /// something to silently normalize away.
    #[test]
    fn folder_template_rejects_parent_traversal() {
        let config = CitekeyConfig::default();
        let maker = KeyMaker::new(&config);
        let err = maker
            .render_folder("../{{ citekey }}", &entry(EINSTEIN), "einstein1905")
            .unwrap_err();
        assert!(format!("{err:#}").contains("unsafe"), "got: {err:#}");
    }

    #[test]
    fn custom_filters_behave() {
        let config = CitekeyConfig::default();
        let maker = KeyMaker::new(&config);
        let e = entry(EINSTEIN);
        let render = |t: &str| {
            maker
                .render_folder(t, &e, "k")
                .unwrap()
                .display()
                .to_string()
        };

        assert_eq!(render("{{ author[0].initial }}"), "A");
        assert_eq!(render("{{ title | word(2) }}"), "the");
        assert_eq!(render("{{ title | abbrev(2) }}"), "On");
        assert_eq!(render("{{ title | slug | truncate(10) }}"), "on-the-ele");
        assert_eq!(render("{{ type }}"), "article");
    }

    /// Regression: `char::is_alphanumeric()` admits `No`/`Nl` characters that
    /// are not `XID_Continue`, so a key could contain `²` — valid YAML, valid
    /// hayagriva, and read by Typst's lexer as a reference ending at the `²`.
    ///
    /// The default template hides this behind `| ascii`, which deunicodes `²`
    /// to `2`. The reachable case is a template that deliberately preserves
    /// unicode, which the config supports precisely so keys can read
    /// `müller2020` rather than `muller2020`.
    #[test]
    fn superscripts_are_stripped_from_unicode_preserving_keys() {
        let config = CitekeyConfig {
            templates: vec![
                "{{ author[0].family | lower }}{{ date.year }}\
                 {{ title | nostop | words(1) | lower }}"
                    .to_owned(),
            ],
            normalize: Normalize::None,
            ..CitekeyConfig::default()
        };
        let yaml = r#"
type: article
title: R² Statistics for Mixed Models
author: ["Nakagawa, Shinichi"]
date: 2013
"#;
        let key = KeyMaker::new(&config)
            .render(&entry(yaml))
            .expect("should render");
        assert_eq!(key, "nakagawa2013r");
        assert!(
            label::is_valid(&key),
            "{key:?} is not referenceable as @{key}"
        );
    }

    /// The same template must still pass unicode letters through untouched —
    /// stripping `²` is about `XID_Continue`, not about being non-ASCII.
    #[test]
    fn unicode_letters_survive_in_unicode_preserving_keys() {
        let config = CitekeyConfig {
            templates: vec!["{{ author[0].family | lower }}{{ date.year }}".to_owned()],
            normalize: Normalize::None,
            ..CitekeyConfig::default()
        };
        let yaml = r#"
type: article
title: Zur Elektrodynamik
author: ["Müller, Hans"]
date: 2020
"#;
        let key = KeyMaker::new(&config)
            .render(&entry(yaml))
            .expect("should render");
        assert_eq!(key, "müller2020");
        assert!(label::is_valid(&key));
    }

    /// Regression: `@kubrick1964dr.` resolves to `kubrick1964dr`, because
    /// Typst's `ref_marker` strips trailing `.` and `:`. A key ending in
    /// either is unreferenceable as written.
    #[test]
    fn keys_never_end_in_punctuation_typst_would_strip() {
        let yaml = r#"
type: article
title: Dr. Strangelove Reconsidered
author: ["Kubrick, Stanley"]
date: 1964
"#;
        assert_eq!(key_of(yaml), "kubrick1964dr");
    }

    /// The length cap is what puts a `.` at the end, so trimming has to happen
    /// after truncation rather than before it.
    #[test]
    fn truncation_cannot_expose_trailing_punctuation() {
        let config = CitekeyConfig {
            templates: vec!["{{ title }}".to_owned()],
            max_length: 13,
            normalize: Normalize::None,
            ..CitekeyConfig::default()
        };
        let yaml = r#"
type: article
title: kubrick1964dr.strangelove
author: ["Kubrick, Stanley"]
date: 1964
"#;
        let key = KeyMaker::new(&config)
            .render(&entry(yaml))
            .expect("should render");
        assert_eq!(key, "kubrick1964dr");
        assert!(label::is_valid(&key));
    }

    /// A template that renders non-empty but sanitizes to nothing must fall
    /// through to the next template, never yield an empty cite key.
    #[test]
    fn a_key_that_sanitizes_to_nothing_falls_through() {
        let config = CitekeyConfig {
            templates: vec![
                "{{ title }}".to_owned(),
                "{{ author[0].family | lower }}{{ date.year }}".to_owned(),
            ],
            normalize: Normalize::None,
            ..CitekeyConfig::default()
        };
        let yaml = r#"
type: article
title: "..."
author: ["Kubrick, Stanley"]
date: 1964
"#;
        assert_eq!(
            KeyMaker::new(&config)
                .render(&entry(yaml))
                .expect("should fall through to the second template"),
            "kubrick1964"
        );
    }
}
