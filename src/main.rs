mod error;
mod grammar;
mod output;
mod po;
mod protect;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use plamo_tr::api::{Auth, Format, Options, Translator};
use plamo_tr::lang::{Direction, parse_language, resolve};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::grammar::{CliInputArgs, SkeletonKind};
use crate::output::{OutputFormat, Response};
use crate::po::{PoFile, State};
use crate::protect::{Protected, protect};

#[derive(Parser)]
#[command(
    name = "po-plamo",
    version,
    about = "Fill gettext PO files with translations from PLaMo Translate"
)]
struct Cli {
    /// Output format
    #[arg(long, global = true, help_heading = "Global Options", value_enum, default_value_t = OutputFormat::Json)]
    output: OutputFormat,
    /// JMESPath expression applied to the output
    #[arg(
        long,
        global = true,
        help_heading = "Global Options",
        value_name = "JMESPATH"
    )]
    query: Option<String>,
    /// Print debug logs to stderr
    #[arg(long, global = true, help_heading = "Global Options")]
    debug: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Translate the entries of gettext PO files
    #[command(subcommand, name = "po-file")]
    PoFile(PoFileCommand),
}

#[derive(Subcommand)]
enum PoFileCommand {
    /// Translate untranslated entries, and fuzzy entries that carry the machine translation comment, in place.
    /// Fuzzy entries without the comment hold a human translation and are left for a person to review.
    /// Progress goes to stderr
    #[command(name = "translate-po-files")]
    TranslatePoFiles(Box<TranslatePoFilesArgs>),
}

#[derive(Args)]
struct TranslatePoFilesArgs {
    /// PO files to translate in place (space-separated)
    #[arg(long, num_args = 1.., required = true)]
    po_files: Vec<PathBuf>,

    /// Source language of the msgids (auto-detected when omitted)
    #[arg(long, value_parser = parse_language)]
    source_language: Option<&'static str>,

    /// Target language (the Language header of each file when omitted)
    #[arg(long, value_parser = parse_language)]
    target_language: Option<&'static str>,

    /// Markup of the msgids. With html, the contents of code, kbd, pre, samp, and var elements are not translated, and a translation that changes the tags is rejected
    #[arg(long, value_enum, default_value_t = MarkupArg::Plain)]
    format: MarkupArg,

    /// Number of msgids sent in one request
    #[arg(long, default_value_t = 20)]
    batch_size: usize,

    /// Upper bound on the characters sent in one request
    #[arg(long, default_value_t = 4000)]
    batch_chars: usize,

    /// Number of requests in flight
    #[arg(long, default_value_t = 4)]
    jobs: usize,

    /// Translate at most this many distinct msgids, then stop
    #[arg(long)]
    limit: Option<usize>,

    /// Write the files after this many seconds of work, and at the end
    #[arg(long, default_value_t = 60)]
    save_every: u64,

    /// JSON file with the apiKey and refreshToken of a PLaMo Translate login
    #[arg(long, env = "PLAMO_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Leave entries untranslated when one of their extracted comments (`#.`) matches this regular expression, such as `<pre>` for code examples
    #[arg(long, value_name = "REGEX", allow_hyphen_values = true)]
    skip_context: Option<Regex>,

    /// Only report how many entries would be translated
    #[arg(long, overrides_with = "no_dry_run")]
    dry_run: bool,

    /// Turn off --dry-run
    #[arg(long, overrides_with = "dry_run")]
    no_dry_run: bool,

    #[command(flatten)]
    input: CliInputArgs,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "PascalCase")]
struct TranslatePoFilesOutput {
    po_files: Vec<String>,
    dry_run: bool,
    pending_msgid_count: usize,
    pending_entry_count: usize,
    skipped_entry_count: usize,
    human_fuzzy_entry_count: usize,
    translated_msgid_count: usize,
    rejected_msgid_count: usize,
}

#[derive(Debug)]
struct TargetError(String);

impl std::fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TargetError {}

fn target_error(message: String) -> anyhow::Error {
    anyhow::Error::new(TargetError(message))
}

