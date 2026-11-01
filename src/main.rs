mod po;
mod protect;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use plamo_tr::api::{Auth, Format, Options, Translator};
use plamo_tr::lang::{Direction, parse_language, resolve};
use regex::Regex;

use crate::po::{PoFile, State};
use crate::protect::{Protected, protect};

/// Fill gettext PO files with translations from PLaMo Translate.
///
/// Untranslated entries, and fuzzy entries that carry the machine translation
/// comment, are translated and marked with that comment. Fuzzy entries without
/// it hold a human translation and are left for a person to review.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// PO files to translate in place.
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Source language of the msgids (auto-detected when omitted).
    #[arg(short, long, value_parser = parse_language)]
    source: Option<&'static str>,

    /// Target language (the Language header of each file when omitted).
    #[arg(short, long, value_parser = parse_language)]
    target: Option<&'static str>,

    /// Markup of the msgids. With html, the contents of code, kbd, pre, samp,
    /// and var elements are not translated, and a translation that changes the
    /// tags is rejected.
    #[arg(long, value_enum, default_value_t = MarkupArg::Plain)]
    format: MarkupArg,

    /// Number of msgids sent in one request.
    #[arg(long, default_value_t = 20)]
    batch_size: usize,

    /// Upper bound on the characters sent in one request.
    #[arg(long, default_value_t = 4000)]
    batch_chars: usize,

    /// Number of requests in flight.
    #[arg(short, long, default_value_t = 4)]
    jobs: usize,

    /// Translate at most this many distinct msgids, then stop.
    #[arg(long)]
    limit: Option<usize>,

    /// Write the files after this many seconds of work, and at the end.
    #[arg(long, default_value_t = 60)]
    save_every: u64,

    /// JSON file with the apiKey and refreshToken of a PLaMo Translate login.
    #[arg(long, env = "PLAMO_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Leave entries untranslated when one of their extracted comments (`#.`)
    /// matches this regular expression, such as `<pre>` for code examples.
    #[arg(long, value_name = "REGEX")]
    skip_context: Option<Regex>,

    /// Only report how many entries would be translated.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum MarkupArg {
    Plain,
    Html,
}

impl From<MarkupArg> for Format {
    fn from(markup: MarkupArg) -> Self {
        match markup {
            MarkupArg::Plain => Format::Plain,
            MarkupArg::Html => Format::Html,
        }
    }
}

struct Target {
    file: usize,
    entry: usize,
}

fn tag_signature(text: &str) -> Vec<String> {
    let tag = Regex::new(r"<(/?)([A-Za-z][A-Za-z0-9-]*)").expect("valid regex");
    let mut tags: Vec<String> = tag
        .captures_iter(text)
        .map(|c| format!("{}{}", &c[1], c[2].to_ascii_lowercase()))
        .collect();
    tags.sort();
    tags
}

fn acceptable(source: &str, translation: &str, format: Format) -> bool {
    !translation.trim().is_empty()
        && (format == Format::Plain || tag_signature(source) == tag_signature(translation))
}

fn translate_one_batch(
    translator: &Translator,
    texts: &[&str],
    options: &Options,
) -> Result<Vec<Option<String>>> {
    if options.format == Format::Plain {
        let translations = translator.translate_batch(texts, options)?;
        return Ok(texts
            .iter()
            .zip(translations)
            .map(|(text, translation)| {
                let translation = translation.trim().to_string();
                acceptable(text, &translation, options.format).then_some(translation)
            })
            .collect());
    }
    let protected: Vec<Protected> = texts.iter().map(|text| protect(text)).collect();
    let masked: Vec<&str> = protected.iter().map(|p| p.text.as_str()).collect();
    let translations = translator.translate_batch(&masked, options)?;
    Ok(texts
        .iter()
        .zip(&protected)
        .zip(translations)
        .map(|((text, protected), translation)| {
            let restored = protected.restore(translation.trim())?;
            acceptable(text, &restored, options.format).then_some(restored)
        })
        .collect())
}

fn translate_texts(
    translator: &Translator,
    texts: &[&str],
    options: &Options,
) -> Vec<(String, Option<String>)> {
    let mut results = Vec::with_capacity(texts.len());
    match translate_one_batch(translator, texts, options) {
        Ok(translations) => {
            for (text, translation) in texts.iter().zip(translations) {
                match translation {
                    Some(translation) => results.push((text.to_string(), Some(translation))),
                    None if texts.len() > 1 => {
                        results.extend(translate_texts(translator, &[text], options));
                    }
                    None => {
                        eprintln!("rejected translation of: {text}");
                        results.push((text.to_string(), None));
                    }
                }
            }
        }
        Err(error) if texts.len() > 1 => {
            eprintln!(
                "batch of {} failed: {error:#}; retrying one by one",
                texts.len()
            );
            for text in texts {
                results.extend(translate_texts(translator, &[text], options));
            }
        }
        Err(error) => {
            eprintln!("failed to translate: {text}: {error:#}", text = texts[0]);
            results.push((texts[0].to_string(), None));
        }
    }
    results
}

