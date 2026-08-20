use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{Local, NaiveDate, TimeZone};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use signal_filter::{ExportFormat, ExportRequest, build_archive_index, export_conversation};
use tempfile::TempDir;

use signal_filter::{
    ConversationExportMode, MultiExportRequest, export_conversations,
    export_conversations_overwriting,
};

struct Fixture {
    _directory: TempDir,
    source: PathBuf,
    media_bytes: Vec<u8>,
    group_media_bytes: Vec<u8>,
}

fn timestamp(year: i32, month: u32, day: u32, hour: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("test date should be unambiguous")
        .timestamp_millis()
}

fn write_jsonl(path: &Path, values: &[Value]) {
    let mut output = File::create(path).expect("fixture file should be created");
    for value in values {
        serde_json::to_writer(&mut output, value).expect("fixture JSON should serialize");
        writeln!(output).expect("fixture line should be written");
    }
}

fn create_fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source = directory.path().join("signal-export");
    fs::create_dir(&source).expect("source directory should be created");

    let plaintext_hash = vec![0x11; 32];
    let local_key = vec![0x22; 32];
    let media_name = format!(
        "{:x}",
        Sha256::digest([plaintext_hash.clone(), local_key.clone()].concat())
    );
    let media_directory = source.join("files").join(&media_name[..2]);
    fs::create_dir_all(&media_directory).expect("media directory should be created");
    let media_bytes = b"not-a-real-jpeg-but-deterministic".to_vec();
    fs::write(
        media_directory.join(format!("{media_name}.jpeg")),
        &media_bytes,
    )
    .expect("media fixture should be written");

    let group_plaintext_hash = vec![0x33; 32];
    let group_local_key = vec![0x44; 32];
    let group_media_name = format!(
        "{:x}",
        Sha256::digest([group_plaintext_hash.clone(), group_local_key.clone(),].concat())
    );
    let group_media_directory = source.join("files").join(&group_media_name[..2]);
    fs::create_dir_all(&group_media_directory).expect("group media directory should be created");
    let group_media_bytes = b"deterministic-project-image".to_vec();
    fs::write(
        group_media_directory.join(format!("{group_media_name}.png")),
        &group_media_bytes,
    )
    .expect("group media fixture should be written");

    let pointer = json!({
        "contentType": "image/jpeg",
        "fileName": "../../holiday (1).jpg",
        "caption": "A <photo>",
        "width": 640,
        "height": 480,
        "locatorInfo": {
            "localKey": BASE64_STANDARD.encode(local_key),
            "integrityCheck": {
                "plaintextHash": BASE64_STANDARD.encode(plaintext_hash)
            }
        }
    });
    let group_pointer = json!({
        "contentType": "image/png",
        "fileName": "holiday (1).jpg",
        "caption": "Project plan",
        "width": 800,
        "height": 600,
        "locatorInfo": {
            "localKey": BASE64_STANDARD.encode(group_local_key),
            "plaintextHash": BASE64_STANDARD.encode(group_plaintext_hash)
        }
    });
    let lines = vec![
        json!({
            "version": "1",
            "backupTimeMs": timestamp(2024, 1, 4, 12),
            "mediaRootBackupKey": "fixture"
        }),
        json!({"account": {"givenName": "Test", "familyName": "Owner"}}),
        json!({"recipient": {"id": "10", "self": {}}}),
        json!({"recipient": {
            "id": "20",
            "contact": {
                "profileGivenName": "Alex",
                "profileFamilyName": "Example"
            }
        }}),
        json!({"recipient": {
            "id": "30",
            "group": {"snapshot": {"title": {"title": "Project Group"}}}
        }}),
        json!({"chat": {"id": "999", "recipientId": "20"}}),
        json!({"chat": {"id": "888", "recipientId": "30"}}),
        json!({"chatItem": {
            "chatId": "999",
            "authorId": "20",
            "dateSent": timestamp(2024, 1, 1, 12),
            "incoming": {"dateReceived": timestamp(2024, 1, 1, 12)},
            "standardMessage": {
                "text": {"body": "Hello <script>alert(1)</script> *world*"}
            }
        }}),
        json!({"chatItem": {
            "chatId": "888",
            "authorId": "30",
            "dateSent": timestamp(2024, 1, 2, 12),
            "incoming": {"dateReceived": timestamp(2024, 1, 2, 12)},
            "standardMessage": {
                "text": {"body": "Other conversation"},
                "attachments": [{"pointer": group_pointer}]
            }
        }}),
        json!({"chatItem": {
            "chatId": "999",
            "authorId": "10",
            "dateSent": timestamp(2024, 1, 3, 12),
            "outgoing": {"sendStatus": []},
            "standardMessage": {
                "text": {"body": "Photo attached"},
                "attachments": [{"pointer": pointer}],
                "reactions": [{
                    "emoji": "👍",
                    "authorId": "20",
                    "sentTimestamp": timestamp(2024, 1, 3, 13)
                }]
            }
        }}),
    ];
    write_jsonl(&source.join("main.jsonl"), &lines);

    Fixture {
        _directory: directory,
        source,
        media_bytes,
        group_media_bytes,
    }
}

