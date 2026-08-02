use crate::legacy_core::config::Config;
use codex_features::Feature;

pub(crate) const TERMINAL_VISUALIZATION_INSTRUCTIONS: &str = "\
- This surface is a terminal. When the formatting rules require a visual, include one in the final answer using compact ASCII diagrams, trees, timelines, or tables.
- Use tables for exact mappings or comparisons rather than collapsing known mappings into prose.
- Use trees for hierarchy or one-to-many relationships, and diagrams or timelines for sequence, change, or state transferred between records across event order.
- Use only ASCII characters in visuals.";

// Preflight keeps syntax errors inside the active turn so diagnostics can drive correction. The
// post-turn renderer remains a fallback because the transcript always keeps the source Markdown.
pub(crate) const INLINE_VISUALIZATION_INSTRUCTIONS: &str = "\
- This terminal automatically renders top-level fenced `mermaid`, `latex`, and bounded `dot` blocks in final answers. D2 is unsupported and remains visible as source.
- When `compile_inline_visualization` is available, call it before emitting each supported block with a stable `artifact_id`, its format, and the exact source intended for the final answer. If it reports a source error, correct the source and retry with the same `artifact_id` at most twice. After success, emit the exact compiled source unchanged.
- When the compiler tool is unavailable, emit the supported top-level fenced block directly so the post-turn renderer can attempt it. If all compiler retries fail, omit the failing fence and use plain text or compact ASCII instead. Do not invoke a skill, create a file, or add a rendering directive.
- Keep explanations as normal Markdown around the block. Use compact ASCII visuals for unsupported formats.";

pub(crate) fn with_terminal_visualization_instructions(
    config: &Config,
    control_instructions: Option<String>,
) -> Option<String> {
    let instructions = if config.features.enabled(Feature::Artifact) {
        INLINE_VISUALIZATION_INSTRUCTIONS
    } else if config
        .features
        .enabled(Feature::TerminalVisualizationInstructions)
    {
        TERMINAL_VISUALIZATION_INSTRUCTIONS
    } else {
        return control_instructions;
    };

    let existing_instructions =
        control_instructions.or_else(|| config.developer_instructions.clone());
    Some(match existing_instructions.as_deref() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{existing}\n\n{instructions}")
        }
        _ => instructions.to_string(),
    })
}
