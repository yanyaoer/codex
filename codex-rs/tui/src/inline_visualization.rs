//! Render-only handling for assistant-authored inline visualization directives.

mod viewer;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::DateTime;
use codex_protocol::ThreadId;
use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use rand::RngCore as _;
use sha2::Digest as _;
use sha2::Sha256;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::io::Write as _;
use std::ops::Range;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

use self::viewer::materialize_document;
use crate::inline_image::InlineImage;
use crate::inline_image::KittyTransport;
use crate::inline_image::MARKDOWN_LANGUAGE;
use crate::inline_image::detect_kitty_transport;

pub(crate) const DIRECTIVE_PREFIX: &str = "::codex-inline-vis{";
const MAX_FRAGMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8_192;
const MAX_IMAGE_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_INLINE_WIDTH: usize = 100;

#[derive(Clone, Debug)]
pub(crate) struct InlineVisualizationContext {
    visualizations_dir: PathBuf,
    thread_dir: PathBuf,
    artifact_dir: Option<PathBuf>,
    image_cache_dir: PathBuf,
    kitty_transport: Option<KittyTransport>,
}

#[derive(Debug)]
struct ValidatedImage {
    path: PathBuf,
    digest: [u8; 32],
    width: u32,
    height: u32,
}

impl InlineVisualizationContext {
    pub(crate) fn new(codex_home: &Path, thread_id: ThreadId) -> Option<Self> {
        Self::new_with_writable_roots(codex_home, thread_id, std::iter::empty())
    }

    pub(crate) fn from_config(
        config: &crate::legacy_core::config::Config,
        thread_id: ThreadId,
    ) -> Option<Self> {
        let writable_roots = config
            .permissions
            .file_system_sandbox_policy()
            .get_writable_roots_with_cwd(config.cwd.as_path());
        let mut context = Self::new_with_writable_roots(
            config.codex_home.as_path(),
            thread_id,
            writable_roots.iter().map(|root| root.root.as_path()),
        )?;
        if config.features.enabled(codex_features::Feature::Artifact) && config.cwd.is_absolute() {
            context.artifact_dir = Some(config.cwd.to_path_buf());
            context.kitty_transport = detect_kitty_transport();
        }
        Some(context)
    }

    fn new_with_writable_roots<'a>(
        codex_home: &Path,
        thread_id: ThreadId,
        writable_roots: impl IntoIterator<Item = &'a Path>,
    ) -> Option<Self> {
        let thread_id = thread_id.to_string();
        let uuid = Uuid::parse_str(&thread_id).ok()?;
        let timestamp = uuid.get_timestamp()?;
        let (seconds, nanos) = timestamp.to_unix();
        let created_at = DateTime::from_timestamp(i64::try_from(seconds).ok()?, nanos)?;
        let visualizations_dir = codex_home.join("visualizations");
        let granted_thread_dirs = writable_roots
            .into_iter()
            .filter(|root| is_visualization_thread_dir(&visualizations_dir, root))
            .collect::<Vec<_>>();
        let thread_dir = match granted_thread_dirs.as_slice() {
            [thread_dir] => (*thread_dir).to_path_buf(),
            _ => visualizations_dir
                .join(created_at.format("%Y/%m/%d").to_string())
                .join(thread_id),
        };
        Some(Self {
            visualizations_dir,
            thread_dir,
            artifact_dir: None,
            image_cache_dir: codex_home.join("cache").join("tui-visualizations"),
            kitty_transport: None,
        })
    }

    fn link_for(&self, file: &str) -> Option<Url> {
        let relative = single_file(file)?;
        match relative
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("html") => {
                let (fragment_path, thread_dir) = self.canonical_thread_file(relative)?;
                let viewer_path = materialize_document(&fragment_path, &thread_dir).ok()?;
                Url::from_file_path(viewer_path).ok()
            }
            Some("png") => Url::from_file_path(self.validated_image(file)?.path).ok(),
            _ => None,
        }
    }

    fn canonical_thread_file(&self, relative: &Path) -> Option<(PathBuf, PathBuf)> {
        let visualizations_dir = fs::canonicalize(&self.visualizations_dir).ok()?;
        let thread_dir = fs::canonicalize(&self.thread_dir).ok()?;
        if !thread_dir.starts_with(&visualizations_dir) {
            return None;
        }
        let fragment_path = fs::canonicalize(thread_dir.join(relative)).ok()?;
        if !fragment_path.starts_with(&thread_dir) {
            return None;
        }
        Some((fragment_path, thread_dir))
    }

    fn validated_image(&self, file: &str) -> Option<ValidatedImage> {
        let relative = single_file(file)?;
        if relative
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("png")
        {
            return None;
        }
        let artifact_dir = fs::canonicalize(self.artifact_dir.as_ref()?).ok()?;
        let source_path = fs::canonicalize(artifact_dir.join(relative)).ok()?;
        if !source_path.starts_with(&artifact_dir) {
            return None;
        }

        let metadata = fs::metadata(&source_path).ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
            return None;
        }
        let bytes = fs::read(&source_path).ok()?;
        if u64::try_from(bytes.len()).ok()? != metadata.len() {
            return None;
        }

        let mut reader =
            image::ImageReader::with_format(Cursor::new(bytes.as_slice()), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
        reader.limits(limits);
        let decoded = reader.decode().ok()?;
        let width = decoded.width();
        let height = decoded.height();
        if width == 0 || height == 0 {
            return None;
        }

        let digest = Sha256::digest(&bytes);
        fs::create_dir_all(&self.image_cache_dir).ok()?;
        let cached_path = self.image_cache_dir.join(format!("{digest:x}.png"));
        if cached_path.is_file() {
            if fs::read(&cached_path).ok().as_deref() != Some(bytes.as_slice()) {
                return None;
            }
        } else {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.image_cache_dir).ok()?;
            temporary.write_all(&bytes).ok()?;
            temporary.flush().ok()?;
            if let Err(error) = temporary.persist_noclobber(&cached_path)
                && (error.error.kind() != std::io::ErrorKind::AlreadyExists
                    || fs::read(&cached_path).ok().as_deref() != Some(bytes.as_slice()))
            {
                return None;
            }
        }

        Some(ValidatedImage {
            path: cached_path,
            digest: digest.into(),
            width,
            height,
        })
    }

    fn inline_image_for(&self, file: &str, available_width: usize) -> Option<InlineImage> {
        let transport = self.kitty_transport?;
        let image = self.validated_image(file)?;
        InlineImage::new(
            image.path,
            &image.digest,
            image.width,
            image.height,
            available_width,
            transport,
        )
    }
}

