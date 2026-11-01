//! Keep code in HTML msgids out of the translation.
//!
//! Elements such as `<code>` and `<kbd>` are replaced with `<x-N/>`
//! placeholders before the text is sent, and put back afterwards.

const PROTECTED: &[&str] = &["code", "kbd", "pre", "samp", "var"];

pub struct Protected {
    pub text: String,
    pieces: Vec<String>,
}

fn tag_name_at(text: &str, at: usize) -> Option<(&str, bool)> {
    let rest = &text[at + 1..];
    let (closing, rest) = match rest.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .unwrap_or(rest.len());
    (end > 0).then(|| (&rest[..end], closing))
}

/// Byte offset just past the element that opens at `start`, counting nested
/// elements of the same name.
fn element_end(text: &str, start: usize, name: &str) -> Option<usize> {
    let mut depth = 0;
    let mut at = start;
    while let Some(offset) = text[at..].find('<') {
        let position = at + offset;
        let close = position + text[position..].find('>')? + 1;
        if let Some((tag, closing)) = tag_name_at(text, position)
            && tag.eq_ignore_ascii_case(name)
        {
            if closing {
                depth -= 1;
                if depth == 0 {
                    return Some(close);
                }
            } else if !text[position..close].ends_with("/>") {
                depth += 1;
            }
        }
        at = close;
    }
    None
}

pub fn protect(text: &str) -> Protected {
    let mut masked = String::with_capacity(text.len());
    let mut pieces = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find('<') {
        let position = at + offset;
        masked.push_str(&text[at..position]);
        let protected_end = match tag_name_at(text, position) {
            Some((tag, false)) if PROTECTED.iter().any(|name| name.eq_ignore_ascii_case(tag)) => {
                element_end(text, position, tag)
            }
            _ => None,
        };
        match protected_end {
            Some(end) => {
                masked.push_str(&format!("<x-{}/>", pieces.len()));
                pieces.push(text[position..end].to_string());
                at = end;
            }
            None => {
                masked.push('<');
                at = position + 1;
            }
        }
    }
    masked.push_str(&text[at..]);
    Protected {
        text: masked,
        pieces,
    }
}

impl Protected {
    /// Put the protected elements back into `translation`, or `None` when a
    /// placeholder is missing or repeated.
    pub fn restore(&self, translation: &str) -> Option<String> {
        let mut restored = translation.to_string();
        for (index, piece) in self.pieces.iter().enumerate() {
            let placeholder = format!("<x-{index}/>");
            if restored.matches(&placeholder).count() != 1 {
                return None;
            }
            restored = restored.replacen(&placeholder, piece, 1);
        }
        Some(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_restores_code() {
        let source = r#"Type <kbd class="kbd">C-u <kbd class="key">RET</kbd></kbd> or set <code class="code">font</code>."#;
        let protected = protect(source);
        assert_eq!(protected.text, "Type <x-0/> or set <x-1/>.");
        assert_eq!(
            protected
                .restore("<x-1/> を設定するか <x-0/> と打つ。")
                .as_deref(),
            Some(
                r#"<code class="code">font</code> を設定するか <kbd class="kbd">C-u <kbd class="key">RET</kbd></kbd> と打つ。"#
            )
        );
    }

    #[test]
    fn rejects_lost_or_doubled_placeholders() {
        let protected = protect("<code>a</code> and <code>b</code>");
        assert_eq!(protected.restore("<x-0/> だけ"), None);
        assert_eq!(protected.restore("<x-0/> <x-0/> <x-1/>"), None);
    }

    #[test]
    fn leaves_other_tags_and_unclosed_elements() {
        let protected = protect(r#"See <a href="x.html">Files</a> and <code>open"#);
        assert_eq!(
            protected.text,
            r#"See <a href="x.html">Files</a> and <code>open"#
        );
    }
}
