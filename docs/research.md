# Starting-point assessment

Public repositories were reviewed on 19 August 2026.

## JenBytes/signal-single-chat-extractor

[signal-single-chat-extractor](https://github.com/JenBytes/signal-single-chat-extractor)
is the closest small project and is MIT-licensed. It confirmed that Signal
Desktop plaintext exports use newline-delimited JSON with
`recipient`, `chat`, and `chatItem` frames.

It was not used as a base:

- it is a 238-line command-line script without tests;
- the README lists media export and an HTML viewer as future work;
- it offers JSON or plain text, but no Markdown, GUI, or date range;
- malformed JSON and broad errors can silently drop messages;
- it treats recipient IDs as `chatId` values despite Signal separating them; and
- nested sticker/update handling misses top-level items.

This app maps each `chat.id -> recipientId` and tests differing IDs.

## Larger backup tools

[signalbackup-tools](https://github.com/bepaald/signalbackup-tools) is a mature,
tested GPL-3.0 CLI for encrypted Signal Android backups. It handles decryption,
repair, merging, import, and several formats, but is not a small base for this
desktop plaintext-export GUI.

[signal-back](https://github.com/xeals/signal-back) also targets encrypted
Android backups, with XML/CSV/raw output and attachment extraction. It does not
support current Signal Desktop plaintext exports or this GUI.

## Official format references

The implementation follows the current public Signal sources:

- [Signal Desktop's plaintext export workflow](https://github.com/signalapp/Signal-Desktop/blob/main/ts/services/backups/index.preload.ts)
  writes `main.jsonl`, `metadata.json`, and optional decrypted files under `files/`.
- [Signal Desktop's local attachment exporter](https://github.com/signalapp/Signal-Desktop/blob/main/ts/jobs/AttachmentLocalBackupManager.preload.ts)
  gives plaintext attachments a MIME/filename-derived extension.
- [Signal Desktop's media-name helper](https://github.com/signalapp/Signal-Desktop/blob/main/ts/services/backups/util/mediaId.preload.ts)
  hashes the plaintext hash plus local key, then uses a two-character subfolder.
- [libsignal's JSON exporter](https://github.com/signalapp/libsignal/blob/main/rust/message-backup/src/json/exporter.rs)
  emits one JSON object per line and omits disappearing messages.
- [The public takeout JSONL fixture](https://github.com/signalapp/libsignal/blob/main/rust/message-backup/tests/res/canonical-backup.takeout-export.expected.jsonl)
  confirms the frame-level JSON.

These files served only as format documentation; no AGPL code was copied.

## Choice

A new Rust implementation provides:

- a native compiled executable;
- bounded-memory JSONL streaming;
- testable filesystem and media handling;
- native GTK 4 controls and typography; and
- no runtime language or package manager beyond GTK.