fn single_file(file: &str) -> Option<&Path> {
    let relative = Path::new(file);
    matches!(
        relative.components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    )
    .then_some(relative)
}

fn is_visualization_thread_dir(visualizations_dir: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(visualizations_dir) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    matches!(
        components.as_slice(),
        [
            Component::Normal(_),
            Component::Normal(_),
            Component::Normal(_),
            Component::Normal(thread_id)
        ] if Uuid::parse_str(&thread_id.to_string_lossy()).is_ok()
    )
}

pub(crate) struct InlineVisualizationRewrite<'a> {
    pub(crate) markdown: Cow<'a, str>,
    pub(crate) trusted_file_links: HashMap<String, TrustedFileLink>,
    pub(crate) trusted_inline_images: HashMap<String, InlineImage>,
}

pub(crate) struct TrustedFileLink {
    pub(crate) destination: Url,
    pub(crate) markdown_label: String,
    pub(crate) display_label: String,
    pub(crate) markdown_destination_label: String,
}

pub(crate) fn rewrite_inline_visualizations<'a>(
    markdown: &'a str,
    context: Option<&InlineVisualizationContext>,
) -> InlineVisualizationRewrite<'a> {
    rewrite_inline_visualizations_impl(markdown, context, /*available_width*/ None)
}

pub(crate) fn rewrite_inline_visualizations_for_terminal<'a>(
    markdown: &'a str,
    context: Option<&InlineVisualizationContext>,
    width: Option<usize>,
) -> InlineVisualizationRewrite<'a> {
    rewrite_inline_visualizations_impl(
        markdown,
        context,
        Some(width.unwrap_or(DEFAULT_INLINE_WIDTH).max(1)),
    )
}

fn rewrite_inline_visualizations_impl<'a>(
    markdown: &'a str,
    context: Option<&InlineVisualizationContext>,
    available_width: Option<usize>,
) -> InlineVisualizationRewrite<'a> {
    if !markdown.contains(DIRECTIVE_PREFIX) {
        return InlineVisualizationRewrite {
            markdown: Cow::Borrowed(markdown),
            trusted_file_links: HashMap::new(),
            trusted_inline_images: HashMap::new(),
        };
    }

    let (markdown, trusted_inline_images) = match (context, available_width) {
        (Some(context), Some(available_width)) => {
            replace_artifact_blocks(markdown, context, available_width)
        }
        _ => (Cow::Borrowed(markdown), HashMap::new()),
    };
    let code_block_ranges = code_block_ranges(&markdown);
    let mut rewritten = String::with_capacity(markdown.len());
    let mut trusted_file_links = HashMap::new();
    let mut source_offset = 0;
    for source_line in markdown.split_inclusive('\n') {
        let line_start = source_offset;
        source_offset += source_line.len();
        let (line, newline) = source_line
            .strip_suffix('\n')
            .map_or((source_line, ""), |line| (line, "\n"));
        let trimmed = line.trim();
        let is_code = code_block_ranges
            .iter()
            .any(|range| range.start < source_offset && line_start < range.end);
        if is_code || !trimmed.starts_with(DIRECTIVE_PREFIX) {
            rewritten.push_str(line);
        } else if let Some(file) = parse_directive_file(trimmed) {
            if let Some(destination) = context.and_then(|context| context.link_for(file)) {
                let placeholder = link_placeholder();
                let (markdown_label, display_label) = visualization_link_labels(file);
                let markdown_destination_label = escape_markdown_label(destination.as_str());
                rewritten.push_str(&format!(
                    "{markdown_label}  \n[{markdown_destination_label}]({placeholder})"
                ));
                trusted_file_links.insert(
                    placeholder,
                    TrustedFileLink {
                        destination,
                        markdown_label,
                        display_label,
                        markdown_destination_label,
                    },
                );
            } else {
                rewritten.push_str("_Visualization unavailable on this device._");
            }
        } else if trimmed.ends_with('}') {
            rewritten.push_str("_Visualization unavailable on this device._");
        }
        rewritten.push_str(newline);
    }
    InlineVisualizationRewrite {
        markdown: Cow::Owned(rewritten),
        trusted_file_links,
        trusted_inline_images,
    }
}

