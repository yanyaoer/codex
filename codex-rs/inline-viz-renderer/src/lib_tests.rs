use std::path::Path;

use tempfile::TempDir;

use super::*;

fn render(source: &str, format: RenderFormat) -> (TempDir, RenderSummary) {
    let temp = tempfile::tempdir().expect("create temp dir");
    let output = temp.path().join("render.png");
    let summary = render_to_path(source, format, &output).expect("render visualization");
    (temp, summary)
}

fn assert_transparent_png(path: &Path) {
    let bytes = std::fs::read(path).expect("read PNG");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let image = image::load_from_memory(&bytes)
        .expect("decode PNG")
        .into_rgba8();
    assert_eq!(image.get_pixel(0, 0).0[3], 0);
    assert!(image.pixels().any(|pixel| pixel.0[3] != 0));
}

#[test]
fn renders_mermaid_with_deterministic_bundled_font() {
    let source = "flowchart LR\n  A[WWWWWWWWWW] --> B[iiiiiiiiii]";
    let (first, first_summary) = render(source, RenderFormat::Mermaid);
    let (second, second_summary) = render(source, RenderFormat::Mermaid);
    assert_eq!(first_summary, second_summary);
    let first_png = std::fs::read(first.path().join("render.png")).expect("read first PNG");
    let second_png = std::fs::read(second.path().join("render.png")).expect("read second PNG");
    assert_eq!(first_png, second_png);
    assert_transparent_png(&first.path().join("render.png"));
}

#[test]
fn maps_only_svg_font_declarations() {
    let svg = format!(
        "<text font-family=\"{FONT_ALIAS}\">{FONT_ALIAS}</text><style>svg{{font-family:{FONT_ALIAS};}}</style>"
    );
    let mapped = map_svg_font_alias(&svg);
    assert!(mapped.contains(&format!("font-family=\"{FONT_FAMILY}\"")));
    assert!(mapped.contains(&format!(">{FONT_ALIAS}</text>")));
}

#[test]
fn rejects_mermaid_directives_urls_and_missing_glyphs() {
    for source in [
        "%%{init: {}}%%\nflowchart LR\nA --> B",
        "flowchart LR\nclick A href \"https://example.com\"",
        "flowchart LR\nA[中文] --> B",
    ] {
        let temp = tempfile::tempdir().expect("create temp dir");
        let error = render_to_path(source, RenderFormat::Mermaid, &temp.path().join("out.png"))
            .expect_err("reject unsafe Mermaid");
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn renders_latex_as_self_contained_transparent_paths() {
    let svg = render_latex(r"E = mc^2").expect("render SVG");
    assert!(svg.contains("<path"));
    assert!(!svg.contains("<text"));

    let (temp, summary) = render(r"E = mc^2", RenderFormat::Latex);
    assert!(summary.width > 0 && summary.height > 0);
    assert_transparent_png(&temp.path().join("render.png"));
}

#[test]
fn rejects_latex_io_links_and_user_macros() {
    for source in [
        r"\input{secret}",
        r"\href{https://example.com}{x}",
        r"\newcommand{\x}{1}",
        "中文",
    ] {
        let error = render_latex(source).expect_err("reject unsafe LaTeX");
        assert!(error.to_string().contains("unsupported"));
    }
}

#[test]
fn renders_bounded_dot_subset() {
    let source = r##"digraph G {
        rankdir=LR;
        node [shape=box, style=filled, fillcolor="#30343b", color="#a7b0bd"];
        user -> agent -> tool [style=dashed, color="#a7b0bd"];
    }"##;
    let (temp, summary) = render(source, RenderFormat::Dot);
    assert!(summary.width > 0 && summary.height > 0);
    assert_transparent_png(&temp.path().join("render.png"));
}

#[test]
fn rejects_dot_features_outside_the_documented_subset() {
    for source in [
        "digraph { subgraph cluster_0 { a -> b } }",
        "digraph { a:p -> b }",
        "digraph { a [label=<b>] }",
        "digraph { a [image=\"x.png\"] }",
        "digraph { a [URL=\"https://example.com\"] }",
    ] {
        let temp = tempfile::tempdir().expect("create temp dir");
        let error = render_to_path(source, RenderFormat::Dot, &temp.path().join("out.png"))
            .expect_err("reject unsupported DOT");
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn refuses_to_overwrite_parent_owned_output() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let output = temp.path().join("render.png");
    std::fs::write(&output, b"sentinel").expect("seed output");
    render_to_path("E=mc^2", RenderFormat::Latex, &output).expect_err("refuse overwrite");
    assert_eq!(std::fs::read(output).expect("read output"), b"sentinel");
}

#[test]
fn enforces_source_limit() {
    let source = "x".repeat(MAX_SOURCE_BYTES + 1);
    let temp = tempfile::tempdir().expect("create temp dir");
    let error = render_to_path(
        &source,
        RenderFormat::Latex,
        &temp.path().join("render.png"),
    )
    .expect_err("reject oversized source");
    assert!(error.to_string().contains("source exceeds"));
}
