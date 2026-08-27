//! Recovering a title from page geometry.
//!
//! When no identifier can be extracted, the title is the only thing left to
//! search on — and the document info dictionary is frequently empty, as it is
//! for every paper built with pdfTeX. `pdftotext -bbox-layout` emits XHTML with
//! a bounding box per line, and line height is a good proxy for font size, so
//! the largest text in the upper part of page one is the probable title.
//!
//! This is dramatically better than "the first non-empty line", which on a
//! typical paper is a licence notice, a conference banner or an arXiv stamp.

use quick_xml::events::Event;

/// One extracted line with the geometry needed to rank it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub y_min: f64,
    pub y_max: f64,
}

impl Line {
    /// Proxy for font size.
    pub fn height(&self) -> f64 {
        self.y_max - self.y_min
    }
}

#[derive(Debug, Default)]
pub struct Page {
    pub height: f64,
    pub lines: Vec<Line>,
}

/// Parse `pdftotext -bbox-layout` output for a single page.
pub fn parse(xhtml: &str) -> Page {
    let mut reader = quick_xml::Reader::from_str(xhtml);
    reader.config_mut().trim_text(true);

    let mut page = Page::default();
    let mut current: Option<Line> = None;
    let mut words: Vec<String> = Vec::new();
    let mut in_word = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()).as_str() {
                "page" => {
                    page.height = attribute(&e, "height").unwrap_or(792.0);
                }
                "line" => {
                    current = Some(Line {
                        text: String::new(),
                        y_min: attribute(&e, "yMin").unwrap_or(0.0),
                        y_max: attribute(&e, "yMax").unwrap_or(0.0),
                    });
                    words.clear();
                }
                "word" => in_word = true,
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_word
                    && let Ok(raw) = e.decode()
                    && let Ok(text) = quick_xml::escape::unescape(&raw)
                {
                    words.push(text.trim().to_owned());
                }
            }
            Ok(Event::End(e)) => match local_name(e.name().as_ref()).as_str() {
                "word" => in_word = false,
                "line" => {
                    if let Some(mut line) = current.take() {
                        line.text = words.join(" ").trim().to_owned();
                        if !line.text.is_empty() {
                            page.lines.push(line);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    page
}

/// The probable title of `page`.
///
/// The tallest lines in the upper region win, and adjacent lines of the same
/// height are joined — a title that wraps is still one title.
pub fn title(page: &Page) -> Option<String> {
    // Titles live above the midpoint. Searching the whole page would let a
    // large section heading or a figure caption win.
    let cutoff = page.height * 0.6;
    let upper: Vec<&Line> = page
        .lines
        .iter()
        .filter(|l| l.y_min < cutoff && l.height() > 0.0)
        .collect();
    if upper.is_empty() {
        return None;
    }

    let tallest = upper.iter().map(|l| l.height()).fold(f64::MIN, f64::max);

    // Rendering jitter means "the same size" is not exact equality.
    let same_size = |line: &Line| (line.height() - tallest).abs() < tallest * 0.08;

    // The first run of same-height lines, so a title above a same-sized
    // section heading further down does not absorb it.
    let mut collected: Vec<&str> = Vec::new();
    let mut started = false;
    let mut previous_bottom = 0.0;
    for line in &upper {
        if same_size(line) {
            // A gap much larger than a line height means a different block.
            if started && line.y_min - previous_bottom > tallest * 1.5 {
                break;
            }
            collected.push(&line.text);
            previous_bottom = line.y_max;
            started = true;
        } else if started {
            break;
        }
    }

    let title = collected.join(" ").trim().to_owned();
    // A single very short line is a running head or a page number, not a title.
    (title.chars().count() >= 6).then_some(title)
}

fn attribute(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<f64> {
    e.attributes().flatten().find_map(|a| {
        (a.key.as_ref() == name.as_bytes())
            .then(|| a.unescape_value().ok()?.parse().ok())
            .flatten()
    })
}

fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    name.rsplit(':')
        .next()
        .unwrap_or(&name)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, y_min: f64, height: f64) -> String {
        let y_max = y_min + height;
        let words: String = text
            .split_whitespace()
            .map(|w| format!(r#"<word xMin="0" yMin="{y_min}" xMax="9" yMax="{y_max}">{w}</word>"#))
            .collect();
        format!(r#"<line xMin="0" yMin="{y_min}" xMax="9" yMax="{y_max}">{words}</line>"#)
    }

    fn page(lines: &[String]) -> String {
        format!(
            r#"<doc><page width="612" height="792"><flow><block>{}</block></flow></page></doc>"#,
            lines.concat()
        )
    }

    /// The shape of a real paper: a licence notice in small type at the very
    /// top, then the title in large type, then authors.
    #[test]
    fn the_title_is_the_largest_text_not_the_first() {
        let xhtml = page(&[
            line(
                "Provided proper attribution is provided, Google hereby grants",
                73.0,
                10.7,
            ),
            line("permission to reproduce the tables and figures", 87.0, 10.7),
            line("Attention Is All You Need", 150.0, 20.6),
            line("Ashish Vaswani Noam Shazeer", 200.0, 11.9),
        ]);
        assert_eq!(
            title(&parse(&xhtml)).as_deref(),
            Some("Attention Is All You Need")
        );
    }

    /// A title that wraps is still one title.
    #[test]
    fn adjacent_lines_of_the_same_size_are_joined() {
        let xhtml = page(&[
            line("A Very Long Title That Does Not", 150.0, 20.0),
            line("Fit On One Line At All", 172.0, 20.0),
            line("Some Author", 220.0, 11.0),
        ]);
        assert_eq!(
            title(&parse(&xhtml)).as_deref(),
            Some("A Very Long Title That Does Not Fit On One Line At All")
        );
    }

    /// A same-sized heading further down the page is a different block.
    #[test]
    fn a_distant_same_sized_line_is_not_absorbed() {
        let xhtml = page(&[
            line("The Real Title", 100.0, 20.0),
            line("1 Introduction", 400.0, 20.0),
        ]);
        assert_eq!(title(&parse(&xhtml)).as_deref(), Some("The Real Title"));
    }

    /// Text below the midpoint cannot be the title, however large.
    #[test]
    fn the_lower_half_of_the_page_is_ignored() {
        let xhtml = page(&[
            line("Actual Title Up Here", 100.0, 14.0),
            line("ENORMOUS FIGURE CAPTION", 600.0, 40.0),
        ]);
        assert_eq!(
            title(&parse(&xhtml)).as_deref(),
            Some("Actual Title Up Here")
        );
    }

    #[test]
    fn a_page_with_nothing_usable_yields_no_title() {
        assert!(title(&parse(&page(&[]))).is_none());
        // A lone short line is a running head, not a title.
        assert!(title(&parse(&page(&[line("3", 100.0, 12.0)]))).is_none());
    }

    #[test]
    fn malformed_output_does_not_panic() {
        assert!(title(&parse("<doc><page height=\"792\"><line")).is_none());
        assert!(title(&parse("")).is_none());
    }
}