struct ArtifactReplacement {
    range: Range<usize>,
    marker: String,
    image: InlineImage,
}

fn replace_artifact_blocks<'a>(
    markdown: &'a str,
    context: &InlineVisualizationContext,
    available_width: usize,
) -> (Cow<'a, str>, HashMap<String, InlineImage>) {
    let mut replacements = Vec::new();
    let mut container_depth = 0usize;
    let mut candidate_start = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(
                Tag::BlockQuote | Tag::List(_) | Tag::Item | Tag::FootnoteDefinition(_),
            ) => container_depth += 1,
            Event::End(
                TagEnd::BlockQuote | TagEnd::List(_) | TagEnd::Item | TagEnd::FootnoteDefinition,
            ) => container_depth = container_depth.saturating_sub(1),
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language)))
                if container_depth == 0 && is_artifact_language(&language) =>
            {
                candidate_start = Some(range.start);
            }
            Event::End(TagEnd::CodeBlock) => {
                let Some(start) = candidate_start.take() else {
                    continue;
                };
                let Some((directive_end, file)) = adjacent_directive(markdown, range.end) else {
                    continue;
                };
                let Some(image) = context.inline_image_for(file, available_width) else {
                    continue;
                };
                replacements.push(ArtifactReplacement {
                    range: start..directive_end,
                    marker: image_placeholder(),
                    image,
                });
            }
            _ => {}
        }
    }
    if replacements.is_empty() {
        return (Cow::Borrowed(markdown), HashMap::new());
    }

    let mut rewritten = String::with_capacity(markdown.len());
    let mut images = HashMap::new();
    let mut source_offset = 0;
    for replacement in replacements {
        rewritten.push_str(&markdown[source_offset..replacement.range.start]);
        let newline = if markdown[replacement.range.clone()].ends_with('\n') {
            "\n"
        } else {
            Default::default()
        };
        rewritten.push_str(&format!(
            "```{MARKDOWN_LANGUAGE}\n{}\n```{newline}",
            replacement.marker
        ));
        source_offset = replacement.range.end;
        images.insert(replacement.marker, replacement.image);
    }
    rewritten.push_str(&markdown[source_offset..]);
    (Cow::Owned(rewritten), images)
}

fn is_artifact_language(language: &str) -> bool {
    language
        .split([',', ' ', '\t'])
        .next()
        .is_some_and(|language| {
            language.eq_ignore_ascii_case("d2") || language.eq_ignore_ascii_case("latex")
        })
}

fn adjacent_directive(markdown: &str, block_end: usize) -> Option<(usize, &str)> {
    let remainder = markdown.get(block_end..)?;
    let content_start = block_end + remainder.find(|character: char| !character.is_whitespace())?;
    let line_start = markdown[..content_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let indent = &markdown[line_start..content_start];
    if indent.contains('\t') || indent.chars().count() > 3 {
        return None;
    }
    let remainder = markdown.get(content_start..)?;
    let relative_end = remainder.find('\n').unwrap_or(remainder.len());
    let line = &remainder[..relative_end];
    let file = parse_directive_file(line.trim())?;
    let line_end = content_start + relative_end;
    let directive_end = line_end + usize::from(markdown.as_bytes().get(line_end) == Some(&b'\n'));
    Some((directive_end, file))
}

fn code_block_ranges(markdown: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = start.take() {
                    ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }
    if let Some(start) = start {
        ranges.push(start..markdown.len());
    }
    ranges
}

fn visualization_link_labels(file: &str) -> (String, String) {
    let name = Path::new(file)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("generated");
    let escaped_name = escape_markdown_label(name);
    (
        format!("Open {escaped_name} visualization in the browser"),
        format!("Open {name} visualization in the browser"),
    )
}

fn escape_markdown_label(label: &str) -> String {
    let mut escaped = String::with_capacity(label.len());
    for character in label.chars() {
        if character.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn link_placeholder() -> String {
    format!(
        "https://codex.invalid/inline-visualization/{}",
        random_token()
    )
}

fn image_placeholder() -> String {
    format!("codex-inline-image-{}", random_token())
}

fn random_token() -> String {
    let mut bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn parse_directive_file(directive: &str) -> Option<&str> {
    let attributes = directive
        .strip_prefix(DIRECTIVE_PREFIX)?
        .strip_suffix('}')?
        .trim();
    let value = attributes.strip_prefix("file=\"")?.strip_suffix('"')?;
    (!value.is_empty() && !value.contains('"')).then_some(value)
}

#[cfg(test)]
#[path = "inline_visualization_tests.rs"]
mod tests;
