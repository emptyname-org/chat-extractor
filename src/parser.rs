use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

use crate::model::{ArchiveIndex, Conversation, Recipient, timestamp_to_local};

#[derive(Debug, Error)]
pub enum AppError {
    #[error("The Signal export is missing or unreadable.")]
    SourceNotFound,
    #[error("The folder has no top-level main.jsonl.")]
    MainJsonlMissing,
    #[error("Select main.jsonl or its export folder.")]
    WrongSourceType,
    #[error("Could not open the Signal export.")]
    OpenSource(#[source] std::io::Error),
    #[error("Could not read line {line} of the Signal export.")]
    ReadSource {
        line: u64,
        #[source]
        source: std::io::Error,
    },
    #[error("main.jsonl contains invalid JSON on line {line}.")]
    InvalidJson {
        line: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("main.jsonl line {line} is not a JSON object.")]
    NonObjectLine { line: u64 },
    #[error("This is not a Signal Desktop plaintext export (header missing).")]
    MissingHeader,
    #[error("No chats were found in this Signal export.")]
    NoConversations,
    #[error("Select at least one chat.")]
    NoConversationsSelected,
    #[error("A chatItem on line {line} has no chatId.")]
    MissingChatId { line: u64 },
    #[error("A chatItem on line {line} has an invalid dateSent value.")]
    InvalidDateSent { line: u64 },
    #[error("A message contains an invalid dateSent timestamp.")]
    InvalidTimestamp,
    #[error("The selected chat no longer exists.")]
    UnknownConversation,
    #[error("The start date is after the end date.")]
    ReversedDateRange,
    #[error("The destination is missing or not a folder.")]
    InvalidDestination,
    #[error("Choose a filename in an existing folder.")]
    InvalidOutputFile,
    #[error("The filename extension does not match the format.")]
    OutputExtensionMismatch,
    #[error("The destination would overwrite the Signal export.")]
    ProtectedSourceDestination,
    #[error("The destination is a symbolic link or incompatible file.")]
    UnsafeDestination,
    #[error("The output or media folder already exists.")]
    OutputExists,
    #[error("Could not create the export folder.")]
    CreateExport(#[source] std::io::Error),
    #[error("Could not write the chat export.")]
    WriteExport(#[source] std::io::Error),
    #[error("Could not serialize the chat export.")]
    SerializeExport(#[source] serde_json::Error),
    #[error("Could not copy export media.")]
    CopyMedia(#[source] std::io::Error),
    #[error("Could not finish the export.")]
    FinishExport(#[source] std::io::Error),
}

#[derive(Debug)]
struct MessageStats {
    count: u64,
    technical_update_count: u64,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    authors: HashSet<String>,
}

pub fn locate_main_jsonl(source: &Path) -> Result<(PathBuf, PathBuf), AppError> {
    if source.is_dir() {
        let candidate = source.join("main.jsonl");
        return if candidate.is_file() {
            Ok((candidate, source.to_path_buf()))
        } else {
            Err(AppError::MainJsonlMissing)
        };
    }
    if source.is_file() {
        let is_jsonl = source
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("jsonl"));
        return if is_jsonl {
            Ok((
                source.to_path_buf(),
                source
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
            ))
        } else {
            Err(AppError::WrongSourceType)
        };
    }
    Err(AppError::SourceNotFound)
}

pub fn build_archive_index(source: &Path) -> Result<ArchiveIndex, AppError> {
    let (source_file, export_root) = locate_main_jsonl(source)?;
    let mut raw_recipients: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut raw_chats: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut stats: HashMap<String, MessageStats> = HashMap::new();
    let mut account_name = String::from("You");
    let mut total_lines = 0;
    let mut saw_header = false;

    for_each_json_object(&source_file, |line, item| {
        total_lines = line;
        if item.contains_key("version") && item.contains_key("backupTimeMs") {
            saw_header = true;
        }

        if let Some(account) = item.get("account").and_then(Value::as_object) {
            account_name = joined_name(account.get("givenName"), account.get("familyName"));
            if account_name.is_empty() {
                account_name = String::from("You");
            }
        }

        if let Some(recipient) = item.get("recipient").and_then(Value::as_object)
            && let Some(recipient_id) = recipient.get("id").and_then(normalize_id)
        {
            raw_recipients.insert(recipient_id, recipient.clone());
        }

        if let Some(chat) = item.get("chat").and_then(Value::as_object)
            && let Some(chat_id) = chat.get("id").and_then(normalize_id)
        {
            raw_chats.insert(chat_id, chat.clone());
        }

        if let Some(chat_item) = item.get("chatItem").and_then(Value::as_object) {
            let chat_id = chat_item
                .get("chatId")
                .and_then(normalize_id)
                .ok_or(AppError::MissingChatId { line })?;
            let timestamp_ms =
                chat_item_timestamp_ms(chat_item).ok_or(AppError::InvalidDateSent { line })?;
            timestamp_to_local(timestamp_ms).map_err(|_| AppError::InvalidDateSent { line })?;
            let author_id = chat_item.get("authorId").and_then(normalize_id);
            let entry = stats.entry(chat_id).or_insert_with(|| MessageStats {
                count: 0,
                technical_update_count: 0,
                first_ms: None,
                last_ms: None,
                authors: HashSet::new(),
            });
            entry.count += 1;
            if is_technical_update(chat_item) {
                entry.technical_update_count += 1;
            }
            if timestamp_ms != 0 {
                entry.first_ms = Some(
                    entry
                        .first_ms
                        .map_or(timestamp_ms, |old| old.min(timestamp_ms)),
                );
                entry.last_ms = Some(
                    entry
                        .last_ms
                        .map_or(timestamp_ms, |old| old.max(timestamp_ms)),
                );
            }
            if let Some(author_id) = author_id {
                entry.authors.insert(author_id);
            }
        }
        Ok(())
    })?;

    if !saw_header {
        return Err(AppError::MissingHeader);
    }

    let recipients: HashMap<String, Recipient> = raw_recipients
        .iter()
        .map(|(id, raw)| (id.clone(), recipient_from_raw(id, raw, &account_name)))
        .collect();
    let all_chat_ids: HashSet<String> = raw_chats.keys().chain(stats.keys()).cloned().collect();
    let conversations: HashMap<String, Conversation> = all_chat_ids
        .into_iter()
        .map(|chat_id| {
            let raw_chat = raw_chats.get(&chat_id);
            let recipient_id = raw_chat
                .and_then(|chat| chat.get("recipientId"))
                .and_then(normalize_id)
                .unwrap_or_default();
            let recipient = recipients.get(&recipient_id);
            let message_stats = stats.remove(&chat_id);
            let conversation = Conversation {
                id: chat_id.clone(),
                recipient_id,
                name: recipient
                    .map(|value| value.name.clone())
                    .unwrap_or_else(|| format!("Unknown chat {chat_id}")),
                kind: recipient
                    .map(|value| value.kind.clone())
                    .unwrap_or_else(|| String::from("unknown")),
                message_count: message_stats.as_ref().map_or(0, |value| value.count),
                is_technical_update_only: message_stats
                    .as_ref()
                    .is_some_and(|value| value.count == 1 && value.technical_update_count == 1),
                first_timestamp_ms: message_stats.as_ref().and_then(|value| value.first_ms),
                last_timestamp_ms: message_stats.as_ref().and_then(|value| value.last_ms),
                author_ids: message_stats.map_or_else(HashSet::new, |value| value.authors),
            };
            (chat_id, conversation)
        })
        .collect();

    if conversations.is_empty() {
        return Err(AppError::NoConversations);
    }

    Ok(ArchiveIndex {
        source_file,
        export_root,
        account_name,
        recipients,
        conversations,
        total_lines,
    })
}

pub(crate) fn for_each_json_object(
    source_file: &Path,
    mut callback: impl FnMut(u64, &Map<String, Value>) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let file = File::open(source_file).map_err(AppError::OpenSource)?;
    let reader = BufReader::new(file);
    for (offset, line_result) in reader.lines().enumerate() {
        let line_number = offset as u64 + 1;
        let line = line_result.map_err(|source| AppError::ReadSource {
            line: line_number,
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|source| AppError::InvalidJson {
            line: line_number,
            source,
        })?;
        let object = value
            .as_object()
            .ok_or(AppError::NonObjectLine { line: line_number })?;
        callback(line_number, object)?;
    }
    Ok(())
}

pub(crate) fn normalize_id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => (!value.is_empty()).then(|| value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_i64(),
        _ => None,
    }
}

pub(crate) fn chat_item_timestamp_ms(chat_item: &Map<String, Value>) -> Option<i64> {
    match chat_item.get("dateSent") {
        Some(value) => value_as_i64(value),
        None => Some(0),
    }
}

fn is_technical_update(chat_item: &Map<String, Value>) -> bool {
    chat_item.get("updateMessage").is_some_and(Value::is_object)
        || chat_item.get("simpleUpdate").is_some_and(Value::is_object)
}

fn recipient_from_raw(
    recipient_id: &str,
    raw: &Map<String, Value>,
    account_name: &str,
) -> Recipient {
    let details = raw
        .get("destination")
        .and_then(Value::as_object)
        .unwrap_or(raw);

    if let Some(contact) = details.get("contact").and_then(Value::as_object) {
        let name = contact_name(contact);
        return Recipient {
            id: recipient_id.to_owned(),
            name: if name.is_empty() {
                format!("Unknown contact {recipient_id}")
            } else {
                name
            },
            kind: String::from("contact"),
        };
    }

    if let Some(group) = details.get("group").and_then(Value::as_object) {
        let name = group
            .get("snapshot")
            .and_then(Value::as_object)
            .and_then(|snapshot| snapshot.get("title"))
            .and_then(Value::as_object)
            .and_then(|title| title.get("title"))
            .and_then(clean_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Unnamed group {recipient_id}"));
        return Recipient {
            id: recipient_id.to_owned(),
            name,
            kind: String::from("group"),
        };
    }

    if details.contains_key("self") {
        return Recipient {
            id: recipient_id.to_owned(),
            name: account_name.to_owned(),
            kind: String::from("self"),
        };
    }
    if details.contains_key("releaseNotes") {
        return Recipient {
            id: recipient_id.to_owned(),
            name: String::from("Signal"),
            kind: String::from("release notes"),
        };
    }
    if let Some(distribution) = details.get("distributionList").and_then(Value::as_object) {
        let name = distribution
            .get("distributionList")
            .and_then(Value::as_object)
            .and_then(|value| value.get("name"))
            .and_then(clean_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Story list {recipient_id}"));
        return Recipient {
            id: recipient_id.to_owned(),
            name,
            kind: String::from("story list"),
        };
    }

    Recipient {
        id: recipient_id.to_owned(),
        name: format!("Unknown recipient {recipient_id}"),
        kind: String::from("unknown"),
    }
}

fn contact_name(contact: &Map<String, Value>) -> String {
    if let Some(nickname) = contact.get("nickname").and_then(Value::as_object) {
        let name = joined_name(nickname.get("given"), nickname.get("family"));
        if !name.is_empty() {
            return name;
        }
    }
    for (given, family) in [
        ("systemGivenName", "systemFamilyName"),
        ("profileGivenName", "profileFamilyName"),
    ] {
        let name = joined_name(contact.get(given), contact.get(family));
        if !name.is_empty() {
            return name;
        }
    }
    for key in ["systemNickname", "username", "e164"] {
        if let Some(name) = contact.get(key).and_then(clean_text)
            && !name.is_empty()
        {
            return name;
        }
    }
    String::new()
}

fn joined_name(given: Option<&Value>, family: Option<&Value>) -> String {
    [given.and_then(clean_text), family.and_then(clean_text)]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_text(value: &Value) -> Option<String> {
    let raw = match value {
        Value::String(value) => value.as_str(),
        Value::Number(value) => return Some(value.to_string()),
        _ => return None,
    };
    Some(raw.split_whitespace().collect::<Vec<_>>().join(" "))
}
