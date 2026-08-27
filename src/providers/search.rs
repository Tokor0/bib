//! Searching for documents that are not in the library yet.
//!
//! Separate from [`MetadataProvider`](super::MetadataProvider) because the
//! shapes differ: resolution takes one identifier and yields one record, while
//! search takes a query and yields many candidates of varying quality.
//!
//! **Results are re-ranked locally.** Provider relevance scores are not
//! comparable across services and at least one of them is actively misleading:
//! Crossref's `query.bibliographic` for *"attention is all you need"* returns
//! *"Is Attention All You Need?"*, *"…Valuation Of AI Tokens"* and *"All You
//! Need Is LSD"* before the actual paper. So provider order decides only which
//! record wins a field-level merge; the ordering the user sees comes from
//! comparing each title against what they typed.

use super::{Http, ProviderError};
use crate::formats::csl::CslItem;
use crate::identify::patterns::Identifier;
use std::collections::BTreeSet;
use std::time::Duration;

/// What to search for. Fields are combined by the provider that can use them;
/// one that cannot express a constraint ignores it rather than failing.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Free text, normally a title.
    pub text: String,
    pub author: Option<String>,
    pub year: Option<i64>,
    /// A CSL type name, when the user knows they want a book.
    pub kind: Option<String>,
    pub limit: usize,
    /// True when `text` is a whole citation string rather than a bare title —
    /// set by the identifier-less PDF path, which has authors and a venue as
    /// well. Crossref answers those far better through `query.bibliographic`.
    pub citation_like: bool,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: 10,
            ..Self::default()
        }
    }
}

pub trait SearchProvider {
    fn name(&self) -> &'static str;
    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError>;
    /// Minimum interval between requests. arXiv's terms make this non-optional.
    fn rate_limit(&self) -> Duration {
        Duration::from_millis(200)
    }
}

/// A candidate, with the identifier a caller would act on.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub item: CslItem,
    /// `None` when a provider returned a record with no usable identifier —
    /// such a result can be shown but not added, so it ranks last.
    pub id: Option<Identifier>,
    pub score: f64,
}

impl Candidate {
    pub fn title(&self) -> Option<String> {
        self.item
            .title
            .as_ref()
            .and_then(crate::formats::csl::Flexible::as_text)
    }
}

/// Extract the best identifier a record carries, preferring the one that
/// reaches the most authoritative catalogue.
pub fn identifier_of(item: &CslItem) -> Option<Identifier> {
    if let Some(doi) = item.doi.as_deref()
        && let Some(id) = Identifier::parse_doi(doi)
    {
        return Some(id);
    }
    // arXiv records carry their ID in the URL rather than a dedicated field.
    if let Some(url) = item.url.as_deref()
        && let Some(id) = crate::identify::patterns::normalize_arxiv(url)
    {
        return Some(Identifier::ArXiv(id));
    }
    let isbn = item
        .isbn
        .as_ref()
        .and_then(crate::formats::csl::Flexible::as_text)?;
    crate::identify::patterns::normalize_isbn(&isbn).map(Identifier::Isbn)
}

/// Merge results from several providers, deduplicate, and rank.
pub fn rank(query: &SearchQuery, results: Vec<CslItem>) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: BTreeSet<Identifier> = BTreeSet::new();

    for item in results {
        let id = identifier_of(&item);
        // The same paper reaches us from several providers; the first to
        // report it wins, which is provider order, which is configuration.
        if let Some(id) = &id
            && !seen.insert(id.clone())
        {
            continue;
        }
        let score = score(query, &item);
        candidates.push(Candidate { item, id, score });
    }

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // A record with no identifier cannot be added, so it sinks.
            .then(b.id.is_some().cmp(&a.id.is_some()))
            // A definite publication over a re-posting: Crossref's
            // `posted-content` is often a third party re-posting a paper that
            // exists properly elsewhere, and it matches the title exactly.
            // Same logic as the arXiv-to-published-DOI upgrade in `resolve`.
            .then(published_rank(&b.item).cmp(&published_rank(&a.item)))
            // Among equally-titled works, prefer the one the literature cites.
            // A widely-quoted title attracts book chapters, editorials and
            // aggregator records that match it exactly; citation count is the
            // one cheap signal that separates the paper from its echoes.
            .then(citation_bucket(&b.item).cmp(&citation_bucket(&a.item)))
            // Then a well-populated record over a bare one.
            .then(completeness(&b.item).cmp(&completeness(&a.item)))
            .then_with(|| a.title().cmp(&b.title()))
    });

    let mut ranked = interleave_by_source(candidates);
    ranked.truncate(query.limit.max(1));
    ranked
}

