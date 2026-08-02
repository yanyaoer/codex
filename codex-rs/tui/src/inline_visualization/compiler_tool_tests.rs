use super::*;
use pretty_assertions::assert_eq;

#[test]
fn compiler_tool_schema_exposes_only_native_formats() {
    let DynamicToolSpec::Function(tool) = inline_visualization_compiler_tool_spec() else {
        panic!("compiler tool should be a function");
    };

    assert_eq!(tool.name, INLINE_VISUALIZATION_TOOL_NAME);
    assert_eq!(
        tool.input_schema["properties"]["format"]["enum"],
        serde_json::json!(["mermaid", "latex", "dot"])
    );
    assert!(!tool.description.contains("D2"));
}

#[test]
fn validates_stable_artifact_identifier_and_supported_formats() {
    for format in ["mermaid", "latex", "dot"] {
        assert!(
            InlineVisualizationCompileArgs::parse(serde_json::json!({
                "artifact_id": "request-flow_1",
                "format": format,
                "source": "a -> b"
            }))
            .is_ok()
        );
    }
    for (artifact_id, format) in [
        ("../request-flow", "dot"),
        ("request-flow", "d2"),
        ("request-flow", "mermaid,theme=dark"),
    ] {
        assert!(
            InlineVisualizationCompileArgs::parse(serde_json::json!({
                "artifact_id": artifact_id,
                "format": format,
                "source": "a -> b"
            }))
            .is_err()
        );
    }
    assert!(
        InlineVisualizationCompileArgs::parse(serde_json::json!({
            "artifact_id": "request-flow",
            "format": "dot",
            "source": "x".repeat(INLINE_VISUALIZATION_MAX_SOURCE_CHARS + 1)
        }))
        .is_err()
    );
}

#[test]
fn keeps_compiler_diagnostics_bounded_and_actionable() {
    let message = format!(
        "inline visualization renderer failed: syntax error {}",
        "x".repeat(8_192)
    );
    let diagnostic = bounded_diagnostic(message);
    assert!(diagnostic.starts_with("syntax error"));
    assert!(diagnostic.chars().count() <= MAX_DIAGNOSTIC_CHARS + 3);
    assert!(!diagnostic.contains("inline visualization renderer"));
}

#[test]
fn retries_only_failures_that_source_correction_can_fix() {
    assert!(source_correction_can_help(
        "Mermaid parse error: unexpected token"
    ));
    assert!(source_correction_can_help(
        "unsupported DOT attribute: image=x"
    ));
    assert!(!source_correction_can_help(
        "launch /missing/renderer: No such file or directory"
    ));
    assert!(!source_correction_can_help(
        "inline visualization renderer timed out after 30 seconds"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn compiler_returns_bounded_source_diagnostic_for_retry() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    let codex_home = tempfile::tempdir().expect("temp codex home");
    let mut context =
        InlineVisualizationContext::new(codex_home.path(), codex_protocol::ThreadId::new())
            .expect("visualization context");
    let helper = codex_home.path().join("renderer");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf 'Mermaid parse error: unexpected token\\n' >&2\nexit 1\n",
    )
    .expect("write helper");
    let mut permissions = fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&helper, permissions).expect("make helper executable");
    context.renderer_program = helper;
    context.native_rendering_enabled = true;

    let response = compile_inline_visualization(
        context,
        InlineVisualizationCompileArgs {
            artifact_id: "invalid-mermaid".to_string(),
            format: "mermaid".to_string(),
            source: "flowchart LR\na -->".to_string(),
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
    assert_eq!(result["retryable"], true);
    assert_eq!(result["retries_remaining"], 2);
    assert_eq!(
        result["diagnostic"],
        "Mermaid parse error: unexpected token"
    );
}
