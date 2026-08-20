use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{Local, NaiveDate, SecondsFormat};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::model::{ArchiveIndex, timestamp_to_local};
use crate::parser::{AppError, chat_item_timestamp_ms, for_each_json_object, normalize_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub conversation_id: String,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub include_media: bool,
    pub format: ExportFormat,
    pub destination: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ExportResult {
    pub output_directory: PathBuf,
    pub output_file: PathBuf,
    pub messages_exported: u64,
    pub media_copied: u64,
    pub media_missing: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationExportMode {
    Combined,
    Separate,
}

#[derive(Debug, Clone)]
pub struct MultiExportRequest {
    pub conversation_ids: Vec<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub include_media: bool,
    pub format: ExportFormat,
    pub output_file: PathBuf,
    pub mode: ConversationExportMode,
}

#[derive(Debug, Clone)]
pub struct MultiExportResult {
    pub output_files: Vec<PathBuf>,
    pub messages_exported: u64,
    pub media_copied: u64,
    pub media_missing: u64,
}

#[derive(Debug, Serialize)]
struct AuthorRecord {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct QuoteRecord {
    author: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct ReactionRecord {
    emoji: String,
    author: String,
}

#[derive(Debug, Serialize)]
struct AttachmentRecord {
    file_name: String,
    content_type: Option<String>,
    caption: Option<String>,
    width: Option<u64>,
    height: Option<u64>,
    source_available: bool,
    exported_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct MessageRecord {
    conversation_id: String,
    conversation_name: String,
    timestamp: String,
    timestamp_ms: i64,
    author: AuthorRecord,
    direction: String,
    message_type: String,
    text: Option<String>,
    quote: Option<QuoteRecord>,
    reactions: Vec<ReactionRecord>,
    attachments: Vec<AttachmentRecord>,
    signal_data: Value,
}

pub fn export_conversation(
    index: &ArchiveIndex,
    request: &ExportRequest,
) -> Result<ExportResult, AppError> {
    if let (Some(start), Some(end)) = (request.start_date, request.end_date)
        && start > end
    {
        return Err(AppError::ReversedDateRange);
    }
    if !request.destination.is_dir() {
        return Err(AppError::InvalidDestination);
    }
    let conversation = index
        .conversations
        .get(&request.conversation_id)
        .ok_or(AppError::UnknownConversation)?;

    let export_name = unique_export_name(&request.destination, &conversation.name);
    let final_directory = request.destination.join(&export_name);
    let staging_directory = create_staging_directory(&request.destination)?;
    let output_name = format!("conversation.{}", request.format.extension());

    let result = export_into_staging(index, request, &staging_directory, &output_name, "media");
    match result {
        Ok(mut export_result) => {
            if let Err(error) = fs::rename(&staging_directory, &final_directory) {
                let _ = fs::remove_dir_all(&staging_directory);
                return Err(AppError::FinishExport(error));
            }
            export_result.output_directory = final_directory.clone();
            export_result.output_file = final_directory.join(output_name);
            Ok(export_result)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_directory);
            Err(error)
        }
    }
}

fn export_into_staging(
    index: &ArchiveIndex,
    request: &ExportRequest,
    staging_directory: &Path,
    output_name: &str,
    media_directory_name: &str,
) -> Result<ExportResult, AppError> {
    let conversation = index
        .conversations
        .get(&request.conversation_id)
        .ok_or(AppError::UnknownConversation)?;
    let output_file = staging_directory.join(output_name);
    let file = create_private_file(&output_file).map_err(AppError::WriteExport)?;
    let mut writer = BufWriter::new(file);
    let mut media = MediaCopier::new_with_directory(
        index.export_root.clone(),
        staging_directory.to_path_buf(),
        request.include_media,
        media_directory_name,
    );
    let mut message_count = 0_u64;
    let exported_at = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    match request.format {
        ExportFormat::Json => {
            write_json_header(&mut writer, &exported_at, conversation, index, request)?
        }
        ExportFormat::Markdown => {
            write_markdown_header(&mut writer, &exported_at, conversation, index, request)?
        }
    }

    let mut first_json_message = true;
    for_each_json_object(&index.source_file, |_line, item| {
        let Some(chat_item) = item.get("chatItem").and_then(Value::as_object) else {
            return Ok(());
        };
        let is_selected = chat_item
            .get("chatId")
            .and_then(normalize_id)
            .is_some_and(|id| id == request.conversation_id);
        if !is_selected {
            return Ok(());
        }
        let timestamp_ms = chat_item_timestamp_ms(chat_item).ok_or(AppError::InvalidTimestamp)?;
        if !timestamp_is_in_range(timestamp_ms, request.start_date, request.end_date)? {
            return Ok(());
        }

        let record = normalize_message(chat_item, conversation, index, &mut media)?;
        match request.format {
            ExportFormat::Json => {
                if !first_json_message {
                    writer.write_all(b",\n").map_err(AppError::WriteExport)?;
                }
                writer.write_all(b"    ").map_err(AppError::WriteExport)?;
                serde_json::to_writer_pretty(&mut writer, &record)
                    .map_err(AppError::SerializeExport)?;
                first_json_message = false;
            }
            ExportFormat::Markdown => write_markdown_message(&mut writer, &record)?,
        }
        message_count += 1;
        Ok(())
    })?;

    if request.format == ExportFormat::Json {
        write_json_footer(&mut writer, message_count, &media)?;
    }
    flush_and_sync(&mut writer).map_err(AppError::WriteExport)?;

    Ok(ExportResult {
        output_directory: staging_directory.to_path_buf(),
        output_file,
        messages_exported: message_count,
        media_copied: media.copied_count,
        media_missing: media.missing_count,
    })
}

fn write_json_header(
    writer: &mut impl Write,
    exported_at: &str,
    conversation: &crate::model::Conversation,
    index: &ArchiveIndex,
    request: &ExportRequest,
) -> Result<(), AppError> {
    writer
        .write_all(b"{\n  \"schema_version\": 1,\n  \"exported_at\": ")
        .map_err(AppError::WriteExport)?;
    serde_json::to_writer(&mut *writer, exported_at).map_err(AppError::SerializeExport)?;
    writer
        .write_all(
            b",\n  \"source_format\": \"Signal Desktop plaintext export\",\n  \"conversation\": ",
        )
        .map_err(AppError::WriteExport)?;
    let conversation_value = json!({
        "id": conversation.id,
        "name": conversation.name,
        "type": conversation.kind,
    });
    serde_json::to_writer_pretty(&mut *writer, &conversation_value)
        .map_err(AppError::SerializeExport)?;
    writer
        .write_all(b",\n  \"date_range\": ")
        .map_err(AppError::WriteExport)?;
    let date_range = json!({
        "start": request.start_date.map(|date| date.to_string()),
        "end": request.end_date.map(|date| date.to_string()),
        "timezone": Local::now().offset().to_string(),
    });
    serde_json::to_writer_pretty(&mut *writer, &date_range).map_err(AppError::SerializeExport)?;
    writer
        .write_all(b",\n  \"include_media\": ")
        .map_err(AppError::WriteExport)?;
    serde_json::to_writer(&mut *writer, &request.include_media)
        .map_err(AppError::SerializeExport)?;
    writer
        .write_all(b",\n  \"participants\": ")
        .map_err(AppError::WriteExport)?;
    let participants = participants(index, conversation);
    serde_json::to_writer_pretty(&mut *writer, &participants).map_err(AppError::SerializeExport)?;
    writer
        .write_all(b",\n  \"messages\": [\n")
        .map_err(AppError::WriteExport)
}

fn write_json_footer(
    writer: &mut impl Write,
    messages: u64,
    media: &MediaCopier,
) -> Result<(), AppError> {
    writer
        .write_all(b"\n  ],\n  \"summary\": ")
        .map_err(AppError::WriteExport)?;
    let summary = json!({
        "messages_exported": messages,
        "media_files_copied": media.copied_count,
        "media_files_missing": media.missing_count,
    });
    serde_json::to_writer_pretty(&mut *writer, &summary).map_err(AppError::SerializeExport)?;
    writer.write_all(b"\n}\n").map_err(AppError::WriteExport)
}

fn write_markdown_header(
    writer: &mut impl Write,
    exported_at: &str,
    conversation: &crate::model::Conversation,
    index: &ArchiveIndex,
    request: &ExportRequest,
) -> Result<(), AppError> {
    writeln!(writer, "# {}\n", escape_markdown(&conversation.name))
        .map_err(AppError::WriteExport)?;
    writeln!(writer, "- Exported: `{}`", escape_markdown(exported_at))
        .map_err(AppError::WriteExport)?;
    writeln!(
        writer,
        "- Dates: `{}` to `{}` (local)",
        request
            .start_date
            .map_or_else(|| "earliest".to_owned(), |date| date.to_string()),
        request
            .end_date
            .map_or_else(|| "latest".to_owned(), |date| date.to_string())
    )
    .map_err(AppError::WriteExport)?;
    writeln!(
        writer,
        "- Media: {}",
        if request.include_media {
            "included"
        } else {
            "not included"
        }
    )
    .map_err(AppError::WriteExport)?;
    let participant_names = participants(index, conversation)
        .into_iter()
        .map(|participant| escape_markdown(&participant.name))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(writer, "- Participants: {participant_names}\n").map_err(AppError::WriteExport)?;
    writeln!(writer, "---\n").map_err(AppError::WriteExport)
}

fn write_markdown_message(
    writer: &mut impl Write,
    message: &MessageRecord,
) -> Result<(), AppError> {
    writeln!(
        writer,
        "### {} · {}\n",
        escape_markdown(&message.timestamp),
        escape_markdown(&message.author.name)
    )
    .map_err(AppError::WriteExport)?;
    if let Some(quote) = &message.quote {
        writeln!(
            writer,
            "> **Quoted from {}**",
            escape_markdown(&quote.author)
        )
        .map_err(AppError::WriteExport)?;
        for line in quote.text.lines() {
            writeln!(writer, "> {}", escape_markdown(line)).map_err(AppError::WriteExport)?;
        }
        writeln!(writer).map_err(AppError::WriteExport)?;
    }
    if let Some(text) = &message.text {
        for line in text.lines() {
            writeln!(writer, "{}  ", escape_markdown(line)).map_err(AppError::WriteExport)?;
        }
    } else {
        writeln!(writer, "_{}_\n", escape_markdown(&message.message_type))
            .map_err(AppError::WriteExport)?;
    }

    for attachment in &message.attachments {
        let label = attachment
            .caption
            .as_deref()
            .unwrap_or(&attachment.file_name);
        match &attachment.exported_path {
            Some(path) => writeln!(
                writer,
                "\n📎 [{}]({})",
                escape_markdown(label),
                percent_encode_path(path)
            ),
            None if attachment.source_available => writeln!(
                writer,
                "\n📎 {} _(available in source; not copied)_",
                escape_markdown(label)
            ),
            None => writeln!(
                writer,
                "\n📎 {} _(not available in source)_",
                escape_markdown(label)
            ),
        }
        .map_err(AppError::WriteExport)?;
    }
    if !message.reactions.is_empty() {
        let reactions = message
            .reactions
            .iter()
            .map(|reaction| {
                format!(
                    "{} {}",
                    escape_markdown(&reaction.emoji),
                    escape_markdown(&reaction.author)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        writeln!(writer, "\n_Reactions: {reactions}_").map_err(AppError::WriteExport)?;
    }
    writeln!(writer, "\n---\n").map_err(AppError::WriteExport)
}

fn normalize_message(
    chat_item: &Map<String, Value>,
    conversation: &crate::model::Conversation,
    index: &ArchiveIndex,
    media: &mut MediaCopier,
) -> Result<MessageRecord, AppError> {
    let timestamp_ms = chat_item_timestamp_ms(chat_item).ok_or(AppError::InvalidTimestamp)?;
    let timestamp = timestamp_to_local(timestamp_ms)?.to_rfc3339_opts(SecondsFormat::Secs, true);
    let author_id = chat_item
        .get("authorId")
        .and_then(normalize_id)
        .unwrap_or_default();
    let author = AuthorRecord {
        name: index.author_name(&author_id),
        id: author_id,
    };
    let direction = if chat_item.contains_key("incoming") {
        "incoming"
    } else if chat_item.contains_key("outgoing") {
        "outgoing"
    } else {
        "directionless"
    }
    .to_owned();
    let (message_type, payload) = message_payload(chat_item);
    let mut text = message_text(&message_type, payload);
    if let Some(pointer) = payload
        .and_then(|value| value.get("longText"))
        .and_then(Value::as_object)
        && let Some(long_text) = media.read_long_text(pointer)?
        && !long_text.is_empty()
    {
        text = Some(long_text);
    }
    let quote = payload
        .and_then(|value| value.get("quote"))
        .and_then(Value::as_object)
        .and_then(|quote| {
            let quoted_text = quote
                .get("text")
                .and_then(Value::as_object)
                .and_then(|text| text.get("body"))
                .and_then(Value::as_str)?;
            let author_id = quote
                .get("authorId")
                .and_then(normalize_id)
                .unwrap_or_default();
            Some(QuoteRecord {
                author: index.author_name(&author_id),
                text: quoted_text.to_owned(),
            })
        });
    let reactions = payload
        .and_then(|value| value.get("reactions"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|reaction| {
                    let reaction = reaction.as_object()?;
                    let emoji = reaction.get("emoji")?.as_str()?.to_owned();
                    let author_id = reaction
                        .get("authorId")
                        .and_then(normalize_id)
                        .unwrap_or_default();
                    Some(ReactionRecord {
                        emoji,
                        author: index.author_name(&author_id),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut pointers = Vec::new();
    if let Some(payload) = payload {
        collect_file_pointers(payload, &mut pointers);
    }
    let attachments = pointers
        .into_iter()
        .map(|pointer| media.attachment_record(pointer))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(MessageRecord {
        conversation_id: conversation.id.clone(),
        conversation_name: conversation.name.clone(),
        timestamp,
        timestamp_ms,
        author,
        direction,
        message_type,
        text,
        quote,
        reactions,
        attachments,
        signal_data: sanitized_signal_data(chat_item),
    })
}

fn message_payload(chat_item: &Map<String, Value>) -> (String, Option<&Map<String, Value>>) {
    const ITEM_KEYS: &[&str] = &[
        "standardMessage",
        "contactMessage",
        "stickerMessage",
        "remoteDeletedMessage",
        "updateMessage",
        "paymentNotification",
        "giftBadge",
        "viewOnceMessage",
        "directStoryReplyMessage",
        "poll",
    ];
    for key in ITEM_KEYS {
        if let Some(payload) = chat_item.get(*key).and_then(Value::as_object) {
            return (humanize_key(key), Some(payload));
        }
    }
    (String::from("Unknown message"), None)
}

fn message_text(message_type: &str, payload: Option<&Map<String, Value>>) -> Option<String> {
    let payload = payload?;
    if message_type == "Standard message" {
        return payload
            .get("text")
            .and_then(Value::as_object)
            .and_then(|text| text.get("body"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if message_type == "Direct story reply message" {
        if let Some(emoji) = payload.get("emoji").and_then(Value::as_str) {
            return Some(format!("Story reply: {emoji}"));
        }
        return payload
            .get("textReply")
            .and_then(Value::as_object)
            .and_then(|reply| reply.get("text"))
            .and_then(Value::as_object)
            .and_then(|text| text.get("body"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if message_type == "Sticker message" {
        let emoji = payload
            .get("sticker")
            .and_then(Value::as_object)
            .and_then(|sticker| sticker.get("emoji"))
            .and_then(Value::as_str)
            .unwrap_or("sticker");
        return Some(format!("[Sticker {emoji}]"));
    }
    if message_type == "Remote deleted message" {
        return Some(String::from("[Message deleted]"));
    }
    if message_type == "View once message" {
        return Some(String::from("[View-once message]"));
    }
    if message_type == "Update message" {
        let update = payload
            .get("simpleUpdate")
            .and_then(Value::as_object)
            .and_then(|update| update.get("type"))
            .and_then(Value::as_str)
            .map(humanize_key)
            .unwrap_or_else(|| String::from("Chat update"));
        return Some(format!("[{update}]"));
    }
    if message_type == "Contact message" {
        return Some(String::from("[Shared contact]"));
    }
    Some(format!("[{message_type}]"))
}

fn collect_file_pointers<'a>(
    value: &'a Map<String, Value>,
    output: &mut Vec<&'a Map<String, Value>>,
) {
    let looks_like_pointer = value.contains_key("locatorInfo")
        && ["contentType", "fileName", "caption", "width", "height"]
            .iter()
            .any(|key| value.contains_key(*key));
    if looks_like_pointer {
        output.push(value);
        return;
    }
    for child in value.values() {
        match child {
            Value::Object(object) => collect_file_pointers(object, output),
            Value::Array(array) => {
                for item in array {
                    if let Some(object) = item.as_object() {
                        collect_file_pointers(object, output);
                    }
                }
            }
            _ => {}
        }
    }
}

fn sanitized_signal_data(chat_item: &Map<String, Value>) -> Value {
    fn sanitize(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| {
                        !matches!(
                            key.as_str(),
                            "locatorInfo" | "incrementalMac" | "incrementalMacChunkSize"
                        )
                    })
                    .map(|(key, value)| (key.clone(), sanitize(value)))
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(sanitize).collect()),
            _ => value.clone(),
        }
    }
    let filtered = chat_item
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "chatId" | "authorId" | "dateSent"))
        .map(|(key, value)| (key.clone(), sanitize(value)))
        .collect();
    Value::Object(filtered)
}

fn participants(
    index: &ArchiveIndex,
    conversation: &crate::model::Conversation,
) -> Vec<AuthorRecord> {
    let mut values = conversation
        .author_ids
        .iter()
        .map(|id| AuthorRecord {
            id: id.clone(),
            name: index.author_name(id),
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    values
}

struct MediaCopier {
    source_root: PathBuf,
    staging_root: PathBuf,
    media_directory_name: String,
    include_media: bool,
    copied: HashMap<PathBuf, String>,
    copied_count: u64,
    missing_count: u64,
    attachment_count: u64,
}

impl MediaCopier {
    fn new_with_directory(
        source_root: PathBuf,
        staging_root: PathBuf,
        include_media: bool,
        media_directory_name: &str,
    ) -> Self {
        Self {
            source_root,
            staging_root,
            media_directory_name: media_directory_name.to_owned(),
            include_media,
            copied: HashMap::new(),
            copied_count: 0,
            missing_count: 0,
            attachment_count: 0,
        }
    }

    fn attachment_record(
        &mut self,
        pointer: &Map<String, Value>,
    ) -> Result<AttachmentRecord, AppError> {
        self.attachment_count += 1;
        let content_type = pointer
            .get("contentType")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let source = self.resolve(pointer);
        if source.is_none() {
            self.missing_count += 1;
        }
        let fallback = format!("attachment-{:04}", self.attachment_count);
        let file_name = pointer
            .get("fileName")
            .and_then(Value::as_str)
            .map(sanitize_file_name)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                source
                    .as_deref()
                    .and_then(Path::extension)
                    .and_then(|value| value.to_str())
                    .map_or_else(
                        || fallback.clone(),
                        |extension| format!("{fallback}.{extension}"),
                    )
            });
        let exported_path = if self.include_media {
            source
                .as_deref()
                .map(|path| self.copy_once(path, &file_name))
                .transpose()?
        } else {
            None
        };
        Ok(AttachmentRecord {
            file_name,
            content_type,
            caption: pointer
                .get("caption")
                .and_then(Value::as_str)
                .map(str::to_owned),
            width: pointer.get("width").and_then(value_as_u64),
            height: pointer.get("height").and_then(value_as_u64),
            source_available: source.is_some(),
            exported_path,
        })
    }

    fn read_long_text(&self, pointer: &Map<String, Value>) -> Result<Option<String>, AppError> {
        let Some(path) = self.resolve(pointer) else {
            return Ok(None);
        };
        let file = File::open(path).map_err(AppError::OpenSource)?;
        let mut bytes = Vec::new();
        BufReader::new(file)
            .take(16 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(AppError::OpenSource)?;
        Ok(String::from_utf8(bytes).ok())
    }

    fn resolve(&self, pointer: &Map<String, Value>) -> Option<PathBuf> {
        let locator = pointer.get("locatorInfo")?.as_object()?;
        let local_key = locator.get("localKey").and_then(decode_signal_bytes)?;
        let plaintext_hash = locator
            .get("plaintextHash")
            .and_then(decode_signal_bytes)
            .or_else(|| {
                locator
                    .get("integrityCheck")?
                    .as_object()?
                    .get("plaintextHash")
                    .and_then(decode_signal_bytes)
            })?;
        let media_name = format!("{:x}", Sha256::digest([plaintext_hash, local_key].concat()));
        let files_root = self.source_root.join("files");
        let directory = files_root.join(&media_name[..2]);
        if fs::symlink_metadata(&files_root)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
            || fs::symlink_metadata(&directory)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
        {
            return None;
        }
        fs::read_dir(directory)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name == media_name || name.starts_with(&format!("{media_name}."))
            })
            .find_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_file() && !kind.is_symlink())
                    .map(|_| entry.path())
            })
    }

    fn copy_once(&mut self, source: &Path, requested_name: &str) -> Result<String, AppError> {
        if let Some(existing) = self.copied.get(source) {
            return Ok(existing.clone());
        }
        let media_directory = self.staging_root.join(&self.media_directory_name);
        create_private_subdirectories(&self.staging_root, Path::new(&self.media_directory_name))?;
        let safe_name = sanitize_file_name(requested_name);
        let output_name = format!("{:04}-{}", self.copied_count + 1, safe_name);
        let destination = media_directory.join(&output_name);
        fs::copy(source, &destination).map_err(AppError::CopyMedia)?;
        set_private_file_permissions(&destination).map_err(AppError::CopyMedia)?;
        let relative_path = format!("{}/{output_name}", self.media_directory_name);
        self.copied
            .insert(source.to_path_buf(), relative_path.clone());
        self.copied_count += 1;
        Ok(relative_path)
    }
}

fn decode_signal_bytes(value: &Value) -> Option<Vec<u8>> {
    let value = value.as_str()?;
    BASE64_STANDARD
        .decode(value)
        .ok()
        .or_else(|| decode_hex(value))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_u64(),
        _ => None,
    }
}

fn timestamp_is_in_range(
    timestamp_ms: i64,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
) -> Result<bool, AppError> {
    // Signal uses zero when dateSent is absent, so date filters retain the record.
    if timestamp_ms == 0 {
        return Ok(true);
    }
    let local_date = timestamp_to_local(timestamp_ms)?.date_naive();
    Ok(start_date.is_none_or(|start| local_date >= start)
        && end_date.is_none_or(|end| local_date <= end))
}

fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    set_private_directory_permissions(path)
}

fn create_private_subdirectories(root: &Path, relative: &Path) -> Result<(), AppError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(AppError::InvalidOutputFile);
        };
        current.push(component);
        match create_private_directory(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&current).map_err(AppError::CreateExport)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(AppError::UnsafeDestination);
                }
                set_private_directory_permissions(&current).map_err(AppError::CreateExport)?;
            }
            Err(error) => return Err(AppError::CreateExport(error)),
        }
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn flush_and_sync(writer: &mut BufWriter<File>) -> std::io::Result<()> {
    writer.flush()?;
    writer.get_ref().sync_all()
}

fn create_staging_directory(destination: &Path) -> Result<PathBuf, AppError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..100_u32 {
        let candidate = destination.join(format!(
            ".signal-filter-{}-{nonce}-{attempt}",
            process::id()
        ));
        match create_private_directory(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(AppError::CreateExport(error)),
        }
    }
    Err(AppError::CreateExport(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve a unique staging folder",
    )))
}

