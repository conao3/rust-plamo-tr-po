# po-plamo

Fill gettext PO files with machine translations from [PLaMo Translate](https://translate.preferredai.jp/), using the [plamo-tr](https://github.com/conao3/rust-plamo-tr) library.

This is an unofficial tool and is not affiliated with Preferred Networks.

## Usage

```console
$ po-plamo --source en --format html --skip-context "<(pre|style|script)>" po/emacs.ja.po
108 distinct msgids in 108 entries to translate; 13 entries skipped by --skip-context; 0 fuzzy entries with a human translation left as is
108 msgids translated, 0 left untranslated
```

`po-plamo` edits the files in place:

- It translates untranslated entries, and fuzzy entries that it translated before. Each translation gets the translator comment `# Machine-translated by PLaMo Translate.`, the `fuzzy` flag and the previous msgid (`#|`) are removed.
- Fuzzy entries without that comment hold a human translation, and are left for a person to review. Remove the comment from an entry after you correct its translation by hand.
- The same msgid is translated once, even when it appears in several entries or files.
- Lines of entries that do not get a new translation are written back unchanged.
- The target language is the `Language` header of each file unless `--target` is given.
- Progress is written every `--save-every` seconds, so an interrupted run keeps what it translated.

| Option | Meaning |
|---|---|
| `-s`, `--source <LANG>` | Source language of the msgids. Detected by PLaMo Translate when omitted. |
| `-t`, `--target <LANG>` | Target language. Defaults to the `Language` header. |
| `--format <plain\|html>` | Markup of the msgids. With `html`, the contents of `code`, `kbd`, `pre`, `samp`, and `var` elements are sent as placeholders and kept as they are, and a translation that changes the other tags is rejected. |
| `--skip-context <REGEX>` | Leave entries untranslated when an extracted comment (`#.`) matches, such as `<pre>` for code blocks extracted by po4a. |
| `--batch-size`, `--batch-chars` | How many msgids and characters go into one request (20 and 4000). |
| `-j`, `--jobs` | Requests in flight (4). |
| `--limit <N>` | Stop after N distinct msgids. |
| `--dry-run` | Only count the entries to translate. |
| `--auth-file <PATH>` | Login file of PLaMo Translate (`PLAMO_AUTH_FILE`). See the plamo-tr README for how to create it. |

A rejected or failed batch is retried one msgid at a time. A msgid whose translation is still rejected stays untranslated, so tools such as po4a keep the original text for it.

## Install

```console
$ nix run github:conao3/rust-po-plamo -- --help
$ cargo install --git https://github.com/conao3/rust-po-plamo
```

## Development

```console
$ nix develop
$ make check
```

## License

Apache-2.0
