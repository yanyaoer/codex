use crate::legacy_core::config::Config;
use codex_features::Feature;

pub(crate) const TERMINAL_VISUALIZATION_INSTRUCTIONS: &str = "\
- This surface is a terminal. When the formatting rules require a visual, include one in the final answer using compact ASCII diagrams, trees, timelines, or tables.
- Use tables for exact mappings or comparisons rather than collapsing known mappings into prose.
- Use trees for hierarchy or one-to-many relationships, and diagrams or timelines for sequence, change, or state transferred between records across event order.
- Use only ASCII characters in visuals.";

pub(crate) const INLINE_VISUALIZATION_INSTRUCTIONS: &str = "\
- This terminal automatically renders top-level fenced `d2`, `mermaid`, and `latex` blocks in final answers.
- When one of those formats materially improves the answer, emit the source block directly; do not invoke a skill, renderer, or tool, create an image file, or add a rendering directive.
- Keep the explanation as normal Markdown around the block. Use compact ASCII visuals for unsupported formats.";

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
