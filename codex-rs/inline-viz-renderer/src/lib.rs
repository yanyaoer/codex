use std::collections::HashSet;
use std::fs::OpenOptions;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use clap::ValueEnum;
use layout::backends::svg::SVGWriter;
use layout::core::color::Color as DotColor;
use layout::gv::GraphBuilder;
use layout::gv::parser::DotParser;
use layout::gv::parser::ast;
use mermaid_rs_renderer::RenderOptions;
use ratex_layout::LayoutOptions;
use ratex_layout::layout;
use ratex_layout::to_display_list;
use ratex_parser::parser::parse;
use ratex_svg::SvgOptions;
use ratex_svg::render_to_svg;
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_PNG_BYTES: usize = 20 * 1024 * 1024;
pub const RENDER_POLICY_VERSION: &str = "inline-viz-v1";
pub const FONT_IDENTITY: &str =
    "noto-sans-bfb7bb691513f12e734dc346c03a03f784912432d7e3fa8e56efcf906fe86b3d";

const MAX_SVG_BYTES: usize = 10 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 40_000_000;
const FONT_ALIAS: &str = "Codex Inline Viz Embedded Font";
const FONT_FAMILY: &str = "Noto Sans";
const MERMAID_FONT_CACHE_VERSION: &str = "v2-font-family-case";
const FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSans-wdth-wght.ttf");

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RenderFormat {
    Mermaid,
    Latex,
    Dot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSummary {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

pub fn render_to_path(source: &str, format: RenderFormat, output: &Path) -> Result<RenderSummary> {
    validate_common_source(source)?;
    let parent = output_parent(output)?;
    let (svg, needs_text_font) = match format {
        RenderFormat::Mermaid => (render_mermaid(source, &parent)?, true),
        RenderFormat::Latex => (render_latex(source)?, false),
        RenderFormat::Dot => (render_dot(source)?, true),
    };
    let (png, width, height) = rasterize(&svg, needs_text_font)?;
    if png.len() > MAX_PNG_BYTES {
        anyhow::bail!("rendered PNG exceeds the {MAX_PNG_BYTES}-byte limit");
    }

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .with_context(|| format!("create output {}", output.display()))?;
    file.write_all(&png)
        .with_context(|| format!("write output {}", output.display()))?;
    Ok(RenderSummary {
        width,
        height,
        bytes: png.len(),
    })
}

fn validate_common_source(source: &str) -> Result<()> {
    if source.trim().is_empty() {
        anyhow::bail!("visualization source is empty");
    }
    if source.len() > MAX_SOURCE_BYTES {
        anyhow::bail!("source exceeds the {MAX_SOURCE_BYTES}-byte limit");
    }
    if source.contains('\0') {
        anyhow::bail!("visualization source contains a NUL byte");
    }
    Ok(())
}

fn output_parent(output: &Path) -> Result<PathBuf> {
    if output.file_name().is_none() {
        anyhow::bail!("output path must name a file");
    }
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    parent
        .canonicalize()
        .with_context(|| format!("resolve output parent {}", parent.display()))
}

fn render_mermaid(source: &str, scratch_parent: &Path) -> Result<String> {
    validate_mermaid_source(source)?;
    validate_font_coverage(source)?;
    configure_mermaid_font_cache(scratch_parent)?;

    let parsed = mermaid_rs_renderer::parse_mermaid(source)
        .map_err(|error| anyhow::anyhow!("Mermaid parse error: {error}"))?;
    let mut options = RenderOptions::modern();
    options.theme.font_family = FONT_ALIAS.to_string();
    options.theme.background = "transparent".to_string();
    set_c4_font_family(&mut options, FONT_ALIAS);
    let layout =
        mermaid_rs_renderer::compute_layout(&parsed.graph, &options.theme, &options.layout);
    let svg = mermaid_rs_renderer::render_svg(&layout, &options.theme, &options.layout);
    Ok(map_svg_font_alias(&svg))
}

fn configure_mermaid_font_cache(parent: &Path) -> Result<()> {
    let cache_root = parent.join(".codex-inline-viz-cache");
    let cache_dir = cache_root.join("mmdr").join("font-cache");
    std::fs::create_dir_all(&cache_dir)?;

    // mermaid-rs-renderer 0.3.1 checks this isolated cache before it loads any
    // system font. The exact dependency pin and deterministic render test guard
    // this small integration against an upstream cache-contract change.
    let key = format!("{MERMAID_FONT_CACHE_VERSION}:{FONT_ALIAS}");
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let stem = format!("{:x}", hasher.finish());
    std::fs::write(cache_dir.join(format!("{stem}.font")), FONT_BYTES)?;
    std::fs::write(cache_dir.join(format!("{stem}.meta")), "0")?;

    // SAFETY: the helper calls this before Mermaid initializes its global text
    // measurer or starts any threads. The process renders one artifact and exits.
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root) };
    Ok(())
}

