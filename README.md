# Chat Extractor for Signal

Chat Extractor for Signal is an offline Rust/GTK app that extracts selected chats from a Signal Desktop export. All processing stays local.

This independent project is not affiliated with Signal Messenger LLC.

## Features

- Reads Signal Desktop exports without uploading data.
- Selects one or more chats, with name filtering and sorting.
- Exports an inclusive date range.
- Writes Markdown or JSON as separate files or one combined file.
- Optionally copies referenced media alongside the export.

## Install

For 64-bit Intel/AMD Debian or Ubuntu, download
[chatextractor_0.1.0-1_amd64.deb](https://github.com/emptyname-org/chat-extractor/releases/latest/download/chatextractor_0.1.0-1_amd64.deb).
Open the downloaded file with your package installer, or run this in its folder:

```sh
sudo apt install ./chatextractor_0.1.0-1_amd64.deb
```

Then open **Chat Extractor for Signal** from the applications menu.

## Workflow

In Signal Desktop, open **Settings → Chats → Export chat history**, choose a folder, and wait for completion.

0. Follow the guide to export chat history from Signal Desktop.
1. Select the export folder; `main.jsonl` is found automatically.
2. Filter, sort, and select chats.
3. Set the inclusive date range, files, and format, then select **Export**. One output opens a Save chooser; multiple files open a folder chooser.

Media goes in a sibling `<output-name>-media` folder. Combined exports use numbered media subfolders to prevent name collisions. Single or separate files default to `<chat-name>-YYYY-MM-DD`; combined files use `N-conversations-YYYY-MM-DD`. Save suggestions are editable; separate files use each chat name and last-message date.

## Build from source

Building requires Rust, GTK 4.8 development files, `pkg-config`, and `xvfb-run`
for GTK tests. On Debian or Ubuntu:

```sh
sudo apt install libgtk-4-dev pkg-config xvfb
make build
./dist/chatextractor
```

Project checks:

```sh
make test
make lint
make audit
make source-audit
make validate-package
```

## License

[Free as Air Licence](https://emptyname.org/faal/) — CC0 1.0 Universal. No copyright, restrictions, or attribution required.