/// Whether a record describes a definite publication or a posting.
///
/// `posted-content` stays *above* nothing: a genuine preprint is a fine result
/// when no published version exists. It merely loses a tie to one that does.
/// arXiv records are `manuscript` and are deliberately not demoted — for a
/// computer-science paper the preprint is frequently the canonical artifact.
fn published_rank(item: &CslItem) -> u8 {
    match item.kind.as_deref() {
        Some("posted-content") => 0,
        _ => 1,
    }
}

/// Citation count, bucketed by order of magnitude.
///
/// Bucketed rather than compared directly so that 412 versus 419 citations does
/// not outrank a better title match, while 100000 versus 0 decisively does.
/// Providers that report no count sit in bucket 0 rather than losing outright,
/// since arXiv reports none at all.
fn citation_bucket(item: &CslItem) -> u32 {
    match item.cited_by {
        Some(count) if count > 0 => (count as f64).log10() as u32 + 1,
        _ => 0,
    }
}

/// How much usable detail a record carries.
fn completeness(item: &CslItem) -> usize {
    let mut score = 0;
    if !item.author.is_empty() {
        score += 1;
    }
    for present in [
        item.issued.as_ref().and_then(|d| d.to_iso()).is_some(),
        item.container_title.is_some(),
        item.page.is_some(),
        item.volume.is_some(),
        item.publisher.is_some(),
        item.abstract_.is_some(),
    ] {
        if present {
            score += 1;
        }
    }
    score
}

/// Round-robin across providers, preserving each provider's own order.
///
/// A widely-indexed title returns many equally exact matches, and without this
/// the highest-priority provider fills every visible slot — five near-identical
/// Crossref rows, with the arXiv preprint and the canonical published record
/// pushed off the end. Showing the best from each source first is more useful
/// than showing the top five from one.
fn interleave_by_source(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut by_source: Vec<(String, Vec<Candidate>)> = Vec::new();
    for candidate in candidates {
        let source = candidate.item.source.clone();
        match by_source.iter_mut().find(|(name, _)| *name == source) {
            Some((_, bucket)) => bucket.push(candidate),
            None => by_source.push((source, vec![candidate])),
        }
    }

    let mut out = Vec::new();
    let mut round = 0;
    loop {
        let mut took = false;
        for (_, bucket) in by_source.iter_mut() {
            if round < bucket.len() {
                out.push(bucket[round].clone());
                took = true;
            }
        }
        if !took {
            break;
        }
        round += 1;
    }
    out
}

/// How well a record answers the query.
///
/// Title similarity dominates. Jaro-Winkler rather than plain edit distance
/// because it rewards a shared prefix, which is what a truncated or subtitled
/// version of the right title looks like.
fn score(query: &SearchQuery, item: &CslItem) -> f64 {
    let Some(title) = item
        .title
        .as_ref()
        .and_then(crate::formats::csl::Flexible::as_text)
    else {
        return 0.0;
    };

    let wanted = normalize(&query.text);
    let got = normalize(&title);
    if wanted.is_empty() {
        return 0.0;
    }

    let mut score = strsim::jaro_winkler(&wanted, &got);

    // An exact match after normalisation should never lose to a near miss that
    // happens to score well on some other axis.
    if wanted == got {
        score = 1.0;
    } else if got.starts_with(&wanted) {
        // "Attention Is All You Need: A Retrospective" is very likely what was
        // meant when the query is the whole prefix.
        score = score.max(0.95);
    }

    // Corroborating fields nudge rather than decide: getting the author right
    // should not rescue an unrelated title.
    if let Some(author) = &query.author {
        let wanted = normalize(author);
        let matched = item.author.iter().any(|a| {
            a.family
                .as_deref()
                .map(normalize)
                .is_some_and(|f| f.contains(&wanted))
        });
        score += if matched { 0.05 } else { -0.05 };
    }
    if let Some(year) = query.year
        && let Some(issued) = item.issued.as_ref().and_then(|d| d.to_iso())
    {
        let matched = issued.starts_with(&year.to_string());
        score += if matched { 0.05 } else { -0.05 };
    }

    score.clamp(0.0, 1.0)
}