fn map_svg_font_alias(svg: &str) -> String {
    svg.replace(
        &format!("font-family=\"{FONT_ALIAS}\""),
        &format!("font-family=\"{FONT_FAMILY}\""),
    )
    .replace(
        &format!("font-family:{FONT_ALIAS};"),
        &format!("font-family:{FONT_FAMILY};"),
    )
    .replace(
        &format!("font-family: {FONT_ALIAS};"),
        &format!("font-family: {FONT_FAMILY};"),
    )
}

fn set_c4_font_family(options: &mut RenderOptions, family: &str) {
    let config = &mut options.layout.c4;
    config.person_font_family = family.to_string();
    config.external_person_font_family = family.to_string();
    config.system_font_family = family.to_string();
    config.external_system_font_family = family.to_string();
    config.system_db_font_family = family.to_string();
    config.external_system_db_font_family = family.to_string();
    config.system_queue_font_family = family.to_string();
    config.external_system_queue_font_family = family.to_string();
    config.boundary_font_family = family.to_string();
    config.message_font_family = family.to_string();
    config.container_font_family = family.to_string();
    config.external_container_font_family = family.to_string();
    config.container_db_font_family = family.to_string();
    config.external_container_db_font_family = family.to_string();
    config.container_queue_font_family = family.to_string();
    config.external_container_queue_font_family = family.to_string();
    config.component_font_family = family.to_string();
    config.external_component_font_family = family.to_string();
    config.component_db_font_family = family.to_string();
    config.external_component_db_font_family = family.to_string();
    config.component_queue_font_family = family.to_string();
    config.external_component_queue_font_family = family.to_string();
}

fn validate_mermaid_source(source: &str) -> Result<()> {
    let lower = source.to_ascii_lowercase();
    for forbidden in ["%%{", "http://", "https://", "file://", "javascript:"] {
        if lower.contains(forbidden) {
            anyhow::bail!("Mermaid source contains unsupported directive or URL: {forbidden}");
        }
    }
    for line in lower.lines().map(str::trim_start) {
        if line.starts_with("click ") || line.contains("@{ img:") || line.contains("@{ icon:") {
            anyhow::bail!("Mermaid links, images, and icon directives are unsupported");
        }
    }
    Ok(())
}

fn validate_font_coverage(source: &str) -> Result<()> {
    let face = ttf_parser::Face::parse(FONT_BYTES, 0).context("parse bundled Noto Sans")?;
    if let Some(character) = source
        .chars()
        .find(|character| !character.is_control() && face.glyph_index(*character).is_none())
    {
        anyhow::bail!(
            "bundled font does not contain glyph U+{:04X}; CJK and emoji are not supported",
            character as u32
        );
    }
    Ok(())
}

fn render_latex(source: &str) -> Result<String> {
    validate_latex_source(source)?;
    let ast = parse(source).map_err(|error| anyhow::anyhow!("LaTeX parse error: {error}"))?;
    let options = LayoutOptions::default()
        .with_style(MathStyle::Display)
        .with_color(Color::BLACK);
    let layout = layout(&ast, &options);
    let display_list = to_display_list(&layout);
    let svg = render_to_svg(
        &display_list,
        &SvgOptions {
            font_size: 128.0,
            padding: 16.0,
            stroke_width: 2.0,
            embed_glyphs: true,
            font_dir: String::new(),
        },
    );
    if svg.contains("<text") {
        anyhow::bail!("RaTeX produced a non-standalone SVG");
    }
    Ok(svg)
}

fn validate_latex_source(source: &str) -> Result<()> {
    if let Some(character) = source.chars().find(|character| !character.is_ascii()) {
        anyhow::bail!(
            "literal Unicode U+{:04X} is unsupported; use a LaTeX control sequence",
            character as u32
        );
    }
    const FORBIDDEN: &[&str] = &[
        "catcode",
        "csname",
        "def",
        "documentclass",
        "edef",
        "gdef",
        "href",
        "html",
        "include",
        "includegraphics",
        "input",
        "newcommand",
        "openin",
        "openout",
        "read",
        "renewcommand",
        "require",
        "special",
        "url",
        "usepackage",
        "write",
        "xdef",
    ];
    for command in control_sequences(source) {
        if FORBIDDEN.contains(&command.as_str()) {
            anyhow::bail!("unsupported LaTeX control sequence: \\{command}");
        }
    }
    Ok(())
}

fn control_sequences(source: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            continue;
        }
        let mut command = String::new();
        while let Some(next) = chars.peek() {
            if !next.is_ascii_alphabetic() {
                break;
            }
            command.push(next.to_ascii_lowercase());
            chars.next();
        }
        if !command.is_empty() {
            commands.push(command);
        }
    }
    commands
}

fn render_dot(source: &str) -> Result<String> {
    validate_font_coverage(source)?;
    if source.contains('<') {
        anyhow::bail!("DOT HTML labels are unsupported");
    }
    let mut parser = DotParser::new(source);
    let graph = parser
        .process()
        .map_err(|error| anyhow::anyhow!("DOT parse error: {error}"))?;
    validate_dot_graph(&graph)?;

    let mut builder = GraphBuilder::new();
    builder.visit_graph(&graph);
    let mut graph = builder.get();
    let mut writer = SVGWriter::new();
    graph.do_it(false, false, false, &mut writer);
    Ok(writer.finalize())
}