fn unique_export_name(destination: &Path, conversation_name: &str) -> String {
    let slug = slugify(conversation_name);
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    let base = format!("signal-chat-{slug}-{timestamp}");
    if !destination.join(&base).exists() {
        return base;
    }
    (2..10_000)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !destination.join(candidate).exists())
        .unwrap_or_else(|| format!("{base}-{}", process::id()))
}

fn validate_output_extension(path: &Path, format: ExportFormat) -> Result<(), AppError> {
    let matches = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(format.extension()));
    if matches {
        Ok(())
    } else {
        Err(AppError::OutputExtensionMismatch)
    }
}

fn ensure_output_does_not_touch_source(
    index: &ArchiveIndex,
    output: &Path,
) -> Result<(), AppError> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| AppError::InvalidDestination)?;
    let file_name = output.file_name().ok_or(AppError::InvalidOutputFile)?;
    let candidate = canonical_parent.join(file_name);
    let source_file = index
        .source_file
        .canonicalize()
        .map_err(AppError::OpenSource)?;
    let export_root = index
        .export_root
        .canonicalize()
        .map_err(AppError::OpenSource)?;
    let files_root = export_root.join("files");
    if candidate == source_file
        || candidate == export_root.join("main.jsonl")
        || candidate == export_root.join("metadata.json")
        || candidate == files_root
        || candidate.starts_with(&files_root)
    {
        Err(AppError::ProtectedSourceDestination)
    } else {
        Ok(())
    }
}