fn expected_conversation(id: &str) -> (&'static str, &'static [&'static str], &'static str) {
    match id {
        "999" => (
            "Alex Example",
            &["Hello <script>alert(1)</script> *world*", "Photo attached"],
            "0001-holiday (1).jpg",
        ),
        "888" => (
            "Project Group",
            &["Other conversation"],
            "0001-holiday (1).jpg",
        ),
        _ => panic!("unexpected fixture conversation: {id}"),
    }
}

fn expected_media_path(document: &Path, expected_ids: &[&str], id: &str) -> String {
    let stem = document
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("test output should have a UTF-8 stem");
    let file_name = expected_conversation(id).2;
    if expected_ids.len() > 1 {
        let position = expected_ids
            .iter()
            .position(|candidate| *candidate == id)
            .expect("expected conversation should have a position");
        format!("{stem}-media/conversation-{:04}/{file_name}", position + 1)
    } else {
        format!("{stem}-media/{file_name}")
    }
}

fn assert_json_export(path: &Path, expected_ids: &[&str], include_media: bool) {
    let root: Value = serde_json::from_reader(BufReader::new(
        File::open(path).expect("JSON export should open"),
    ))
    .expect("JSON export should be valid");
    let documents = root["conversation_exports"]
        .as_array()
        .map_or_else(|| vec![&root], |exports| exports.iter().collect::<Vec<_>>());
    assert_eq!(documents.len(), expected_ids.len());

    for expected_id in expected_ids {
        let (expected_name, expected_messages, _) = expected_conversation(expected_id);
        let expected_media_path = expected_media_path(path, expected_ids, expected_id);
        let document = documents
            .iter()
            .find(|document| document["conversation"]["id"] == *expected_id)
            .unwrap_or_else(|| panic!("conversation {expected_id} should be exported"));

        assert_eq!(document["schema_version"], 1);
        assert_eq!(document["conversation"]["name"], expected_name);
        assert_eq!(document["include_media"], include_media);
        let messages = document["messages"]
            .as_array()
            .expect("messages should be an array");
        assert_eq!(messages.len(), expected_messages.len());
        assert!(
            messages
                .iter()
                .all(|message| message["conversation_id"] == *expected_id)
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message["text"]
                    .as_str()
                    .expect("fixture messages should contain text"))
                .collect::<Vec<_>>(),
            expected_messages
        );

        let attachments = messages
            .iter()
            .flat_map(|message| {
                message["attachments"]
                    .as_array()
                    .expect("attachments should be an array")
            })
            .collect::<Vec<_>>();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["source_available"], true);
        if include_media {
            assert_eq!(attachments[0]["exported_path"], expected_media_path);
        } else {
            assert!(attachments[0]["exported_path"].is_null());
        }

        assert_eq!(
            document["summary"]["messages_exported"],
            expected_messages.len()
        );
        assert_eq!(
            document["summary"]["media_files_copied"],
            usize::from(include_media)
        );
        assert_eq!(document["summary"]["media_files_missing"], 0);
    }
}

