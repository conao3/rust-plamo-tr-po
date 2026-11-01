//! A minimal PO reader and writer that edits translations in place.
//!
//! Every entry is kept as its original lines. Only the `msgstr`, the `fuzzy`
//! flag, the previous-msgid comments, and the machine translation comment of
//! entries that receive a new translation are rewritten, so everything else in
//! the file survives byte for byte.

use anyhow::{Result, bail};

pub const MACHINE_COMMENT: &str = "# Machine-translated by PLaMo Translate.";

pub struct PoFile {
    entries: Vec<Entry>,
}

pub struct Entry {
    lines: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Header,
    Obsolete,
    Plural,
    Untranslated,
    Fuzzy,
    Translated,
}

impl PoFile {
    pub fn parse(text: &str) -> Result<Self> {
        let mut entries = Vec::new();
        let mut lines = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                if !lines.is_empty() {
                    entries.push(Entry {
                        lines: std::mem::take(&mut lines),
                    });
                }
            } else {
                lines.push(line.to_string());
            }
        }
        if !lines.is_empty() {
            entries.push(Entry { lines });
        }
        for entry in &entries {
            entry.check()?;
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry_mut(&mut self, index: usize) -> &mut Entry {
        &mut self.entries[index]
    }

    pub fn language(&self) -> Option<String> {
        let header = self.entries.iter().find(|e| e.state() == State::Header)?;
        header.msgstr().lines().find_map(|line| {
            line.strip_prefix("Language:")
                .map(|language| language.trim().to_string())
                .filter(|language| !language.is_empty())
        })
    }

    pub fn to_text(&self) -> String {
        let mut text = self
            .entries
            .iter()
            .map(|entry| entry.lines.join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        text.push('\n');
        text
    }
}

impl Entry {
    fn check(&self) -> Result<()> {
        if self.is_obsolete() {
            return Ok(());
        }
        let has_msgid = self.field("msgid").is_some();
        let has_msgstr = self.field("msgstr").is_some() || self.field("msgstr[0]").is_some();
        if has_msgid != has_msgstr {
            bail!("malformed PO entry:\n{}", self.lines.join("\n"));
        }
        Ok(())
    }

    fn is_obsolete(&self) -> bool {
        self.lines.iter().all(|line| line.starts_with('#'))
            && self.lines.iter().any(|line| line.starts_with("#~"))
    }

    /// The range of lines holding `keyword` and its continuation strings.
    fn field(&self, keyword: &str) -> Option<std::ops::Range<usize>> {
        let prefix = format!("{keyword} \"");
        let start = self
            .lines
            .iter()
            .position(|line| line.starts_with(&prefix))?;
        let end = self.lines[start + 1..]
            .iter()
            .position(|line| !line.starts_with('"'))
            .map_or(self.lines.len(), |offset| start + 1 + offset);
        Some(start..end)
    }

    fn string(&self, keyword: &str) -> String {
        let Some(range) = self.field(keyword) else {
            return String::new();
        };
        let first = &self.lines[range.start][keyword.len() + 1..];
        std::iter::once(first)
            .chain(
                self.lines[range.start + 1..range.end]
                    .iter()
                    .map(|l| l.as_str()),
            )
            .map(|quoted| unescape(quoted.trim().trim_matches('"')))
            .collect()
    }

    pub fn msgid(&self) -> String {
        self.string("msgid")
    }

    pub fn msgstr(&self) -> String {
        self.string("msgstr")
    }

    fn flags(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|line| line.strip_prefix("#,"))
            .flat_map(|flags| flags.split(','))
            .map(|flag| flag.trim().to_string())
            .filter(|flag| !flag.is_empty())
            .collect()
    }

    /// The extracted comments (`#.` lines) without their marker.
    pub fn extracted_comments(&self) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .filter_map(|line| line.strip_prefix("#."))
            .map(str::trim)
    }

    pub fn is_machine_translated(&self) -> bool {
        self.lines.iter().any(|line| line == MACHINE_COMMENT)
    }

    pub fn state(&self) -> State {
        if self.is_obsolete() {
            State::Obsolete
        } else if self.field("msgid_plural").is_some() {
            State::Plural
        } else if self.msgid().is_empty() {
            State::Header
        } else if self.msgstr().is_empty() {
            State::Untranslated
        } else if self.flags().iter().any(|flag| flag == "fuzzy") {
            State::Fuzzy
        } else {
            State::Translated
        }
    }

    /// Store a machine translation: replace the msgstr, clear `fuzzy` and the
    /// previous msgid, and mark the entry with [`MACHINE_COMMENT`].
    pub fn set_machine_translation(&mut self, translation: &str) {
        let range = self.field("msgstr").expect("entry without msgstr");
        self.lines.splice(range, encode("msgstr", translation));

        let flags: Vec<String> = self
            .flags()
            .into_iter()
            .filter(|flag| flag != "fuzzy")
            .collect();
        let flags_at = self.lines.iter().position(|line| line.starts_with("#,"));
        self.lines.retain(|line| !line.starts_with("#,"));
        self.lines.retain(|line| !line.starts_with("#|"));
        if let Some(at) = flags_at
            && !flags.is_empty()
        {
            self.lines.insert(at, format!("#, {}", flags.join(", ")));
        }
        if !self.is_machine_translated() {
            self.lines.insert(0, MACHINE_COMMENT.to_string());
        }
    }
}