fn media_directory_name(output: &Path) -> Result<String, AppError> {
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or(AppError::InvalidOutputFile)?;
    Ok(format!("{stem}-media"))
}

fn media_directory_for_output(output: &Path) -> Result<PathBuf, AppError> {
    Ok(output.with_file_name(media_directory_name(output)?))
}

fn staged_path_for(staging_directory: &Path, final_path: &Path) -> Result<PathBuf, AppError> {
    Ok(staging_directory.join(final_path.file_name().ok_or(AppError::InvalidOutputFile)?))
}

pub fn export_conversations(
    index: &ArchiveIndex,
    request: &MultiExportRequest,
) -> Result<MultiExportResult, AppError> {
    export_conversations_inner(index, request, false)
}

pub fn export_conversations_overwriting(
    index: &ArchiveIndex,
    request: &MultiExportRequest,
) -> Result<MultiExportResult, AppError> {
    export_conversations_inner(index, request, true)
}

fn export_conversations_inner(
    index: &ArchiveIndex,
    request: &MultiExportRequest,
    overwrite_existing: bool,
) -> Result<MultiExportResult, AppError> {
    let mut seen = HashSet::new();
    let conversation_ids = request
        .conversation_ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect::<Vec<_>>();

    if conversation_ids.is_empty() {
        return Err(AppError::NoConversationsSelected);
    }

    if matches!(
        (request.start_date, request.end_date),
        (Some(start), Some(end)) if start > end
    ) {
        return Err(AppError::ReversedDateRange);
    }

    let output_parent = request
        .output_file
        .parent()
        .unwrap_or_else(|| Path::new("."));
    if !output_parent.is_dir() {
        return Err(AppError::InvalidDestination);
    }
    let Some(output_name) = request.output_file.file_name() else {
        return Err(AppError::InvalidOutputFile);
    };
    if output_name.to_string_lossy().is_empty() {
        return Err(AppError::InvalidOutputFile);
    }

    ensure_output_does_not_touch_source(index, &request.output_file)?;
    validate_output_extension(&request.output_file, request.format)?;
    let output_files = explicit_output_files(index, request, &conversation_ids)?;
    for output_file in &output_files {
        ensure_output_does_not_touch_source(index, output_file)?;
    }
    let output_media_pairs = output_files
        .iter()
        .map(|output| Ok((output.clone(), media_directory_for_output(output)?)))
        .collect::<Result<Vec<_>, AppError>>()?;
    let existing_outputs = preflight_destinations(
        &output_media_pairs,
        request.include_media,
        overwrite_existing,
    )?;

    let staging_directory = create_staging_directory(output_parent)?;
    let operation = (|| {
        let staged = stage_conversations(
            index,
            request,
            &conversation_ids,
            &output_files,
            &staging_directory,
        )?;
        let artifacts = install_artifacts_for(
            &staging_directory,
            &output_media_pairs,
            &existing_outputs,
            request.include_media,
            overwrite_existing,
        )?;
        install_artifacts(&staging_directory, &artifacts)?;
        Ok(MultiExportResult {
            output_files: output_files.clone(),
            messages_exported: staged.messages_exported,
            media_copied: staged.media_copied,
            media_missing: staged.media_missing,
        })
    })();
    let _ = fs::remove_dir_all(&staging_directory);
    operation
}