fn classify(error: anyhow::Error) -> error::Error {
    let message = format!("{error:#}");
    if error.chain().any(|cause| cause.is::<TargetError>()) {
        error::target(message)
    } else {
        error::other(message)
    }
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

fn translate_po_files(args: &TranslatePoFilesArgs) -> Result<TranslatePoFilesOutput> {
    let format = Format::from(args.format);
    let mut output = TranslatePoFilesOutput {
        po_files: args
            .po_files
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        dry_run: args.dry_run,
        ..TranslatePoFilesOutput::default()
    };

    let mut files = Vec::new();
    for path in &args.po_files {
        let text = std::fs::read_to_string(path)
            .map_err(|error| target_error(format!("failed to read {}: {error}", path.display())))?;
        let file = PoFile::parse(&text).map_err(|error| {
            target_error(format!("failed to parse {}: {error:#}", path.display()))
        })?;
        files.push(file);
    }

    let mut targets: HashMap<&'static str, HashMap<String, Vec<Target>>> = HashMap::new();
    let mut order: HashMap<&'static str, Vec<String>> = HashMap::new();
    for (index, file) in files.iter().enumerate() {
        let target = match (args.target_language, file.language()) {
            (Some(target), _) => target,
            (None, Some(language)) => parse_language(&language).map_err(|error| {
                target_error(format!(
                    "{}: Language header: {error}",
                    args.po_files[index].display()
                ))
            })?,
            (None, None) => {
                return Err(target_error(format!(
                    "{} has no Language header; pass --target-language",
                    args.po_files[index].display()
                )));
            }
        };
        for (entry_index, entry) in file.entries().iter().enumerate() {
            let pending = match entry.state() {
                State::Untranslated => true,
                State::Fuzzy if entry.is_machine_translated() => true,
                State::Fuzzy => {
                    output.human_fuzzy_entry_count += 1;
                    false
                }
                _ => false,
            };
            if !pending {
                continue;
            }
            if let Some(skip) = &args.skip_context
                && entry
                    .extracted_comments()
                    .any(|comment| skip.is_match(comment))
            {
                output.skipped_entry_count += 1;
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

    output.pending_msgid_count = order.values().map(Vec::len).sum();
    output.pending_entry_count = targets
        .values()
        .flat_map(|t| t.values())
        .map(Vec::len)
        .sum();
    eprintln!(
        "{} distinct msgids in {} entries to translate; {} entries skipped by --skip-context; {} fuzzy entries with a human translation left as is",
        output.pending_msgid_count,
        output.pending_entry_count,
        output.skipped_entry_count,
        output.human_fuzzy_entry_count
    );
    if args.dry_run || output.pending_msgid_count == 0 {
        return Ok(output);
    }

    let auth_file = args.auth_file.as_ref().context("auth file is required")?;
    let auth = Auth::load(auth_file)?;
    let translator = Translator::sign_in(&auth)
        .map_err(|error| target_error(format!("PLaMo Translate sign-in failed: {error:#}")))?;
    let mut remaining = args.limit.unwrap_or(usize::MAX);
    let mut translated = 0;
    let mut rejected = 0;
    for (target, mut texts) in order {
        if remaining == 0 {
            break;
        }
        texts.truncate(remaining);
        remaining -= texts.len();
        let direction = match args.source_language {
            Some(source) => resolve(Some(source), Some(target), "")
                .map_err(|error| target_error(format!("{error:#}")))?,
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
        let queue = Mutex::new(batches(texts, args.batch_size, args.batch_chars));
        let batch_count = queue.lock().unwrap().len();
        let (sender, receiver) = mpsc::channel::<Vec<(String, Option<String>)>>();
        let by_text = &targets[target];

        std::thread::scope(|scope| -> Result<()> {
            for _ in 0..args.jobs {
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
                if last_save.elapsed() >= Duration::from_secs(args.save_every) {
                    for &index in &dirty {
                        save(&args.po_files[index], &files[index])?;
                    }
                    dirty.clear();
                    last_save = Instant::now();
                    eprintln!(
                        "{done}/{batch_count} batches into {target}; {translated} translated"
                    );
                }
            }
            for &index in &dirty {
                save(&args.po_files[index], &files[index])?;
            }
            Ok(())
        })?;
    }
    output.translated_msgid_count = translated;
    output.rejected_msgid_count = rejected;
    Ok(output)
}

fn to_value<T: Serialize>(value: &T) -> error::Result<Value> {
    serde_json::to_value(value).map_err(|error| error::other(error.to_string()))
}

fn output_skeleton(path: &[String]) -> error::Result<Value> {
    match path.last().map(String::as_str) {
        Some("translate-po-files") => to_value(&TranslatePoFilesOutput {
            po_files: vec![String::new()],
            ..TranslatePoFilesOutput::default()
        }),
        _ => Err(error::parameter(
            "--generate-cli-skeleton needs a command and a subcommand",
        )),
    }
}

fn run() -> error::Result<()> {
    let mut root = Cli::command();
    root.build();
    let prepared = grammar::prepare(&root, std::env::args_os().collect())?;
    if prepared.argv.iter().any(|token| token == "--debug") {
        eprintln!("debug: argv {:?}", &prepared.argv[1..]);
    }

    if let Some(kind) = prepared.skeleton {
        let mut command = &root;
        for name in &prepared.path {
            command = command
                .find_subcommand(name)
                .ok_or_else(|| error::other(format!("unknown subcommand {name}")))?;
        }
        if command.has_subcommands() {
            return Err(error::parameter(
                "--generate-cli-skeleton needs a command and a subcommand",
            ));
        }
        let value = match kind {
            SkeletonKind::Input => grammar::input_skeleton(command),
            SkeletonKind::Output => output_skeleton(&prepared.path)?,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| error::other(error.to_string()))?
        );
        return Ok(());
    }

    let cli = match Cli::try_parse_from(&prepared.argv) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 252 } else { 0 };
            let _ = error.print();
            std::process::exit(code);
        }
    };

    if let Some(query) = &cli.query {
        output::compile_query(query)?;
    }
    let Command::PoFile(PoFileCommand::TranslatePoFiles(args)) = cli.command;
    if args.batch_size == 0 || args.jobs == 0 {
        return Err(error::parameter(
            "--batch-size and --jobs must be at least 1",
        ));
    }
    if args.auth_file.is_none() && !args.dry_run {
        return Err(error::parameter(
            "pass --auth-file or set PLAMO_AUTH_FILE (or use --dry-run)",
        ));
    }
    let value = to_value(&translate_po_files(&args).map_err(classify)?)?;
    print!(
        "{}",
        output::render(
            "TranslatePoFiles",
            Response { value, text: None },
            cli.output,
            cli.query.as_deref()
        )?
    );
    Ok(())
}

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    };
    std::process::exit(code);
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
