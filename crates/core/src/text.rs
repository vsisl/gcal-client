//! Text helpers for content that comes from Google.

/// Reduces an event description to plain text.
///
/// Descriptions may carry a small subset of HTML (bold, links, lists, line breaks). Until
/// the formatted view arrives, tags are dropped, breaks and block ends become new lines,
/// and the common character references are decoded.
pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            // A lone "<" is just text.
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let tag = rest[open + 1..open + close].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let ends_line = name == "br"
            || (tag.starts_with('/') && matches!(name, "p" | "div" | "li" | "ul" | "ol"));
        if ends_line {
            out.push('\n');
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);

    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        // Last, so that "&amp;lt;" becomes "&lt;" and not "<".
        .replace("&amp;", "&");
    decoded.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(
            plain_text("Bring the slides\nRoom 2"),
            "Bring the slides\nRoom 2"
        );
        assert_eq!(plain_text("if a < b then"), "if a < b then");
    }

    #[test]
    fn tags_are_dropped_and_breaks_become_new_lines() {
        assert_eq!(
            plain_text("<b>Agenda</b><br>1. Budget<br/>2. <a href=\"https://x.example\">Plan</a>"),
            "Agenda\n1. Budget\n2. Plan"
        );
        assert_eq!(plain_text("<ul><li>one</li><li>two</li></ul>"), "one\ntwo");
    }

    #[test]
    fn character_references_are_decoded_once() {
        assert_eq!(plain_text("Tom &amp; Jerry&nbsp;&lt;3"), "Tom & Jerry <3");
        assert_eq!(plain_text("&amp;lt;"), "&lt;");
    }
}