struct ConversationStage {
    document_path: PathBuf,
    writer: BufWriter<File>,
    media: MediaCopier,
    message_count: u64,
    first_json_message: bool,
}

struct StagedExport {
    messages_exported: u64,
    media_copied: u64,
    media_missing: u64,
}

fn stage_conversations(
    index: &ArchiveIndex,
    request: &MultiExportRequest,
    conversation_ids: &[String],
    output_files: &[PathBuf],
    staging_directory: &Path,
) -> Result<StagedExport, AppError> {
    let document_directory = staging_directory.join("documents");
    create_private_directory(&document_directory).map_err(AppError::CreateExport)?;
    let exported_at = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut positions = HashMap::with_capacity(conversation_ids.len());
    let mut stages = Vec::with_capacity(conversation_ids.len());

    for (position, conversation_id) in conversation_ids.iter().enumerate() {
        positions.insert(conversation_id.clone(), position);
        let conversation = index
            .conversations
            .get(conversation_id)
            .ok_or(AppError::UnknownConversation)?;
        let document_path = document_directory.join(format!(
            "conversation-{position:04}.{}",
            request.format.extension()
        ));
        let mut writer =
            BufWriter::new(create_private_file(&document_path).map_err(AppError::CreateExport)?);
        let output_index = if request.mode == ConversationExportMode::Combined {
            0
        } else {
            position
        };
        let media_root = media_directory_name(&output_files[output_index])?;
        let media_directory_name =
            if request.mode == ConversationExportMode::Combined && conversation_ids.len() > 1 {
                format!("{media_root}/conversation-{:04}", position + 1)
            } else {
                media_root
            };
        let single_request = ExportRequest {
            conversation_id: conversation_id.clone(),
            start_date: request.start_date,
            end_date: request.end_date,
            include_media: request.include_media,
            format: request.format,
            destination: staging_directory.to_path_buf(),
        };
        match request.format {
            ExportFormat::Json => write_json_header(
                &mut writer,
                &exported_at,
                conversation,
                index,
                &single_request,
            )?,
            ExportFormat::Markdown => write_markdown_header(
                &mut writer,
                &exported_at,
                conversation,
                index,
                &single_request,
            )?,
        }
        stages.push(ConversationStage {
            document_path,
            writer,
            media: MediaCopier::new_with_directory(
                index.export_root.clone(),
                staging_directory.to_path_buf(),
                request.include_media,
                &media_directory_name,
            ),
            message_count: 0,
            first_json_message: true,
        });
    }

    for_each_json_object(&index.source_file, |_line, item| {
        let Some(chat_item) = item.get("chatItem").and_then(Value::as_object) else {
            return Ok(());
        };
        let Some(chat_id) = chat_item.get("chatId").and_then(normalize_id) else {
            return Ok(());
        };
        let Some(&position) = positions.get(&chat_id) else {
            return Ok(());
        };
        let timestamp_ms = chat_item_timestamp_ms(chat_item).ok_or(AppError::InvalidTimestamp)?;
        if !timestamp_is_in_range(timestamp_ms, request.start_date, request.end_date)? {
            return Ok(());
        }
        let conversation = index
            .conversations
            .get(&chat_id)
            .ok_or(AppError::UnknownConversation)?;
        let stage = &mut stages[position];
        let record = normalize_message(chat_item, conversation, index, &mut stage.media)?;
        match request.format {
            ExportFormat::Json => {
                if !stage.first_json_message {
                    stage
                        .writer
                        .write_all(b",\n")
                        .map_err(AppError::WriteExport)?;
                }
                stage
                    .writer
                    .write_all(b"    ")
                    .map_err(AppError::WriteExport)?;
                serde_json::to_writer_pretty(&mut stage.writer, &record)
                    .map_err(AppError::SerializeExport)?;
                stage.first_json_message = false;
            }
            ExportFormat::Markdown => write_markdown_message(&mut stage.writer, &record)?,
        }
        stage.message_count += 1;
        Ok(())
    })?;

    let mut messages_exported = 0;
    let mut media_copied = 0;
    let mut media_missing = 0;
    let mut documents = Vec::with_capacity(stages.len());
    for mut stage in stages {
        if request.format == ExportFormat::Json {
            write_json_footer(&mut stage.writer, stage.message_count, &stage.media)?;
        }
        flush_and_sync(&mut stage.writer).map_err(AppError::WriteExport)?;
        messages_exported += stage.message_count;
        media_copied += stage.media.copied_count;
        media_missing += stage.media.missing_count;
        documents.push(stage.document_path);
    }

    match request.mode {
        ConversationExportMode::Combined => {
            let staged_output = staged_path_for(staging_directory, &output_files[0])?;
            write_combined_documents(&staged_output, request.format, &documents)?;
        }
        ConversationExportMode::Separate => {
            for (document, output_file) in documents.iter().zip(output_files) {
                let staged_output = staged_path_for(staging_directory, output_file)?;
                fs::rename(document, staged_output).map_err(AppError::FinishExport)?;
            }
        }
    }

    Ok(StagedExport {
        messages_exported,
        media_copied,
        media_missing,
    })
}