fn assert_markdown_export(path: &Path, expected_ids: &[&str], include_media: bool) {
    let markdown = fs::read_to_string(path).expect("Markdown export should be readable UTF-8");
    if expected_ids.len() > 1 {
        assert!(markdown.starts_with("# Signal chat export\n"));
    }

    for expected_id in expected_ids {
        let (expected_name, _, _) = expected_conversation(expected_id);
        let expected_media_path = expected_media_path(path, expected_ids, expected_id);
        assert!(markdown.contains(&format!("# {expected_name}\n")));
        match *expected_id {
            "999" => {
                assert!(markdown.contains("Photo attached"));
                assert!(markdown.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
                assert!(markdown.contains("\\*world\\*"));
            }
            "888" => assert!(markdown.contains("Other conversation")),
            _ => unreachable!("fixture IDs are checked by expected_conversation"),
        }

        if include_media {
            let encoded_path = expected_media_path
                .replace(' ', "%20")
                .replace('(', "%28")
                .replace(')', "%29");
            assert!(markdown.contains(&format!("]({encoded_path})")));
        }
    }

    if include_media {
        assert!(!markdown.contains("available in source; not copied"));
    } else {
        assert_eq!(
            markdown.matches("available in source; not copied").count(),
            expected_ids.len()
        );
    }
}

fn assert_export_document(
    path: &Path,
    format: ExportFormat,
    expected_ids: &[&str],
    include_media: bool,
) {
    assert!(path.is_file(), "expected export file should exist");
    match format {
        ExportFormat::Json => assert_json_export(path, expected_ids, include_media),
        ExportFormat::Markdown => assert_markdown_export(path, expected_ids, include_media),
    }
}

fn assert_media_outputs(
    fixture: &Fixture,
    document: &Path,
    expected_ids: &[&str],
    include_media: bool,
) {
    let media_directory = document.with_file_name(format!(
        "{}-media",
        document
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("test output should have a UTF-8 stem")
    ));
    if !include_media {
        assert!(!media_directory.exists());
        return;
    }
    assert!(media_directory.is_dir());

    for expected_id in expected_ids {
        let relative_path = expected_media_path(document, expected_ids, expected_id);
        let expected_bytes = match *expected_id {
            "999" => &fixture.media_bytes,
            "888" => &fixture.group_media_bytes,
            _ => unreachable!("fixture IDs are checked by expected_conversation"),
        };
        assert_eq!(
            fs::read(
                document
                    .parent()
                    .expect("document should have a parent")
                    .join(relative_path),
            )
            .expect("exported media should be readable"),
            *expected_bytes
        );
    }
}

fn assert_destination_entries(
    destination: &Path,
    expected_file_names: &[String],
    include_media: bool,
) {
    let mut actual = fs::read_dir(destination)
        .expect("destination should be readable")
        .map(|entry| {
            entry
                .expect("destination entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    actual.sort();

    let mut expected = expected_file_names.to_vec();
    if include_media {
        expected.extend(expected_file_names.iter().map(|file_name| {
            let stem = Path::new(file_name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("expected filename should have a UTF-8 stem");
            format!("{stem}-media")
        }));
    }
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn actual_single_conversation_export_has_valid_content_exact_name_and_optional_media() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");

    for format in [ExportFormat::Json, ExportFormat::Markdown] {
        for include_media in [false, true] {
            let destination = tempfile::tempdir().expect("destination should be created");
            let file_name = format!("alex-example-2024-01-03.{}", format.extension());
            let output_file = destination.path().join(&file_name);
            let request = MultiExportRequest {
                conversation_ids: vec![String::from("999")],
                start_date: None,
                end_date: None,
                include_media,
                format,
                output_file: output_file.clone(),
                mode: ConversationExportMode::Separate,
            };

            let result = export_conversations(&index, &request)
                .expect("single-conversation export should succeed");
            assert_eq!(result.output_files, vec![output_file.clone()]);
            assert_eq!(result.messages_exported, 2);
            assert_eq!(result.media_copied, u64::from(include_media));
            assert_eq!(result.media_missing, 0);
            assert_export_document(&output_file, format, &["999"], include_media);
            assert_media_outputs(&fixture, &output_file, &["999"], include_media);
            assert_destination_entries(destination.path(), &[file_name], include_media);
        }
    }
}

#[test]
fn actual_combined_export_has_valid_conversations_exact_name_and_optional_media() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");

    for format in [ExportFormat::Json, ExportFormat::Markdown] {
        for include_media in [false, true] {
            let destination = tempfile::tempdir().expect("destination should be created");
            let file_name = format!("2-conversations-2024-01-03.{}", format.extension());
            let output_file = destination.path().join(&file_name);
            let request = MultiExportRequest {
                conversation_ids: vec![String::from("999"), String::from("888")],
                start_date: None,
                end_date: None,
                include_media,
                format,
                output_file: output_file.clone(),
                mode: ConversationExportMode::Combined,
            };

            let result =
                export_conversations(&index, &request).expect("combined export should succeed");
            assert_eq!(result.output_files, vec![output_file.clone()]);
            assert_eq!(result.messages_exported, 3);
            assert_eq!(result.media_copied, 2 * u64::from(include_media));
            assert_eq!(result.media_missing, 0);
            assert_export_document(&output_file, format, &["999", "888"], include_media);
            assert_media_outputs(&fixture, &output_file, &["999", "888"], include_media);
            assert_destination_entries(destination.path(), &[file_name], include_media);
        }
    }
}

#[test]
fn actual_separate_export_has_valid_files_exact_names_and_optional_media() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");

    for format in [ExportFormat::Json, ExportFormat::Markdown] {
        for include_media in [false, true] {
            let destination = tempfile::tempdir().expect("destination should be created");
            let extension = format.extension();
            let alex_name = format!("alex-example-2024-01-03.{extension}");
            let project_name = format!("project-group-2024-01-02.{extension}");
            let alex_file = destination.path().join(&alex_name);
            let project_file = destination.path().join(&project_name);
            let request = MultiExportRequest {
                conversation_ids: vec![String::from("999"), String::from("888")],
                start_date: None,
                end_date: None,
                include_media,
                format,
                output_file: destination.path().join(format!("2024-01-03.{extension}")),
                mode: ConversationExportMode::Separate,
            };

            let result =
                export_conversations(&index, &request).expect("separate export should succeed");
            assert_eq!(
                result.output_files,
                vec![alex_file.clone(), project_file.clone()]
            );
            assert_eq!(result.messages_exported, 3);
            assert_eq!(result.media_copied, 2 * u64::from(include_media));
            assert_eq!(result.media_missing, 0);
            assert_export_document(&alex_file, format, &["999"], include_media);
            assert_export_document(&project_file, format, &["888"], include_media);
            assert_media_outputs(&fixture, &alex_file, &["999"], include_media);
            assert_media_outputs(&fixture, &project_file, &["888"], include_media);
            assert_destination_entries(
                destination.path(),
                &[alex_name, project_name],
                include_media,
            );
        }
    }
}

#[test]
fn indexes_chat_ids_separately_from_recipient_ids() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");

    let direct = index
        .conversations
        .get("999")
        .expect("chat id should be indexed");
    assert_eq!(direct.recipient_id, "20");
    assert_eq!(direct.name, "Alex Example");
    assert_eq!(direct.message_count, 2);
    assert!(!index.conversations.contains_key("20"));
    assert_eq!(index.account_name, "Test Owner");
}

#[test]
fn exports_inclusive_date_range_as_json_and_copies_media() {
    let fixture = create_fixture();
    let destination = tempfile::tempdir().expect("destination should be created");
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let request = ExportRequest {
        conversation_id: String::from("999"),
        start_date: Some(NaiveDate::from_ymd_opt(2024, 1, 3).expect("valid date")),
        end_date: Some(NaiveDate::from_ymd_opt(2024, 1, 3).expect("valid date")),
        include_media: true,
        format: ExportFormat::Json,
        destination: destination.path().to_path_buf(),
    };

    let result = export_conversation(&index, &request).expect("export should succeed");
    assert_eq!(result.messages_exported, 1);
    assert_eq!(result.media_copied, 1);
    assert_eq!(result.media_missing, 0);

    let document: Value = serde_json::from_reader(BufReader::new(
        File::open(&result.output_file).expect("JSON output should exist"),
    ))
    .expect("JSON output should parse");
    let messages = document["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["author"]["name"], "Test Owner");
    assert_eq!(messages[0]["text"], "Photo attached");
    assert!(
        !messages[0]["signal_data"]
            .to_string()
            .contains("locatorInfo")
    );

    let exported_path = messages[0]["attachments"][0]["exported_path"]
        .as_str()
        .expect("attachment should have an exported path");
    assert!(!exported_path.contains(".."));
    assert_eq!(
        fs::read(result.output_directory.join(exported_path))
            .expect("copied media should be readable"),
        fixture.media_bytes
    );
}

#[test]
fn markdown_escapes_message_html_and_does_not_copy_unrequested_media() {
    let fixture = create_fixture();
    let destination = tempfile::tempdir().expect("destination should be created");
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let request = ExportRequest {
        conversation_id: String::from("999"),
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Markdown,
        destination: destination.path().to_path_buf(),
    };

    let result = export_conversation(&index, &request).expect("export should succeed");
    let markdown = fs::read_to_string(&result.output_file).expect("Markdown should be readable");
    assert_eq!(result.messages_exported, 2);
    assert_eq!(result.media_copied, 0);
    assert!(markdown.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(markdown.contains("\\*world\\*"));
    assert!(markdown.contains("available in source; not copied"));
    assert!(!result.output_directory.join("media").exists());
}

#[test]
fn rejects_a_malformed_jsonl_line_without_silently_skipping_it() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source = directory.path().join("main.jsonl");
    fs::write(
        &source,
        "{\"version\":\"1\",\"backupTimeMs\":\"1\"}\n{not valid json}\n",
    )
    .expect("fixture should be written");

    let error = build_archive_index(&source).expect_err("malformed JSON must fail");
    assert!(error.to_string().contains("line 2"));
}

#[test]
fn identifies_single_technical_updates_for_ui_filtering() {
    let fixture = create_fixture();
    let mut source = std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.source.join("main.jsonl"))
        .expect("fixture source must open");
    for record in [
        json!({
            "recipient": {
                "id": "99001",
                "contact": { "name": { "given": "Empty contact" } }
            }
        }),
        json!({ "chat": { "id": "99001", "recipientId": "99001" } }),
        json!({
            "recipient": {
                "id": "99002",
                "contact": { "name": { "given": "Safety notice only" } }
            }
        }),
        json!({ "chat": { "id": "99002", "recipientId": "99002" } }),
        json!({
            "chatItem": {
                "chatId": "99002",
                "dateSent": "1704067200000",
                "directionless": {},
                "updateMessage": { "simpleUpdate": { "type": "IDENTITY_UPDATE" } }
            }
        }),
        json!({ "chat": { "id": "99003", "recipientId": "99002" } }),
        json!({
            "chatItem": {
                "chatId": "99003",
                "dateSent": "1704067200000",
                "directionless": {},
                "updateMessage": { "simpleUpdate": { "type": "BLOCKED" } }
            }
        }),
        json!({ "chat": { "id": "99004", "recipientId": "99002" } }),
        json!({
            "chatItem": {
                "chatId": "99004",
                "dateSent": "1704067200000",
                "directionless": {},
                "updateMessage": {
                    "profileChange": { "previousName": "Old", "newName": "New" }
                }
            }
        }),
        json!({ "chat": { "id": "99005", "recipientId": "99002" } }),
        json!({
            "chatItem": {
                "chatId": "99005",
                "dateSent": "1704067200000",
                "authorId": "99002",
                "incoming": {},
                "standardMessage": { "text": { "body": "A real message" } }
            }
        }),
    ] {
        writeln!(
            source,
            "{}",
            serde_json::to_string(&record).expect("record must serialize")
        )
        .expect("record must append");
    }
    drop(source);

    let index = build_archive_index(&fixture.source).expect("fixture must index");
    let empty = index
        .conversations
        .get("99001")
        .expect("empty conversation must be indexed");
    assert_eq!(empty.message_count, 0);
    assert!(!empty.is_technical_update_only);
    let identity_only = index
        .conversations
        .get("99002")
        .expect("identity-update conversation must be indexed");
    assert_eq!(identity_only.message_count, 1);
    assert!(identity_only.is_technical_update_only);
    assert!(index.conversations["99003"].is_technical_update_only);
    assert!(index.conversations["99004"].is_technical_update_only);
    assert!(!index.conversations["99005"].is_technical_update_only);
}

#[test]
fn explicit_combined_export_creates_the_requested_file() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture must index");
    let mut conversation_ids = index.conversations.keys().cloned().collect::<Vec<_>>();
    conversation_ids.sort();
    let destination = tempfile::tempdir().expect("destination must exist");
    let output_file = destination.path().join("chosen-name.json");
    let request = MultiExportRequest {
        conversation_ids,
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Json,
        output_file: output_file.clone(),
        mode: ConversationExportMode::Combined,
    };

    let result = export_conversations(&index, &request).expect("combined export must succeed");
    assert_eq!(result.output_files, vec![output_file.clone()]);
    assert!(output_file.is_file());
    let document: serde_json::Value =
        serde_json::from_reader(File::open(output_file).expect("combined output must open"))
            .expect("combined output must be valid JSON");
    assert_eq!(
        document["conversation_exports"]
            .as_array()
            .expect("combined exports must be an array")
            .len(),
        request.conversation_ids.len()
    );
}

#[test]
fn separate_export_creates_sibling_files_from_the_chosen_name() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture must index");
    let mut conversation_ids = index.conversations.keys().cloned().collect::<Vec<_>>();
    conversation_ids.sort();
    let destination = tempfile::tempdir().expect("destination must exist");
    let request = MultiExportRequest {
        conversation_ids,
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Markdown,
        output_file: destination.path().join("chosen-name.md"),
        mode: ConversationExportMode::Separate,
    };

    let result = export_conversations(&index, &request).expect("separate export must succeed");
    assert_eq!(result.output_files.len(), request.conversation_ids.len());
    for output_file in result.output_files {
        assert_eq!(output_file.parent(), Some(destination.path()));
        assert_eq!(
            output_file.extension().and_then(|value| value.to_str()),
            Some("md")
        );
        assert!(
            output_file
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with("chosen-name-"))
        );
        assert!(output_file.is_file());
    }
}