fn validate_dot_graph(graph: &ast::Graph) -> Result<()> {
    let mut nodes = HashSet::new();
    let mut edges = 0usize;
    for statement in &graph.list.list {
        match statement {
            ast::Stmt::SubGraph(_) => anyhow::bail!("DOT subgraphs and clusters are unsupported"),
            ast::Stmt::Node(node) => {
                validate_no_port(&node.id)?;
                nodes.insert(node.id.name.as_str());
                validate_dot_attributes(DotAttributeTarget::Node, &node.list)?;
            }
            ast::Stmt::Edge(edge) => {
                validate_no_port(&edge.from)?;
                nodes.insert(edge.from.name.as_str());
                for (node, _) in &edge.to {
                    validate_no_port(node)?;
                    nodes.insert(node.name.as_str());
                }
                edges += edge.to.len();
                validate_dot_attributes(DotAttributeTarget::Edge, &edge.list)?;
            }
            ast::Stmt::Attribute(attributes) => {
                let target = match attributes.target {
                    ast::AttrStmtTarget::Graph => DotAttributeTarget::Graph,
                    ast::AttrStmtTarget::Node => DotAttributeTarget::Node,
                    ast::AttrStmtTarget::Edge => DotAttributeTarget::Edge,
                };
                validate_dot_attributes(target, &attributes.list)?;
            }
        }
    }
    if nodes.len() > 256 || edges > 512 {
        anyhow::bail!("DOT graph exceeds the 256-node or 512-edge limit");
    }
    Ok(())
}

fn validate_no_port(node: &ast::NodeId) -> Result<()> {
    if node.port.is_some() {
        anyhow::bail!("DOT ports are unsupported");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DotAttributeTarget {
    Graph,
    Node,
    Edge,
}

fn validate_dot_attributes(
    target: DotAttributeTarget,
    attributes: &ast::AttributeList,
) -> Result<()> {
    for (name, value) in attributes.iter() {
        let name = name.to_ascii_lowercase();
        match (target, name.as_str()) {
            (DotAttributeTarget::Graph, "rankdir") if matches!(value.as_str(), "LR" | "TB") => {}
            (DotAttributeTarget::Node, "label") | (DotAttributeTarget::Edge, "label") => {}
            (DotAttributeTarget::Node, "shape")
                if matches!(value.as_str(), "box" | "circle" | "doublecircle") => {}
            (DotAttributeTarget::Node, "style") if value == "filled" => {}
            (DotAttributeTarget::Edge, "style") if value == "dashed" => {}
            (DotAttributeTarget::Node | DotAttributeTarget::Edge, "color")
            | (DotAttributeTarget::Node, "fillcolor") => validate_dot_color(value)?,
            (DotAttributeTarget::Node | DotAttributeTarget::Edge, "fontsize") => {
                validate_dot_number(name.as_str(), value, 6, 64)?;
            }
            (DotAttributeTarget::Node, "width") | (DotAttributeTarget::Edge, "penwidth") => {
                validate_dot_number(name.as_str(), value, 1, 8)?;
            }
            _ => anyhow::bail!("unsupported DOT attribute or value: {name}={value}"),
        }
    }
    Ok(())
}

fn validate_dot_color(value: &str) -> Result<()> {
    if DotColor::from_name(value).is_none() {
        anyhow::bail!("unsupported DOT color: {value}");
    }
    Ok(())
}

fn validate_dot_number(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let value = value
        .parse::<usize>()
        .with_context(|| format!("DOT {name} must be an integer"))?;
    if !(min..=max).contains(&value) {
        anyhow::bail!("DOT {name} must be between {min} and {max}");
    }
    Ok(())
}

fn rasterize(svg: &str, load_text_font: bool) -> Result<(Vec<u8>, u32, u32)> {
    if svg.len() > MAX_SVG_BYTES {
        anyhow::bail!("rendered SVG exceeds the {MAX_SVG_BYTES}-byte limit");
    }
    let mut options = usvg::Options {
        font_family: FONT_FAMILY.to_string(),
        ..Default::default()
    };
    if load_text_font {
        options.fontdb_mut().load_font_data(FONT_BYTES.to_vec());
        options.fontdb_mut().set_sans_serif_family(FONT_FAMILY);
        options.fontdb_mut().set_serif_family(FONT_FAMILY);
        options.fontdb_mut().set_monospace_family(FONT_FAMILY);
    }
    let tree = usvg::Tree::from_str(svg, &options).context("parse generated SVG")?;
    let size = tree.size().to_int_size();
    let width = size.width();
    let height = size.height();
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        anyhow::bail!("rendered image dimensions {width}x{height} exceed safety limits");
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).context("allocate PNG canvas")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let png = pixmap.encode_png().context("encode PNG")?;
    Ok((png, width, height))
}

#[cfg(test)]
mod lib_tests;
