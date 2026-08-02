use super::*;
use pretty_assertions::assert_eq;

#[test]
fn validates_stable_artifact_identifier_and_exact_format() {
    assert!(
        InlineVisualizationCompileArgs::parse(serde_json::json!({
            "artifact_id": "request-flow_1",
            "format": "d2",
            "source": "a -> b"
        }))
        .is_ok()
    );
    assert!(
        InlineVisualizationCompileArgs::parse(serde_json::json!({
            "artifact_id": "../request-flow",
            "format": "d2",
            "source": "a -> b"
        }))
        .is_err()
    );
    assert!(
        InlineVisualizationCompileArgs::parse(serde_json::json!({
            "artifact_id": "request-flow",
            "format": "d2,theme=dark",
            "source": "a -> b"
        }))
        .is_err()
    );
    assert!(
        InlineVisualizationCompileArgs::parse(serde_json::json!({
            "artifact_id": "request-flow",
            "format": "d2",
            "source": "x".repeat(INLINE_VISUALIZATION_MAX_SOURCE_CHARS + 1)
        }))
        .is_err()
    );
}

#[test]
fn keeps_compiler_diagnostics_bounded_and_actionable() {
    let message = format!("/tmp/d2 failed: syntax error {}", "x".repeat(8_192));
    let diagnostic = bounded_diagnostic(message);
    assert!(diagnostic.starts_with("syntax error"));
    assert!(diagnostic.chars().count() <= MAX_DIAGNOSTIC_CHARS + 3);
    assert!(!diagnostic.contains("/tmp/d2"));
}

#[tokio::test]
async fn real_compiler_returns_d2_diagnostic_when_requested() {
    if std::env::var_os("CODEX_INLINE_VIZ_REAL_SMOKE").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }

    let codex_home = tempfile::tempdir().expect("temp codex home");
    let mut context =
        InlineVisualizationContext::new(codex_home.path(), codex_protocol::ThreadId::new())
            .expect("visualization context");
    context.native_rendering_enabled = true;
    let response = compile_inline_visualization(
        context,
        InlineVisualizationCompileArgs {
            artifact_id: "invalid-d2".to_string(),
            format: "d2".to_string(),
            source: "diagram: {\n".to_string(),
        },
        /*retries_remaining*/ 2,
    )
    .await;

    assert!(!response.success);
    let [DynamicToolCallOutputContentItem::InputText { text }] = response.content_items.as_slice()
    else {
        panic!("compiler should return one text diagnostic");
    };
    let result: Value = serde_json::from_str(text).expect("compiler response JSON");
    let diagnostic = result["diagnostic"].as_str().expect("compiler diagnostic");
    assert_eq!(
        result,
        serde_json::json!({
            "status": "error",
            "artifact_id": "invalid-d2",
            "format": "d2",
            "diagnostic": diagnostic,
            "retryable": true,
            "retries_remaining": 2,
            "instruction": concat!(
                "Correct the source using this diagnostic, then call ",
                "compile_inline_visualization again with the same artifact_id."
            )
        })
    );
    assert!(!diagnostic.is_empty());
    assert_ne!(diagnostic, "no diagnostic output");
}