#[test]
fn separate_default_names_use_each_conversations_last_message_date() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture must index");
    let destination = tempfile::tempdir().expect("destination must exist");
    let request = MultiExportRequest {
        conversation_ids: index.conversations.keys().cloned().collect(),
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Markdown,
        output_file: destination.path().join("2024-01-03.md"),
        mode: ConversationExportMode::Separate,
    };

    let result = export_conversations(&index, &request).expect("separate export must succeed");
    let stems = result
        .output_files
        .iter()
        .filter_map(|path| path.file_stem().and_then(|stem| stem.to_str()))
        .collect::<Vec<_>>();
    assert!(stems.iter().any(|stem| stem.ends_with("-2024-01-02")));
    assert!(stems.iter().any(|stem| stem.ends_with("-2024-01-03")));
}

#[test]
fn confirmed_overwrite_replaces_an_existing_export_file() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture must index");
    let destination = tempfile::tempdir().expect("destination must exist");
    let output_file = destination.path().join("existing.json");
    fs::write(&output_file, "old export").expect("existing export must be created");
    let request = MultiExportRequest {
        conversation_ids: index.conversations.keys().cloned().collect(),
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Json,
        output_file: output_file.clone(),
        mode: ConversationExportMode::Combined,
    };

    let error =
        export_conversations(&index, &request).expect_err("unconfirmed overwrite must be rejected");
    assert!(matches!(error, signal_filter::AppError::OutputExists));

    export_conversations_overwriting(&index, &request)
        .expect("confirmed overwrite must replace the file");
    let document: serde_json::Value =
        serde_json::from_reader(File::open(output_file).expect("replacement must open"))
            .expect("replacement must be valid JSON");
    assert!(document["conversation_exports"].is_array());
}

