use super::InlineVisualizationContext;
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_app_server_protocol::DynamicToolFunctionSpec;
use codex_app_server_protocol::DynamicToolSpec;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const INLINE_VISUALIZATION_TOOL_NAME: &str = "compile_inline_visualization";
pub(crate) const INLINE_VISUALIZATION_MAX_SOURCE_CHARS: usize = 8 * 1024;
pub(crate) const INLINE_VISUALIZATION_MAX_RETRIES: u8 = 2;
const MAX_DIAGNOSTIC_CHARS: usize = 768;

pub(crate) fn inline_visualization_compiler_tool_spec() -> DynamicToolSpec {
    DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: INLINE_VISUALIZATION_TOOL_NAME.to_string(),
        description: concat!(
            "Compile one D2, Mermaid, or LaTeX visualization before emitting it in the final ",
            "answer. Use a stable artifact_id for the initial call and at most two syntax-fix ",
            "retries. After a successful compile, copy the exact source unchanged into a ",
            "top-level fenced block in the final answer."
        )
        .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {
                    "type": "string",
                    "description": "Stable identifier reused for every retry of this artifact.",
                    "minLength": 1,
                    "maxLength": 64,
                    "pattern": "^[A-Za-z0-9_-]+$"
                },
                "format": {
                    "type": "string",
                    "enum": ["d2", "mermaid", "latex"]
                },
                "source": {
                    "type": "string",
                    "description": "Exact source intended for the final fenced block.",
                    "maxLength": INLINE_VISUALIZATION_MAX_SOURCE_CHARS
                }
            },
            "required": ["artifact_id", "format", "source"],
            "additionalProperties": false
        }),
        defer_loading: false,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InlineVisualizationCompileArgs {
    pub(crate) artifact_id: String,
    pub(crate) format: String,
    pub(crate) source: String,
}

impl InlineVisualizationCompileArgs {
    pub(crate) fn parse(arguments: Value) -> Result<Self, String> {
        let arguments: Self = serde_json::from_value(arguments)
            .map_err(|error| format!("invalid compiler arguments: {error}"))?;
        if arguments.artifact_id.is_empty()
            || arguments.artifact_id.len() > 64
            || !arguments
                .artifact_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(
                "artifact_id must contain 1-64 ASCII letters, digits, underscores, or hyphens"
                    .to_string(),
            );
        }
        if !matches!(arguments.format.as_str(), "d2" | "mermaid" | "latex") {
            return Err("format must be d2, mermaid, or latex".to_string());
        }
        if arguments.source.chars().count() > INLINE_VISUALIZATION_MAX_SOURCE_CHARS {
            return Err(format!(
                "source must contain at most {INLINE_VISUALIZATION_MAX_SOURCE_CHARS} characters"
            ));
        }
        Ok(arguments)
    }
}

pub(crate) async fn compile_inline_visualization(
    context: InlineVisualizationContext,
    arguments: InlineVisualizationCompileArgs,
    retries_remaining: u8,
) -> DynamicToolCallResponse {
    let result = context
        .compile_native_artifact(&arguments.format, arguments.source)
        .await;
    match result {
        Ok(file) => response(
            true,
            serde_json::json!({
                "status": "ok",
                "artifact_id": arguments.artifact_id,
                "format": arguments.format,
                "file": file,
                "retries_remaining": retries_remaining,
                "instruction": "Use the exact compiled source unchanged in the final top-level fenced block."
            }),
        ),
        Err(error) => {
            let diagnostic = bounded_diagnostic(error.to_string());
            let retryable = retries_remaining > 0
                && !diagnostic.contains("is missing; run `codex inline-viz-setup`");
            let instruction = if retryable {
                format!(
                    "Correct the source using this diagnostic, then call {INLINE_VISUALIZATION_TOOL_NAME} again with the same artifact_id."
                )
            } else {
                "Do not emit this failing fenced block; use plain text or a compact ASCII visual instead."
                    .to_string()
            };
            response(
                false,
                serde_json::json!({
                    "status": "error",
                    "artifact_id": arguments.artifact_id,
                    "format": arguments.format,
                    "diagnostic": diagnostic,
                    "retryable": retryable,
                    "retries_remaining": retries_remaining,
                    "instruction": instruction
                }),
            )
        }
    }
}

pub(crate) fn inline_visualization_error_response(
    message: impl Into<String>,
    retries_remaining: u8,
    retryable: bool,
) -> DynamicToolCallResponse {
    let retryable = retryable && retries_remaining > 0;
    let instruction = if retryable {
        format!(
            "Correct the compiler arguments or source using this diagnostic, then call {INLINE_VISUALIZATION_TOOL_NAME} again."
        )
    } else {
        "Do not emit the failing fenced block; use plain text or a compact ASCII visual instead."
            .to_string()
    };
    response(
        false,
        serde_json::json!({
            "status": "error",
            "diagnostic": bounded_diagnostic(message.into()),
            "retryable": retryable,
            "retries_remaining": retries_remaining,
            "instruction": instruction
        }),
    )
}

fn response(success: bool, value: Value) -> DynamicToolCallResponse {
    DynamicToolCallResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: value.to_string(),
        }],
        success,
    }
}

fn bounded_diagnostic(message: String) -> String {
    let message = message
        .split_once(" failed: ")
        .map_or(message.as_str(), |(_, diagnostic)| diagnostic);
    let mut characters = message.chars();
    let bounded = characters
        .by_ref()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

#[cfg(test)]
#[path = "compiler_tool_tests.rs"]
mod tests;
