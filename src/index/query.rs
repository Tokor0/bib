//! The query language: `author:einstein year:1905-1910 tag:relativity AND "moving bodies"`.
//!
//! Deliberately small. Field terms, `AND`/`OR`/`NOT`, parentheses, quoted
//! phrases and year ranges; anything not recognised as a field falls through to
//! full-text search. Adjacent terms are implicitly `AND`ed, so the common case
//! reads like a search box rather than like SQL.
//!
//! Operators are uppercase-only. Lowercase `and` in `"Sense and Sensibility"`
//! is a word people search for far more often than they need a conjunction they
//! would get from a space anyway.

use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Matches every document; what an empty query parses to.
    All,
    Field {
        field: Field,
        value: Value,
    },
    /// A bare word or quoted phrase, resolved against the full-text index.
    Text(Text),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
    Not(Box<Query>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Key,
    Type,
    Title,
    Author,
    Editor,
    /// Either author or editor, for people who do not care which.
    Person,
    Year,
    Tag,
    Publisher,
    /// Containing work: journal, proceedings, anthology.
    Parent,
    Doi,
    Arxiv,
    Isbn,
    /// Any serial number, whatever its kind.
    Id,
    /// Has an attachment.
    File,
}

impl Field {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "key" | "citekey" | "ref" => Self::Key,
            "type" => Self::Type,
            "title" => Self::Title,
            "author" => Self::Author,
            "editor" => Self::Editor,
            "person" | "by" => Self::Person,
            "year" | "date" => Self::Year,
            "tag" | "tags" => Self::Tag,
            "publisher" => Self::Publisher,
            "parent" | "journal" | "booktitle" | "in" => Self::Parent,
            "doi" => Self::Doi,
            "arxiv" => Self::Arxiv,
            "isbn" => Self::Isbn,
            "id" | "serial" => Self::Id,
            "file" | "has" => Self::File,
            _ => return None,
        })
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Year)
    }
}

/// The right-hand side of a field term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Substring match, case-insensitive — what a bare `author:einstein` means.
    Contains(String),
    /// Written quoted: `author:"van der Waals"`, matched exactly.
    Exact(String),
    /// `year:1905`, or `year:1905-1910`, `year:-1910`, `year:1905-`.
    Range { low: Option<i64>, high: Option<i64> },
    /// `file:yes` / `file:no`.
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    /// A bare word, matched as a prefix so `einstein` finds `einsteinian`.
    Prefix(String),
    /// A quoted phrase, matched as written.
    Phrase(String),
}

pub fn parse(input: &str) -> Result<Query> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(Query::All);
    }
    let mut parser = Parser { tokens, pos: 0 };
    let query = parser.expression()?;
    if let Some(extra) = parser.peek() {
        bail!("unexpected `{}` in query", extra.text());
    }
    Ok(query)
}

// ---------------------------------------------------------------- tokenizer

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A bare word, or the left half of `field:value`.
    Word(String),
    /// Text that was quoted, so it must not be reinterpreted as an operator.
    Quoted(String),
    /// `field:value`; the bool records whether the value was quoted.
    Pair(String, String, bool),
    And,
    Or,
    Not,
    Open,
    Close,
}

impl Token {
    fn text(&self) -> String {
        match self {
            Self::Word(w) | Self::Quoted(w) => w.clone(),
            Self::Pair(f, v, _) => format!("{f}:{v}"),
            Self::And => "AND".into(),
            Self::Or => "OR".into(),
            Self::Not => "NOT".into(),
            Self::Open => "(".into(),
            Self::Close => ")".into(),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            // `-term` negates. Whitespace is already skipped above, so reaching
            // this arm means the hyphen begins a token: a hyphen inside a word
            // (`well-known`) is consumed by `read_word` and never seen here.
            '-' if i + 1 < chars.len() && !chars[i + 1].is_whitespace() => {
                tokens.push(Token::Not);
                i += 1;
            }
            '"' | '\'' => {
                let (text, next) = read_quoted(&chars, i)?;
                i = next;
                tokens.push(Token::Quoted(text));
            }
            _ => {
                let (word, next) = read_word(&chars, i);
                i = next;
                match word.as_str() {
                    "AND" => tokens.push(Token::And),
                    "OR" => tokens.push(Token::Or),
                    "NOT" => tokens.push(Token::Not),
                    _ => {
                        // `field:` may be followed by a quoted value, which the
                        // word reader stops before.
                        if let Some(name) = word.strip_suffix(':')
                            && i < chars.len()
                            && matches!(chars[i], '"' | '\'')
                        {
                            let (value, next) = read_quoted(&chars, i)?;
                            i = next;
                            tokens.push(Token::Pair(name.to_owned(), value, true));
                        } else if let Some((name, value)) = split_pair(&word) {
                            tokens.push(Token::Pair(name, value, false));
                        } else {
                            tokens.push(Token::Word(word));
                        }
                    }
                }
            }
        }
    }
    Ok(tokens)
}

