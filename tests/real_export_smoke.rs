use signal_filter::{
    ConversationExportMode, ExportFormat, MultiExportRequest, build_archive_index,
    export_conversations,
};

#[test]
#[ignore = "requires an explicitly supplied local Signal export"]
fn selected_real_conversation_copies_available_media() {
    let source = std::path::PathBuf::from(
        std::env::var_os("CHATEXTRACTOR_TEST_EXPORT")
            .expect("CHATEXTRACTOR_TEST_EXPORT must be set"),
    );
    let conversation_name = std::env::var("CHATEXTRACTOR_TEST_CONVERSATION")
        .expect("CHATEXTRACTOR_TEST_CONVERSATION must be set");
    let index = build_archive_index(&source).expect("local export should parse");
    let conversation_id = index
        .conversations
        .values()
        .find(|conversation| conversation.name == conversation_name)
        .map(|conversation| conversation.id.clone())
        .expect("selected conversation should exist");
    let destination = tempfile::tempdir().expect("temporary destination should be created");
    let request = MultiExportRequest {
        conversation_ids: vec![conversation_id],
        start_date: None,
        end_date: None,
        include_media: true,
        format: ExportFormat::Json,
        output_file: destination.path().join("conversation.json"),
        mode: ConversationExportMode::Separate,
    };

    let result = export_conversations(&index, &request).expect("local export should succeed");
    assert!(
        result.messages_exported > 0,
        "conversation should export messages"
    );
    assert_eq!(
        result.media_missing, 0,
        "every referenced source media file should resolve"
    );
    assert!(
        result.media_copied > 0,
        "selected conversation should copy at least one media file"
    );
}
