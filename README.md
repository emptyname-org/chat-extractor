# Chat Extractor for Signal

Chat Extractor for Signal is an offline Rust/GTK app that extracts selected chats from a Signal Desktop export. All processing stays local.

This independent project is not affiliated with Signal Messenger LLC.

## Features

- Uses native GTK controls, theme, and fonts.
- Guides Signal Desktop export creation.
- Finds `main.jsonl` in the selected folder.
- Remembers the last source and destination folders locally.
- Hides empty contacts and chats containing only one technical update.
- Filters chat names and sorts by Name, Messages, or Date.
- Offers checkboxes plus filtered Select all and Select none actions.
- Uses `YYYY-MM-DD` fields with optional GTK calendars.
- Exports Markdown or JSON with optional media.
- Defaults to one file per chat; combined export is optional.
- Suggests filenames from each chat and last-message date.
- Confirms replacements and installs them transactionally.
- Prevents media-name collisions between outputs.
- Creates owner-only output on Unix.
- Uses a four-step installer-style workflow.

## Requirements

- Rust toolchain
- GTK 4.8+ development files
- `pkg-config`
- `xvfb-run` (for GTK tests)

On Debian or Ubuntu, install the system packages with:

```sh
sudo apt install libgtk-4-dev pkg-config xvfb
```

Run this yourself; the project never invokes `sudo`.

## Run

From the repository root:

```sh
cargo run --release
```

Or build and run the local binary:

```sh
make build
./dist/chatextractor
```

## Debian package

Download the current AMD64 Debian/Ubuntu package from the
[latest release](https://github.com/emptyname-org/chat-extractor/releases/latest), then install it:

```sh
sudo apt install ./chatextractor_0.1.0-1_amd64.deb
```

Run this yourself. To build the package locally:

```sh
make deb
make validate-package
```

The package is written to `dist/`. Other architectures use the matching package from `make deb`.

## Workflow

In Signal Desktop, open **Settings → Chats → Export chat history**, choose a folder, and wait for completion.

0. Follow the guide to export chat history from Signal Desktop.
1. Select the export folder; `main.jsonl` is found automatically.
2. Filter, sort, and select chats.
3. Set the inclusive date range, files, and format, then select **Export**. One output opens a Save chooser; multiple files open a folder chooser.

Media goes in a sibling `<output-name>-media` folder. Combined exports use numbered media subfolders to prevent name collisions. Single or separate files default to `<chat-name>-YYYY-MM-DD`; combined files use `N-conversations-YYYY-MM-DD`. Save suggestions are editable; separate files use each chat name and last-message date.

## Development

```sh
make build
make test
make lint
make audit
make source-audit
make validate-package
```

Streaming I/O scans the archive once per export. Output is built in a private staging folder. Existing files move only after all new output is complete and are restored on failure. Staging data is always removed.

## Preparing a public repository

The source audit checks an allowlist and rejects unexpected files and private-machine paths without printing matches. Run it before staging or publishing:

```sh
make source-audit
git status --short
```

Use only synthetic test fixtures. Never add real Signal exports, media, preferences, logs, `.env` files, credentials, or generated `target/` or `dist/` content. CI repeats source, test, lint, dependency, and package checks.

## License

[Free as Air Licence](https://emptyname.org/faal/) — CC0 1.0 Universal. No copyright, restrictions, or attribution required.