fn read_quoted(chars: &[char], start: usize) -> Result<(String, usize)> {
    let quote = chars[start];
    let mut out = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == quote {
            return Ok((out, i + 1));
        }
        out.push(chars[i]);
        i += 1;
    }
    Err(anyhow!("unterminated {quote} in query"))
}

fn read_word(chars: &[char], start: usize) -> (String, usize) {
    let mut out = String::new();
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || matches!(c, '(' | ')') {
            break;
        }
        out.push(c);
        i += 1;
        // Stop right after `field:` so a quoted value is read separately.
        if c == ':' && i < chars.len() && matches!(chars[i], '"' | '\'') {
            break;
        }
    }
    (out, i)
}

/// Split `field:value`, but only on a known field name — so a bare `10.1002/x`
/// or a URL stays one search term instead of becoming a nonsense field.
fn split_pair(word: &str) -> Option<(String, String)> {
    let (name, value) = word.split_once(':')?;
    if value.is_empty() || Field::parse(name).is_none() {
        return None;
    }
    Some((name.to_owned(), value.to_owned()))
}

// ------------------------------------------------------------------ parser

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expression(&mut self) -> Result<Query> {
        let mut left = self.conjunction()?;
        while self.eat(&Token::Or) {
            let right = self.conjunction()?;
            left = Query::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn conjunction(&mut self) -> Result<Query> {
        let mut left = self.unary()?;
        loop {
            // Explicit AND and mere adjacency mean the same thing.
            let explicit = self.eat(&Token::And);
            match self.peek() {
                Some(Token::Close) | Some(Token::Or) | None if !explicit => break,
                None => bail!("query ends after AND"),
                _ => {}
            }
            let right = self.unary()?;
            left = Query::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Query> {
        if self.eat(&Token::Not) {
            return Ok(Query::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Query> {
        let token = self
            .peek()
            .cloned()
            .ok_or_else(|| anyhow!("query ended unexpectedly"))?;
        self.pos += 1;

        match token {
            Token::Open => {
                let inner = self.expression()?;
                if !self.eat(&Token::Close) {
                    bail!("unclosed ( in query");
                }
                Ok(inner)
            }
            Token::Word(w) => Ok(Query::Text(Text::Prefix(w))),
            Token::Quoted(w) => Ok(Query::Text(Text::Phrase(w))),
            Token::Pair(name, value, quoted) => {
                let field =
                    Field::parse(&name).ok_or_else(|| anyhow!("unknown search field `{name}`"))?;
                Ok(Query::Field {
                    field,
                    value: parse_value(field, &value, quoted)?,
                })
            }
            Token::Close => bail!("unmatched ) in query"),
            other => bail!("`{}` needs something on both sides", other.text()),
        }
    }
}

fn parse_value(field: Field, raw: &str, quoted: bool) -> Result<Value> {
    if field == Field::File {
        return Ok(Value::Bool(matches!(
            raw.to_ascii_lowercase().as_str(),
            "yes" | "y" | "true" | "1" | "any"
        )));
    }
    if field.is_numeric() {
        return parse_range(raw).ok_or_else(|| anyhow!("`{raw}` is not a year or year range"));
    }
    Ok(if quoted {
        Value::Exact(raw.to_owned())
    } else {
        Value::Contains(raw.to_owned())
    })
}

/// `1905`, `1905-1910`, `1905-`, `-1910`, `>1905`, `<=1910`.
fn parse_range(raw: &str) -> Option<Value> {
    let num = |s: &str| s.trim().parse::<i64>().ok();

    if let Some(rest) = raw.strip_prefix(">=") {
        return Some(Value::Range {
            low: Some(num(rest)?),
            high: None,
        });
    }
    if let Some(rest) = raw.strip_prefix("<=") {
        return Some(Value::Range {
            low: None,
            high: Some(num(rest)?),
        });
    }
    if let Some(rest) = raw.strip_prefix('>') {
        return Some(Value::Range {
            low: Some(num(rest)? + 1),
            high: None,
        });
    }
    if let Some(rest) = raw.strip_prefix('<') {
        return Some(Value::Range {
            low: None,
            high: Some(num(rest)? - 1),
        });
    }
    if let Some(rest) = raw.strip_prefix('-') {
        return Some(Value::Range {
            low: None,
            high: Some(num(rest)?),
        });
    }
    if let Some(start) = raw.strip_suffix('-') {
        return Some(Value::Range {
            low: Some(num(start)?),
            high: None,
        });
    }
    if let Some((lo, hi)) = raw.split_once('-') {
        return Some(Value::Range {
            low: Some(num(lo)?),
            high: Some(num(hi)?),
        });
    }
    let exact = num(raw)?;
    Some(Value::Range {
        low: Some(exact),
        high: Some(exact),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(q: &Query) -> (Field, Value) {
        match q {
            Query::Field { field, value } => (*field, value.clone()),
            other => panic!("expected a field term, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(parse("").unwrap(), Query::All);
        assert_eq!(parse("   ").unwrap(), Query::All);
    }

    #[test]
    fn bare_words_become_prefix_text_search() {
        assert_eq!(
            parse("einstein").unwrap(),
            Query::Text(Text::Prefix("einstein".into()))
        );
        assert_eq!(
            parse("\"moving bodies\"").unwrap(),
            Query::Text(Text::Phrase("moving bodies".into()))
        );
    }

    #[test]
    fn adjacent_terms_are_implicitly_anded() {
        let implicit = parse("author:einstein tag:relativity").unwrap();
        let explicit = parse("author:einstein AND tag:relativity").unwrap();
        assert_eq!(implicit, explicit);
    }

    #[test]
    fn or_binds_looser_than_and() {
        // a AND (b OR c) would be a different tree; check we got (a AND b) OR c.
        let q = parse("author:a author:b OR author:c").unwrap();
        match q {
            Query::Or(left, right) => {
                assert!(matches!(*left, Query::And(..)), "left should be the AND");
                assert_eq!(field(&right).1, Value::Contains("c".into()));
            }
            other => panic!("expected OR at the root, got {other:?}"),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        let q = parse("(author:a OR author:b) author:c").unwrap();
        assert!(matches!(q, Query::And(..)), "got {q:?}");
    }

    #[test]
    fn negation_spelled_both_ways() {
        let word = parse("NOT author:einstein").unwrap();
        let dash = parse("-author:einstein").unwrap();
        assert_eq!(word, dash);
        assert!(matches!(word, Query::Not(_)));
    }

    /// Regression: a `-` after a complete term must still negate. An earlier
    /// guard only allowed it at the start of an expression, so the common
    /// `year:>1900 -author:einstein` silently matched nothing.
    #[test]
    fn negation_works_after_a_term() {
        let q = parse("year:>1900 -author:einstein").unwrap();
        match q {
            Query::And(left, right) => {
                assert_eq!(field(&left).0, Field::Year);
                assert!(matches!(*right, Query::Not(_)), "got {right:?}");
            }
            other => panic!("expected AND, got {other:?}"),
        }
    }

    /// A hyphen inside a word is part of the word, not an operator.
    #[test]
    fn interior_hyphens_stay_in_the_word() {
        assert_eq!(
            parse("well-known").unwrap(),
            Query::Text(Text::Prefix("well-known".into()))
        );
    }

    #[test]
    fn year_ranges() {
        let cases = [
            ("year:1905", Some(1905), Some(1905)),
            ("year:1905-1910", Some(1905), Some(1910)),
            ("year:1905-", Some(1905), None),
            ("year:-1910", None, Some(1910)),
            ("year:>1905", Some(1906), None),
            ("year:>=1905", Some(1905), None),
            ("year:<1910", None, Some(1909)),
            ("year:<=1910", None, Some(1910)),
        ];
        for (input, low, high) in cases {
            assert_eq!(
                field(&parse(input).unwrap()).1,
                Value::Range { low, high },
                "parsing {input}"
            );
        }
    }

    #[test]
    fn a_bad_year_is_an_error_not_a_text_search() {
        assert!(parse("year:soon").is_err());
    }

    /// `10.1002/andp` contains a colon in neither half, but a URL does — and a
    /// DOI search must not be reinterpreted as an unknown field.
    #[test]
    fn only_known_field_names_split_on_a_colon() {
        assert_eq!(
            parse("https://example.org").unwrap(),
            Query::Text(Text::Prefix("https://example.org".into()))
        );
        assert_eq!(field(&parse("doi:10.1002/x").unwrap()).0, Field::Doi);
    }

    #[test]
    fn quoted_field_values_are_matched_exactly() {
        assert_eq!(
            field(&parse("author:\"van der Waals\"").unwrap()).1,
            Value::Exact("van der Waals".into())
        );
        assert_eq!(
            field(&parse("author:waals").unwrap()).1,
            Value::Contains("waals".into())
        );
    }

    /// Lowercase `and` is a word people search for; only uppercase is the
    /// operator.
    #[test]
    fn lowercase_operators_are_ordinary_words() {
        assert_eq!(
            parse("\"sense and sensibility\"").unwrap(),
            Query::Text(Text::Phrase("sense and sensibility".into()))
        );
        assert!(matches!(parse("sense and").unwrap(), Query::And(..)));
    }

    #[test]
    fn malformed_queries_are_rejected() {
        for bad in [
            "(author:a",
            "author:a)",
            "AND author:a",
            "author:a AND",
            "\"open",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