#[derive(Clone, Copy)]
enum ArtifactKind {
    File,
    Directory,
}

struct InstallArtifact {
    staged: Option<PathBuf>,
    destination: PathBuf,
    kind: ArtifactKind,
}

fn preflight_destinations(
    pairs: &[(PathBuf, PathBuf)],
    include_media: bool,
    overwrite_existing: bool,
) -> Result<Vec<bool>, AppError> {
    let mut existing_outputs = Vec::with_capacity(pairs.len());
    for (output, media) in pairs {
        let output_exists = validate_existing_artifact(output, ArtifactKind::File)?;
        let media_exists = if include_media || (overwrite_existing && output_exists) {
            validate_existing_artifact(media, ArtifactKind::Directory)?
        } else {
            false
        };
        if !overwrite_existing && (output_exists || (include_media && media_exists)) {
            return Err(AppError::OutputExists);
        }
        existing_outputs.push(output_exists);
    }
    Ok(existing_outputs)
}

fn validate_existing_artifact(path: &Path, kind: ArtifactKind) -> Result<bool, AppError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AppError::FinishExport(error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(AppError::UnsafeDestination);
    }
    let matches = match kind {
        ArtifactKind::File => metadata.is_file(),
        ArtifactKind::Directory => metadata.is_dir(),
    };
    if !matches {
        return Err(AppError::UnsafeDestination);
    }
    Ok(true)
}

