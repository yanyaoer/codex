use super::*;

#[tokio::test]
async fn final_png_preview_is_cleared_by_the_next_user_message() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    let context = crate::inline_visualization::InlineVisualizationContext::from_config(
        &chat.config,
        thread_id,
    )
    .expect("visualization context");
    std::fs::create_dir_all(context.thread_dir()).expect("create visualization directory");
    image::RgbaImage::from_pixel(40, 20, image::Rgba([20, 80, 160, 255]))
        .save_with_format(
            context.thread_dir().join("chart.png"),
            image::ImageFormat::Png,
        )
        .expect("write PNG");

    chat.on_agent_message_item_completed(
        AgentMessageItem {
            id: "assistant-1".to_string(),
            content: vec![AgentMessageContent::Text {
                text: "::codex-inline-vis{file=\"chart.png\"}".to_string(),
            }],
            phase: Some(MessagePhase::FinalAnswer),
            memory_citation: None,
        },
        "turn-1",
        /*from_replay*/ true,
    );
    assert!(chat.artifact_preview.is_some());

    replay_user_message_text(
        &mut chat,
        "user-2",
        "continue",
        ReplayKind::ResumeInitialMessages,
    );
    assert!(chat.artifact_preview.is_none());
}