/// Case-folded, punctuation-stripped, whitespace-collapsed.
fn normalize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Percent-encode a query-string value.
///
/// Hand-rolled rather than pulling in `url`: this is the only place the crate
/// would be used, the rule is short and fixed, and a title full of colons,
/// slashes and quotes must not silently change meaning on the way into a URL.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::csl::Flexible;

    fn item(title: &str, doi: Option<&str>) -> CslItem {
        CslItem {
            kind: Some("article-journal".into()),
            title: Some(Flexible::Text(title.into())),
            doi: doi.map(str::to_owned),
            ..CslItem::default()
        }
    }

    /// The case that motivated local re-ranking: this is Crossref's own
    /// ordering for `query.bibliographic=attention is all you need`, with the
    /// real paper appended because Crossref did not return it in the top five
    /// at all. Re-ranking must lift the exact title to the front.
    #[test]
    fn an_exact_title_beats_crossrefs_own_ordering() {
        let crossref_order = vec![
            item(
                "Is Attention All You Need?",
                Some("10.1007/978-3-031-84300-6_13"),
            ),
            item(
                "Attention Is All You Need: An Analysis Of The Valuation Of Artificial Intelligence Tokens",
                Some("10.2139/ssrn.4993784"),
            ),
            item("All You Need Is LSD", Some("10.1234/lsd")),
            item("Attention Is All You Need", Some("10.5555/3295222.3295349")),
        ];

        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            crossref_order,
        );
        assert_eq!(
            ranked[0].title().as_deref(),
            Some("Attention Is All You Need"),
            "ranking left the exact match at position {:?}",
            ranked
                .iter()
                .position(|c| c.title().as_deref() == Some("Attention Is All You Need"))
        );
        assert_eq!(ranked[0].score, 1.0);
    }

    /// Case and punctuation are noise in a title search.
    #[test]
    fn matching_ignores_case_and_punctuation() {
        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            vec![item("Attention Is All You Need!", None)],
        );
        assert_eq!(ranked[0].score, 1.0);
    }

    /// A subtitled version of the right paper should rank above an unrelated
    /// title that merely shares words.
    #[test]
    fn a_prefix_match_outranks_a_word_salad_match() {
        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            vec![
                item(
                    "All You Need Is Attention For Everything",
                    Some("10.1234/a"),
                ),
                item(
                    "Attention Is All You Need: A Retrospective",
                    Some("10.1234/b"),
                ),
            ],
        );
        assert_eq!(ranked[0].id.as_ref().unwrap().value(), "10.1234/b");
    }

    #[test]
    fn duplicates_across_providers_collapse_to_the_first() {
        let mut first = item("Attention Is All You Need", Some("10.5555/x"));
        first.source = "crossref".into();
        let mut second = item("Attention is all you need", Some("10.5555/x"));
        second.source = "openalex".into();

        let ranked = rank(&SearchQuery::new("attention"), vec![first, second]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].item.source, "crossref");
    }

    /// Different papers must not be collapsed just because they rank alike.
    #[test]
    fn distinct_identifiers_are_kept() {
        let ranked = rank(
            &SearchQuery::new("attention"),
            vec![
                item("Attention Is All You Need", Some("10.1234/a")),
                item("Attention Is All You Need", Some("10.1234/b")),
            ],
        );
        assert_eq!(ranked.len(), 2);
    }

    /// A result nobody can add is worth showing but not worth ranking above
    /// one that can be.
    #[test]
    fn records_without_an_identifier_sink() {
        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            vec![
                item("Attention Is All You Need", None),
                item("Attention Is All You Need", Some("10.1234/real")),
            ],
        );
        assert!(ranked[0].id.is_some(), "an unusable result ranked first");
    }

    #[test]
    fn author_and_year_corroborate_without_overriding() {
        let query = SearchQuery {
            author: Some("Vaswani".into()),
            year: Some(2017),
            ..SearchQuery::new("attention is all you need")
        };
        let mut right = item("Attention Is All You Need", Some("10.1234/right"));
        right.author = vec![crate::formats::csl::CslName {
            family: Some("Vaswani".into()),
            given: Some("Ashish".into()),
            ..Default::default()
        }];
        right.issued = Some(serde_json::from_str(r#"{"date-parts":[[2017]]}"#).unwrap());

        // A wrong title with the right author must still lose.
        let mut decoy = item("Something Else Entirely", Some("10.1234/decoy"));
        decoy.author = right.author.clone();
        decoy.issued = right.issued.clone();

        let ranked = rank(&query, vec![decoy, right]);
        assert_eq!(ranked[0].id.as_ref().unwrap().value(), "10.1234/right");
    }

    #[test]
    fn the_limit_is_honoured() {
        let items: Vec<CslItem> = (0..20)
            .map(|i| item(&format!("Paper {i}"), Some(&format!("10.1234/{i}"))))
            .collect();
        let query = SearchQuery {
            limit: 5,
            ..SearchQuery::new("paper")
        };
        assert_eq!(rank(&query, items).len(), 5);
    }

    #[test]
    fn identifiers_are_recovered_from_doi_url_or_isbn() {
        let mut arxiv = item("A Preprint", None);
        arxiv.url = Some("https://arxiv.org/abs/1706.03762".into());
        assert_eq!(
            identifier_of(&arxiv).map(|i| i.to_string()).as_deref(),
            Some("arxiv:1706.03762")
        );

        let mut book = item("A Book", None);
        book.isbn = Some(Flexible::Text("0201896834".into()));
        assert_eq!(
            identifier_of(&book).map(|i| i.to_string()).as_deref(),
            Some("isbn:9780201896831")
        );

        assert!(identifier_of(&item("Nothing", None)).is_none());
    }

    fn from(source: &str, title: &str, doi: &str) -> CslItem {
        CslItem {
            source: source.to_owned(),
            ..item(title, Some(doi))
        }
    }

    /// A widely-indexed title returns many exact matches. Without interleaving,
    /// the first provider fills every visible slot and the others never appear
    /// — which is what happened live for "attention is all you need": five
    /// near-identical Crossref rows, arXiv nowhere.
    #[test]
    fn results_are_interleaved_across_providers() {
        let mut items = Vec::new();
        for i in 0..4 {
            items.push(from(
                "crossref",
                "Attention Is All You Need",
                &format!("10.1234/c{i}"),
            ));
        }
        for i in 0..4 {
            items.push(from(
                "arxiv",
                "Attention Is All You Need",
                &format!("10.1234/a{i}"),
            ));
        }
        let query = SearchQuery {
            limit: 4,
            ..SearchQuery::new("attention is all you need")
        };
        let ranked = rank(&query, items);
        let sources: Vec<&str> = ranked.iter().map(|c| c.item.source.as_str()).collect();
        assert_eq!(sources, ["crossref", "arxiv", "crossref", "arxiv"]);
    }

    /// Among identically-titled works, the one the literature actually cites.
    #[test]
    fn citation_count_breaks_a_title_tie() {
        let mut obscure = item("Attention Is All You Need", Some("10.1234/obscure"));
        obscure.cited_by = Some(2);
        let mut canonical = item("Attention Is All You Need", Some("10.1234/canonical"));
        canonical.cited_by = Some(90000);

        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            vec![obscure, canonical],
        );
        assert_eq!(ranked[0].id.as_ref().unwrap().value(), "10.1234/canonical");
    }

    /// Bucketed, so a marginal citation difference cannot outweigh the rest.
    #[test]
    fn similar_citation_counts_do_not_decide() {
        let mut a = item("A Paper", Some("10.1234/a"));
        a.cited_by = Some(412);
        let mut b = item("A Paper", Some("10.1234/b"));
        b.cited_by = Some(419);
        assert_eq!(citation_bucket(&a), citation_bucket(&b));
    }

    /// Crossref `posted-content` is frequently a third party re-posting a paper
    /// that exists properly elsewhere, matching the title exactly.
    #[test]
    fn a_re_posting_loses_a_tie_to_a_real_publication() {
        let mut reposted = item("Attention Is All You Need", Some("10.1234/reposted"));
        reposted.kind = Some("posted-content".into());
        let published = item("Attention Is All You Need", Some("10.1234/published"));

        let ranked = rank(
            &SearchQuery::new("attention is all you need"),
            vec![reposted, published],
        );
        assert_eq!(ranked[0].id.as_ref().unwrap().value(), "10.1234/published");
    }

    /// arXiv preprints must not be demoted: for a computer-science paper the
    /// preprint is often the canonical artifact, and sometimes the only one
    /// with a resolvable identifier.
    #[test]
    fn arxiv_preprints_are_not_treated_as_re_postings() {
        let mut preprint = item("A Paper", None);
        preprint.kind = Some("manuscript".into());
        assert_eq!(
            published_rank(&preprint),
            published_rank(&item("A Paper", None))
        );
    }

    /// A record with a venue and a date beats a bare one.
    #[test]
    fn completeness_breaks_a_remaining_tie() {
        let bare = item("A Paper", Some("10.1234/bare"));
        let mut rich = item("A Paper", Some("10.1234/rich"));
        rich.container_title = Some(Flexible::Text("A Journal".into()));
        rich.issued = Some(serde_json::from_str(r#"{"date-parts":[[2017]]}"#).unwrap());
        rich.page = Some(Flexible::Text("1-10".into()));

        let ranked = rank(&SearchQuery::new("a paper"), vec![bare, rich]);
        assert_eq!(ranked[0].id.as_ref().unwrap().value(), "10.1234/rich");
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::encode;

    /// Titles carry punctuation that changes a URL's meaning if it survives.
    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("attention is all"), "attention%20is%20all");
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode("10.1002/andp"), "10.1002%2Fandp");
        assert_eq!(encode("\"quoted\""), "%22quoted%22");
        // Unreserved characters pass through untouched.
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    /// Non-ASCII must become UTF-8 percent triplets, not be dropped.
    #[test]
    fn non_ascii_is_encoded_as_utf8() {
        assert_eq!(encode("Körper"), "K%C3%B6rper");
    }
}