fn unescape(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some(other) => result.push(other),
            None => result.push('\\'),
        }
    }
    result
}

fn escape(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            other => result.push(other),
        }
    }
    result
}

fn encode(keyword: &str, text: &str) -> Vec<String> {
    let pieces: Vec<&str> = text.split_inclusive('\n').collect();
    if pieces.len() <= 1 {
        return vec![format!("{keyword} \"{}\"", escape(text))];
    }
    std::iter::once(format!("{keyword} \"\""))
        .chain(
            pieces
                .into_iter()
                .map(|piece| format!("\"{}\"", escape(piece))),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# SOME DESCRIPTIVE TITLE
msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\n"
"Language: ja\n"

#. type: Content of: <html><body><h3>
#: Fonts.html:52
msgid "Fonts"
msgstr ""

#: Fonts.html:60
#, fuzzy, no-wrap
#| msgid "The <code>default</code> face."
msgid ""
"The <code>default</code> face is used "
"for \"ordinary\" text."
msgstr "古い訳"

#~ msgid "Gone"
#~ msgstr "消えた"
"#;

    #[test]
    fn reads_states_and_strings() {
        let po = PoFile::parse(SAMPLE).unwrap();
        let states: Vec<State> = po.entries().iter().map(Entry::state).collect();
        assert_eq!(
            states,
            [
                State::Header,
                State::Untranslated,
                State::Fuzzy,
                State::Obsolete
            ]
        );
        assert_eq!(po.language().as_deref(), Some("ja"));
        assert_eq!(
            po.entries()[2].msgid(),
            "The <code>default</code> face is used for \"ordinary\" text."
        );
    }

    #[test]
    fn keeps_untouched_entries_byte_for_byte() {
        let po = PoFile::parse(SAMPLE).unwrap();
        assert_eq!(po.to_text(), SAMPLE);
    }

    #[test]
    fn stores_machine_translation() {
        let mut po = PoFile::parse(SAMPLE).unwrap();
        po.entry_mut(2)
            .set_machine_translation("<code>default</code> フェイスは\n\"通常の\"テキストに使う。");
        let entry = &po.entries()[2];
        assert_eq!(entry.state(), State::Translated);
        assert!(entry.is_machine_translated());
        assert_eq!(
            entry.lines,
            [
                MACHINE_COMMENT,
                "#: Fonts.html:60",
                "#, no-wrap",
                "msgid \"\"",
                "\"The <code>default</code> face is used \"",
                "\"for \\\"ordinary\\\" text.\"",
                "msgstr \"\"",
                "\"<code>default</code> フェイスは\\n\"",
                "\"\\\"通常の\\\"テキストに使う。\"",
            ]
        );
        let reparsed = PoFile::parse(&po.to_text()).unwrap();
        assert_eq!(
            reparsed.entries()[2].msgstr(),
            "<code>default</code> フェイスは\n\"通常の\"テキストに使う。"
        );
    }

    #[test]
    fn drops_flag_line_when_only_fuzzy() {
        let mut po =
            PoFile::parse("#, fuzzy\n#| msgid \"Old\"\nmsgid \"New\"\nmsgstr \"古い\"\n").unwrap();
        po.entry_mut(0).set_machine_translation("新しい");
        assert_eq!(
            po.to_text(),
            format!("{MACHINE_COMMENT}\nmsgid \"New\"\nmsgstr \"新しい\"\n")
        );
    }
}