fn batches(texts: Vec<String>, size: usize, chars: usize) -> VecDeque<Vec<String>> {
    let mut batches = VecDeque::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_chars = 0;
    for text in texts {
        let length = text.chars().count();
        if !current.is_empty() && (current.len() >= size || current_chars + length > chars) {
            batches.push_back(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars += length;
        current.push(text);
    }
    if !current.is_empty() {
        batches.push_back(current);
    }
    batches
}

fn save(path: &Path, file: &PoFile) -> Result<()> {
    let temporary = path.with_extension("po.tmp");
    std::fs::write(&temporary, file.to_text())?;
    std::fs::rename(&temporary, path).with_context(|| format!("failed to write {}", path.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let format = Format::from(cli.format);
    if cli.batch_size == 0 || cli.jobs == 0 {
        bail!("--batch-size and --jobs must be at least 1");
    }

    let mut files = Vec::new();
    for path in &cli.files {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let file =
            PoFile::parse(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        files.push(file);
    }

    let mut targets: HashMap<&'static str, HashMap<String, Vec<Target>>> = HashMap::new();
    let mut order: HashMap<&'static str, Vec<String>> = HashMap::new();
    let mut human_fuzzy = 0;
    let mut skipped = 0;
    for (index, file) in files.iter().enumerate() {
        let target = match (cli.target, file.language()) {
            (Some(target), _) => target,
            (None, Some(language)) => parse_language(&language).map_err(anyhow::Error::msg)?,
            (None, None) => bail!(
                "{} has no Language header; pass --target",
                cli.files[index].display()
            ),
        };
        for (entry_index, entry) in file.entries().iter().enumerate() {
            let pending = match entry.state() {
                State::Untranslated => true,
                State::Fuzzy if entry.is_machine_translated() => true,
                State::Fuzzy => {
                    human_fuzzy += 1;
                    false
                }
                _ => false,
            };
            if !pending {
                continue;
            }
            if let Some(skip) = &cli.skip_context
                && entry
                    .extracted_comments()
                    .any(|comment| skip.is_match(comment))
            {
                skipped += 1;
                continue;
            }
            let msgid = entry.msgid();
            let by_text = targets.entry(target).or_default();
            if !by_text.contains_key(&msgid) {
                order.entry(target).or_default().push(msgid.clone());
            }
            by_text.entry(msgid).or_default().push(Target {
                file: index,
                entry: entry_index,
            });
        }
    }

    let total: usize = order.values().map(Vec::len).sum();
    let entries: usize = targets
        .values()
        .flat_map(|t| t.values())
        .map(Vec::len)
        .sum();
    eprintln!(
        "{total} distinct msgids in {entries} entries to translate; {skipped} entries skipped by --skip-context; {human_fuzzy} fuzzy entries with a human translation left as is"
    );
    if cli.dry_run || total == 0 {
        return Ok(());
    }

    let Some(auth_file) = &cli.auth_file else {
        bail!("pass --auth-file or set PLAMO_AUTH_FILE");
    };
    let translator = Translator::sign_in(&Auth::load(auth_file)?)?;
    let mut remaining = cli.limit.unwrap_or(usize::MAX);
    let mut translated = 0;
    let mut rejected = 0;
    for (target, mut texts) in order {
        if remaining == 0 {
            break;
        }
        texts.truncate(remaining);
        remaining -= texts.len();
        let direction = match cli.source {
            Some(source) => resolve(Some(source), Some(target), "")?,
            None => Direction {
                source: None,
                target,
            },
        };
        let options = Options {
            direction: &direction,
            format,
            writing_style: None,
        };
        let queue = Mutex::new(batches(texts, cli.batch_size, cli.batch_chars));
        let batch_count = queue.lock().unwrap().len();
        let (sender, receiver) = mpsc::channel::<Vec<(String, Option<String>)>>();
        let by_text = &targets[target];

        std::thread::scope(|scope| -> Result<()> {
            for _ in 0..cli.jobs {
                let sender = sender.clone();
                let queue = &queue;
                let translator = &translator;
                let options = &options;
                scope.spawn(move || {
                    loop {
                        let Some(batch) = queue.lock().unwrap().pop_front() else {
                            return;
                        };
                        let texts: Vec<&str> = batch.iter().map(String::as_str).collect();
                        if sender
                            .send(translate_texts(translator, &texts, options))
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
            drop(sender);

            let mut dirty = HashSet::new();
            let mut last_save = Instant::now();
            let mut done = 0;
            for results in receiver {
                done += 1;
                for (text, translation) in results {
                    let Some(translation) = translation else {
                        rejected += 1;
                        continue;
                    };
                    translated += 1;
                    for target in &by_text[&text] {
                        files[target.file]
                            .entry_mut(target.entry)
                            .set_machine_translation(&translation);
                        dirty.insert(target.file);
                    }
                }
                if last_save.elapsed() >= Duration::from_secs(cli.save_every) {
                    for &index in &dirty {
                        save(&cli.files[index], &files[index])?;
                    }
                    dirty.clear();
                    last_save = Instant::now();
                    eprintln!(
                        "{done}/{batch_count} batches into {target}; {translated} translated"
                    );
                }
            }
            for &index in &dirty {
                save(&cli.files[index], &files[index])?;
            }
            Ok(())
        })?;
    }
    eprintln!("{translated} msgids translated, {rejected} left untranslated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_signature_ignores_order_and_attributes() {
        assert_eq!(
            tag_signature(r#"Type <kbd class="kbd">C-x</kbd> in <a href="x.html">Files</a>"#),
            tag_signature(r#"<a href="x.html">ファイル</a>で<kbd>C-x</kbd>と入力"#)
        );
        assert_ne!(tag_signature("<code>a</code>"), tag_signature("a"));
    }

    #[test]
    fn rejects_empty_or_tag_changing_translations() {
        assert!(acceptable("<b>x</b>", "<b>エックス</b>", Format::Html));
        assert!(!acceptable("<b>x</b>", "エックス", Format::Html));
        assert!(acceptable("<b>x</b>", "エックス", Format::Plain));
        assert!(!acceptable("x", "  ", Format::Plain));
    }

    #[test]
    fn splits_batches_by_count_and_characters() {
        let texts: Vec<String> = ["aaaa", "bb", "cccc", "d", "e"].map(String::from).into();
        let sizes: Vec<usize> = batches(texts, 3, 6).iter().map(Vec::len).collect();
        assert_eq!(sizes, [2, 3]);
    }
}