#[test]
fn accepts_an_omitted_default_date_sent_value() {
    let source = tempfile::tempdir().expect("source directory should be created");
    write_jsonl(
        &source.path().join("main.jsonl"),
        &[
            json!({"version": "1", "backupTimeMs": "1"}),
            json!({"recipient": {"id": "10", "self": {}}}),
            json!({"chat": {"id": "20", "recipientId": "10"}}),
            json!({"chatItem": {
                "chatId": "20",
                "authorId": "10",
                "directionless": {},
                "updateMessage": {}
            }}),
        ],
    );

    let index = build_archive_index(source.path()).expect("default timestamp should parse");
    let conversation = index
        .conversations
        .get("20")
        .expect("conversation should be indexed");
    assert_eq!(conversation.message_count, 1);
    assert_eq!(conversation.first_timestamp_ms, None);
    assert_eq!(conversation.last_timestamp_ms, None);

    let destination = tempfile::tempdir().expect("destination should be created");
    let result = export_conversation(
        &index,
        &ExportRequest {
            conversation_id: String::from("20"),
            start_date: Some(NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date")),
            end_date: Some(NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date")),
            include_media: false,
            format: ExportFormat::Json,
            destination: destination.path().to_path_buf(),
        },
    )
    .expect("default timestamp should export");
    assert_eq!(result.messages_exported, 1);

    let multi_destination = tempfile::tempdir().expect("destination should be created");
    let multi_result = export_conversations(
        &index,
        &MultiExportRequest {
            conversation_ids: vec![String::from("20")],
            start_date: Some(NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date")),
            end_date: Some(NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date")),
            include_media: false,
            format: ExportFormat::Json,
            output_file: multi_destination.path().join("missing-date.json"),
            mode: ConversationExportMode::Separate,
        },
    )
    .expect("default timestamp should survive the GUI export date range");
    assert_eq!(multi_result.messages_exported, 1);
}

#[test]
fn identical_attachment_names_are_isolated_and_later_exports_do_not_change_earlier_media() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let destination = tempfile::tempdir().expect("destination should exist");

    let combined = destination.path().join("combined.json");
    export_conversations(
        &index,
        &MultiExportRequest {
            conversation_ids: vec![String::from("999"), String::from("888")],
            start_date: None,
            end_date: None,
            include_media: true,
            format: ExportFormat::Json,
            output_file: combined.clone(),
            mode: ConversationExportMode::Combined,
        },
    )
    .expect("combined export should succeed");
    assert_media_outputs(&fixture, &combined, &["999", "888"], true);

    let first = destination.path().join("first.json");
    export_conversations(
        &index,
        &MultiExportRequest {
            conversation_ids: vec![String::from("999")],
            start_date: None,
            end_date: None,
            include_media: true,
            format: ExportFormat::Json,
            output_file: first.clone(),
            mode: ConversationExportMode::Separate,
        },
    )
    .expect("first single export should succeed");
    let first_media = first
        .parent()
        .expect("output should have a parent")
        .join(expected_media_path(&first, &["999"], "999"));
    let before = fs::read(&first_media).expect("first media should be readable");

    let second = destination.path().join("second.json");
    export_conversations(
        &index,
        &MultiExportRequest {
            conversation_ids: vec![String::from("888")],
            start_date: None,
            end_date: None,
            include_media: true,
            format: ExportFormat::Json,
            output_file: second.clone(),
            mode: ConversationExportMode::Separate,
        },
    )
    .expect("second single export should not require replacing the first");
    assert_eq!(
        fs::read(first_media).expect("first media should remain"),
        before
    );
    assert_media_outputs(&fixture, &second, &["888"], true);
}

#[test]
fn rejects_wrong_extensions_and_destinations_inside_the_loaded_source() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let destination = tempfile::tempdir().expect("destination should exist");
    let base = MultiExportRequest {
        conversation_ids: vec![String::from("999")],
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Json,
        output_file: destination.path().join("wrong.md"),
        mode: ConversationExportMode::Separate,
    };
    assert!(matches!(
        export_conversations(&index, &base),
        Err(signal_filter::AppError::OutputExtensionMismatch)
    ));

    let source_bytes = fs::read(&index.source_file).expect("source should be readable");
    let mut source_request = base.clone();
    source_request.output_file = index.source_file.clone();
    assert!(matches!(
        export_conversations_overwriting(&index, &source_request),
        Err(signal_filter::AppError::ProtectedSourceDestination)
    ));
    assert_eq!(
        fs::read(&index.source_file).expect("source should still be readable"),
        source_bytes
    );

    source_request.output_file = index.export_root.join("files").join("export.json");
    assert!(matches!(
        export_conversations(&index, &source_request),
        Err(signal_filter::AppError::ProtectedSourceDestination)
    ));
}

#[test]
fn incompatible_separate_destination_is_rejected_before_any_existing_file_is_changed() {
    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let destination = tempfile::tempdir().expect("destination should exist");
    let first = destination.path().join("chosen-alex-example.json");
    let second = destination.path().join("chosen-project-group.json");
    fs::write(&first, b"previous export").expect("old export should be written");
    fs::create_dir(&second).expect("incompatible destination should be created");
    let request = MultiExportRequest {
        conversation_ids: vec![String::from("999"), String::from("888")],
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Json,
        output_file: destination.path().join("chosen.json"),
        mode: ConversationExportMode::Separate,
    };

    assert!(matches!(
        export_conversations_overwriting(&index, &request),
        Err(signal_filter::AppError::UnsafeDestination)
    ));
    assert_eq!(
        fs::read(&first).expect("first old export should remain"),
        b"previous export"
    );
    assert!(second.is_dir());
    assert!(
        fs::read_dir(destination.path())
            .expect("destination should be readable")
            .all(|entry| !entry
                .expect("entry should be readable")
                .file_name()
                .to_string_lossy()
                .starts_with(".signal-filter-"))
    );
}

#[cfg(unix)]
#[test]
fn confirmed_overwrite_rejects_a_media_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let destination = tempfile::tempdir().expect("destination should exist");
    let output = destination.path().join("existing.json");
    fs::write(&output, b"old export").expect("old export should be written");
    let outside = tempfile::tempdir().expect("outside target should exist");
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"do not alter").expect("sentinel should be written");
    symlink(outside.path(), destination.path().join("existing-media"))
        .expect("media symlink should be created");
    let request = MultiExportRequest {
        conversation_ids: vec![String::from("999")],
        start_date: None,
        end_date: None,
        include_media: true,
        format: ExportFormat::Json,
        output_file: output.clone(),
        mode: ConversationExportMode::Separate,
    };

    assert!(matches!(
        export_conversations_overwriting(&index, &request),
        Err(signal_filter::AppError::UnsafeDestination)
    ));
    assert_eq!(
        fs::read(output).expect("old output should remain"),
        b"old export"
    );
    assert_eq!(
        fs::read(sentinel).expect("symlink target should remain"),
        b"do not alter"
    );
}