fn install_artifacts_for(
    staging_directory: &Path,
    pairs: &[(PathBuf, PathBuf)],
    existing_outputs: &[bool],
    include_media: bool,
    overwrite_existing: bool,
) -> Result<Vec<InstallArtifact>, AppError> {
    let mut artifacts = Vec::with_capacity(pairs.len() * 2);
    for ((output, media), output_existed) in pairs.iter().zip(existing_outputs) {
        artifacts.push(InstallArtifact {
            staged: Some(staged_path_for(staging_directory, output)?),
            destination: output.clone(),
            kind: ArtifactKind::File,
        });
        if include_media || (overwrite_existing && *output_existed) {
            let staged_media = staged_path_for(staging_directory, media)?;
            artifacts.push(InstallArtifact {
                staged: staged_media.is_dir().then_some(staged_media),
                destination: media.clone(),
                kind: ArtifactKind::Directory,
            });
        }
    }
    Ok(artifacts)
}

fn install_artifacts(
    staging_directory: &Path,
    artifacts: &[InstallArtifact],
) -> Result<(), AppError> {
    for artifact in artifacts {
        validate_existing_artifact(&artifact.destination, artifact.kind)?;
    }
    let backup_directory = staging_directory.join("backups");
    create_private_directory(&backup_directory).map_err(AppError::FinishExport)?;
    let mut backups = Vec::<(PathBuf, PathBuf)>::new();
    for (position, artifact) in artifacts.iter().enumerate() {
        if fs::symlink_metadata(&artifact.destination).is_ok() {
            let backup = backup_directory.join(format!("artifact-{position:04}"));
            if let Err(error) = fs::rename(&artifact.destination, &backup) {
                return Err(finish_after_rollback(error, &[], &backups));
            }
            backups.push((backup, artifact.destination.clone()));
        }
    }

    let mut installed = Vec::new();
    for artifact in artifacts {
        let Some(staged) = &artifact.staged else {
            continue;
        };
        if let Err(error) = fs::rename(staged, &artifact.destination) {
            return Err(finish_after_rollback(error, &installed, &backups));
        }
        installed.push(artifact.destination.clone());
    }
    Ok(())
}

