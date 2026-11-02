# AGENTS.md

`po-plamo` is a Rust CLI that fills gettext PO files with machine translations from PLaMo Translate through the `plamo-tr` library (`https://github.com/conao3/rust-plamo-tr`).

## Layout

- `src/main.rs`: the command definitions (clap derive), collecting the entries to translate, batching, worker threads, periodic saving, and the output summary.
- `src/grammar.rs`, `src/output.rs`, `src/error.rs`: the AWS CLI grammar (`file://` values, a trailing `help`, `--cli-input-json`, `--generate-cli-skeleton`), `--output` and `--query` rendering, and exit codes 252/254/255.
- `src/po.rs`: a line-preserving PO reader and writer. Only entries that receive a translation are rewritten.
- `src/protect.rs`: placeholders (`<x-N/>`) for code elements in HTML msgids.

## Conventions

- Machine translations are marked with the translator comment `# Machine-translated by PLaMo Translate.` Fuzzy entries without it are human translations and are never overwritten.
- Commit messages, comments, and docs are in English.

## Verification

```console
$ nix develop --command make check
$ nix build
```

The tests do not call the API. To try a real run, point `PLAMO_AUTH_FILE` at an auth file (plamo-tr README "Login") and run `cargo run -- po-file translate-po-files --po-files <file.po>` on a copy of a PO file.

When working on `plamo-tr` at the same time, a local `.cargo/config.toml` with `[patch."https://github.com/conao3/rust-plamo-tr"] plamo-tr = { path = "../rust-plamo-tr" }` builds against the checkout. Do not commit it.