#[cfg(unix)]
#[test]
fn exported_documents_and_media_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = create_fixture();
    let index = build_archive_index(&fixture.source).expect("fixture should parse");
    let destination = tempfile::tempdir().expect("destination should exist");
    let output = destination.path().join("private.json");
    export_conversations(
        &index,
        &MultiExportRequest {
            conversation_ids: vec![String::from("999")],
            start_date: None,
            end_date: None,
            include_media: true,
            format: ExportFormat::Json,
            output_file: output.clone(),
            mode: ConversationExportMode::Separate,
        },
    )
    .expect("private export should succeed");
    let media_directory = destination.path().join("private-media");
    let media_file = media_directory.join("0001-holiday (1).jpg");
    assert_eq!(
        fs::metadata(&output)
            .expect("output metadata should exist")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&media_directory)
            .expect("media metadata should exist")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(media_file)
            .expect("media file metadata should exist")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn generated_separate_names_are_globally_unique() {
    let fixture = create_fixture();
    let mut index = build_archive_index(&fixture.source).expect("fixture should parse");
    index
        .conversations
        .get_mut("999")
        .expect("chat should exist")
        .name = String::from("A");
    index
        .conversations
        .get_mut("888")
        .expect("chat should exist")
        .name = String::from("A");
    let mut third = index.conversations["888"].clone();
    third.id = String::from("777");
    third.name = String::from("A-2");
    index.conversations.insert(third.id.clone(), third);
    let destination = tempfile::tempdir().expect("destination should exist");
    let request = MultiExportRequest {
        conversation_ids: vec![
            String::from("999"),
            String::from("888"),
            String::from("777"),
        ],
        start_date: None,
        end_date: None,
        include_media: false,
        format: ExportFormat::Markdown,
        output_file: destination.path().join("chosen.md"),
        mode: ConversationExportMode::Separate,
    };

    let result = export_conversations(&index, &request).expect("unique names should export");
    let names = result
        .output_files
        .iter()
        .map(|path| path.file_name().expect("file should be named").to_owned())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 3);
    assert!(result.output_files.iter().all(|path| path.is_file()));
}
