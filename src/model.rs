use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Local, TimeZone};

use crate::parser::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub recipient_id: String,
    pub name: String,
    pub kind: String,
    pub message_count: u64,
    pub is_technical_update_only: bool,
    pub first_timestamp_ms: Option<i64>,
    pub last_timestamp_ms: Option<i64>,
    pub author_ids: HashSet<String>,
}

impl Conversation {
    pub fn first_local_datetime(&self) -> Result<Option<DateTime<Local>>, AppError> {
        self.first_timestamp_ms.map(timestamp_to_local).transpose()
    }

    pub fn last_local_datetime(&self) -> Result<Option<DateTime<Local>>, AppError> {
        self.last_timestamp_ms.map(timestamp_to_local).transpose()
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveIndex {
    pub source_file: PathBuf,
    pub export_root: PathBuf,
    pub account_name: String,
    pub recipients: HashMap<String, Recipient>,
    pub conversations: HashMap<String, Conversation>,
    pub total_lines: u64,
}

impl ArchiveIndex {
    pub fn author_name(&self, recipient_id: &str) -> String {
        self.recipients
            .get(recipient_id)
            .map(|recipient| recipient.name.clone())
            .unwrap_or_else(|| format!("Unknown recipient {recipient_id}"))
    }
}

pub fn timestamp_to_local(timestamp_ms: i64) -> Result<DateTime<Local>, AppError> {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .ok_or(AppError::InvalidTimestamp)
}