fn finish_after_rollback(
    original: std::io::Error,
    installed: &[PathBuf],
    backups: &[(PathBuf, PathBuf)],
) -> AppError {
    let mut rollback_error = None;
    for path in installed.iter().rev() {
        if let Err(error) = remove_path_without_following(path) {
            rollback_error.get_or_insert(error);
        }
    }
    for (backup, destination) in backups.iter().rev() {
        if let Err(error) = fs::rename(backup, destination) {
            rollback_error.get_or_insert(error);
        }
    }
    if let Some(rollback_error) = rollback_error {
        AppError::FinishExport(std::io::Error::other(format!(
            "export failed ({original}); restoring the previous export also failed ({rollback_error})"
        )))
    } else {
        AppError::FinishExport(original)
    }
}

fn remove_path_without_following(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn explicit_output_files(
    index: &ArchiveIndex,
    request: &MultiExportRequest,
    conversation_ids: &[String],
) -> Result<Vec<PathBuf>, AppError> {
    if request.mode == ConversationExportMode::Combined || conversation_ids.len() == 1 {
        return Ok(vec![request.output_file.clone()]);
    }

    let parent = request
        .output_file
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let stem = request
        .output_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or(AppError::InvalidOutputFile)?;
    let extension = match request.format {
        ExportFormat::Markdown => "md",
        ExportFormat::Json => "json",
    };
    let use_conversation_date_names = NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_ok();
    let mut used_names = HashSet::<String>::new();
    let mut files = Vec::with_capacity(conversation_ids.len());

    for conversation_id in conversation_ids {
        let conversation = index
            .conversations
            .get(conversation_id)
            .ok_or(AppError::UnknownConversation)?;
        let slug = slugify(&conversation.name);
        let base_stem = if use_conversation_date_names {
            let date = conversation
                .last_local_datetime()?
                .map(|date_time| date_time.date_naive().format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| stem.to_owned());
            format!("{slug}-{date}")
        } else {
            format!("{stem}-{slug}")
        };
        let mut file_stem = base_stem.clone();
        let mut suffix = 2_u64;
        while !used_names.insert(file_stem.clone()) {
            file_stem = format!("{base_stem}-{suffix}");
            suffix += 1;
        }
        files.push(parent.join(format!("{file_stem}.{extension}")));
    }
    Ok(files)
}

fn write_combined_documents(
    output_file: &Path,
    format: ExportFormat,
    documents: &[PathBuf],
) -> Result<(), AppError> {
    let file = create_private_file(output_file).map_err(AppError::CreateExport)?;
    let mut writer = BufWriter::new(file);
    match format {
        ExportFormat::Markdown => {
            writeln!(writer, "# Signal chat export").map_err(AppError::WriteExport)?;
            writeln!(writer).map_err(AppError::WriteExport)?;
            for (position, document) in documents.iter().enumerate() {
                if position > 0 {
                    writeln!(writer).map_err(AppError::WriteExport)?;
                    writeln!(writer, "---").map_err(AppError::WriteExport)?;
                    writeln!(writer).map_err(AppError::WriteExport)?;
                }
                let mut reader =
                    BufReader::new(File::open(document).map_err(AppError::CreateExport)?);
                std::io::copy(&mut reader, &mut writer).map_err(AppError::WriteExport)?;
            }
        }
        ExportFormat::Json => {
            writeln!(writer, "{{").map_err(AppError::WriteExport)?;
            let exported_at = serde_json::to_string(&chrono::Utc::now().to_rfc3339())
                .map_err(AppError::SerializeExport)?;
            writeln!(writer, "  \"exported_at\": {exported_at},").map_err(AppError::WriteExport)?;
            writeln!(writer, "  \"conversation_exports\": [").map_err(AppError::WriteExport)?;
            for (position, document) in documents.iter().enumerate() {
                if position > 0 {
                    writeln!(writer, ",").map_err(AppError::WriteExport)?;
                }
                let mut reader =
                    BufReader::new(File::open(document).map_err(AppError::CreateExport)?);
                std::io::copy(&mut reader, &mut writer).map_err(AppError::WriteExport)?;
            }
            writeln!(writer).map_err(AppError::WriteExport)?;
            writeln!(writer, "  ]").map_err(AppError::WriteExport)?;
            writeln!(writer, "}}").map_err(AppError::WriteExport)?;
        }
    }
    flush_and_sync(&mut writer).map_err(AppError::FinishExport)?;
    Ok(())
}

fn slugify(value: &str) -> String {
    let mut output = String::new();
    let mut needs_separator = false;
    for character in value.chars() {
        if character.is_alphanumeric() {
            if needs_separator && !output.is_empty() {
                output.push('-');
            }
            for lower in character.to_lowercase() {
                output.push(lower);
            }
            needs_separator = false;
        } else {
            needs_separator = true;
        }
        if output.chars().count() >= 40 {
            break;
        }
    }
    if output.is_empty() {
        String::from("conversation")
    } else {
        output.trim_end_matches('-').to_owned()
    }
}

fn sanitize_file_name(value: &str) -> String {
    let file_name = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment");
    let filtered = file_name
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
        .take(120)
        .collect::<String>();
    let trimmed = filtered.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        String::from("attachment")
    } else {
        trimmed.to_owned()
    }
}

fn humanize_key(value: &str) -> String {
    let mut output = String::new();
    for (index, character) in value.chars().enumerate() {
        if character == '_' || character == '-' {
            if !output.ends_with(' ') {
                output.push(' ');
            }
        } else if character.is_uppercase() && index > 0 {
            output.push(' ');
            output.push(character.to_ascii_lowercase());
        } else if index == 0 {
            output.extend(character.to_uppercase());
        } else {
            output.push(character.to_ascii_lowercase());
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_markdown(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\\' | '*' | '_' | '[' | ']' | '\x60' | '#' | '+' | '-' | '!' | '|' | '{' | '}' => {
                output.push('\\');
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    output
}

fn percent_encode_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'~') {
            output.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}
