//! Parsing poppler's output formats.
//!
//! Kept separate from the backend so these run against recorded fixtures,
//! which is how most identification tests avoid needing poppler at all.

use std::collections::BTreeMap;

/// `pdfinfo` output: `Key:            value`, one per line.
pub fn info(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                map.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    map
}

/// Page count from a parsed info dictionary.
pub fn page_count(info: &BTreeMap<String, String>) -> Option<usize> {
    info.get("Pages")?.parse().ok()
}

/// `pdfinfo -url` output: a header line, then `%4d  Annotation    <uri>`.
pub fn urls(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(page), Some(kind)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(page) = page.parse::<usize>() else {
            // The header row, which has no page number.
            continue;
        };
        if kind != "Annotation" {
            continue;
        }
        let uri = parts.collect::<Vec<_>>().join(" ");
        if !uri.is_empty() {
            found.push((page, uri));
        }
    }
    found
}

/// Flatten an XMP packet to `local-name -> text`, last value winning.
///
/// Namespaces are deliberately ignored: publishers disagree about prefixes
/// (`prism:doi`, `pdfx:doi`, `crossmark:doi`) but agree about local names, and
/// we only need to know a DOI when we see one.
pub fn xmp(xml: &str) -> BTreeMap<String, String> {
    use quick_xml::events::Event;

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                // Attributes carry metadata too, in the compact XMP form
                // (`<rdf:Description pdfx:doi="10.…"/>`).
                for attribute in e.attributes().flatten() {
                    let key = local_name(attribute.key.as_ref());
                    if let Ok(value) = attribute.unescape_value() {
                        let value = value.trim().to_owned();
                        if !value.is_empty() {
                            out.insert(key, value);
                        }
                    }
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                for attribute in e.attributes().flatten() {
                    let key = local_name(attribute.key.as_ref());
                    if let Ok(value) = attribute.unescape_value() {
                        let value = value.trim().to_owned();
                        if !value.is_empty() {
                            out.insert(key, value);
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let (Some(name), Ok(raw)) = (stack.last(), e.decode())
                    && let Ok(text) = quick_xml::escape::unescape(&raw)
                {
                    let text = text.trim().to_owned();
                    if !text.is_empty() {
                        out.insert(name.clone(), text);
                    }
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            // A malformed or truncated packet is a soft failure: keep whatever
            // was read before the break rather than discarding all of it.
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    name.rsplit(':')
        .next()
        .unwrap_or(&name)
        .to_ascii_lowercase()
}

/// Split `pdftotext` output into pages on the form-feed separator.
pub fn pages(text: &str) -> Vec<&str> {
    text.split('\u{c}').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDFINFO: &str = "\
Title:           Zur Elektrodynamik bewegter Körper
Subject:         doi:10.1002/andp.19053221004
Producer:        Acrobat Distiller
Pages:           31
Page size:       595 x 842 pts
";

    #[test]
    fn info_lines_become_key_value_pairs() {
        let map = info(PDFINFO);
        assert_eq!(map.get("Pages").map(String::as_str), Some("31"));
        assert_eq!(page_count(&map), Some(31));
        assert!(map["Title"].starts_with("Zur Elektrodynamik"));
    }

    /// `Page size: 595 x 842 pts` has no colon problem, but a value containing
    /// a colon (a DOI, a URL) must keep everything after the first one.
    #[test]
    fn values_may_contain_colons() {
        let map = info("Subject:  doi:10.1002/andp\nURL: https://example.org/x\n");
        assert_eq!(map["Subject"], "doi:10.1002/andp");
        assert_eq!(map["URL"], "https://example.org/x");
    }

    const PDFURLS: &str = "\
Page  Type          URL
   1  Annotation    https://doi.org/10.1002/andp.19053221004
   1  Annotation    mailto:someone@example.org
   3  Annotation    https://example.org/figure
";

    #[test]
    fn url_lines_are_parsed_with_their_page() {
        let found = urls(PDFURLS);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].0, 1);
        assert_eq!(found[0].1, "https://doi.org/10.1002/andp.19053221004");
        assert_eq!(found[2].0, 3);
    }

    #[test]
    fn the_url_header_row_is_skipped() {
        assert!(urls("Page  Type          URL\n").is_empty());
    }

    const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
      xmlns:prism="http://prismstandard.org/namespaces/basic/2.0/"
      prism:doi="10.1002/andp.19053221004">
   <dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">
     Zur Elektrodynamik bewegter Körper
   </dc:title>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn xmp_yields_values_from_both_attributes_and_elements() {
        let map = xmp(XMP);
        assert_eq!(
            map.get("doi").map(String::as_str),
            Some("10.1002/andp.19053221004")
        );
        assert_eq!(
            map.get("title").map(String::as_str),
            Some("Zur Elektrodynamik bewegter Körper")
        );
    }

    /// A truncated packet must yield what it can rather than nothing.
    #[test]
    fn a_broken_xmp_packet_degrades() {
        let broken = r#"<x:xmpmeta><rdf:Description prism:doi="10.1000/x">"#;
        assert_eq!(
            xmp(broken).get("doi").map(String::as_str),
            Some("10.1000/x")
        );
    }

    #[test]
    fn pages_split_on_form_feeds() {
        assert_eq!(pages("one\u{c}two\u{c}three"), ["one", "two", "three"]);
    }
}
