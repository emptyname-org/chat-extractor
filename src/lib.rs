//! Core library for the offline Chat Extractor for Signal app.

pub mod app;
pub mod export;
pub mod model;
pub mod parser;

pub use export::{
    ConversationExportMode, ExportFormat, ExportRequest, ExportResult, MultiExportRequest,
    MultiExportResult, export_conversation, export_conversations, export_conversations_overwriting,
};
pub use model::{ArchiveIndex, Conversation, Recipient};
pub use parser::{AppError, build_archive_index};
