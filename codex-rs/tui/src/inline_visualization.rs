//! Terminal fallback for assistant-authored inline visualization directives.

mod viewer;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::DateTime;
use codex_protocol::ThreadId;
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

pub(crate) const DIRECTIVE_PREFIX: &str = "::codex-inline-vis{";
const MAX_FRAGMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8_192;
const MAX_IMAGE_DECODE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct InlineVisualizationContext {
    visualizations_dir: PathBuf,
    thread_dir: PathBuf,
    preview_cache_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidatedVisualizationImage {
    pub(crate) path: PathBuf,
    pub(crate) file_name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl InlineVisualizationContext {
    pub(crate) fn new(codex_home: &Path, thread_id: ThreadId) -> Option<Self> {
        Some(Self {
            visualizations_dir: codex_home.join("visualizations"),
            thread_dir: thread_visualization_dir(codex_home, thread_id)?,
            preview_cache_dir: codex_home.join("cache").join("tui-visualizations"),
        })
    }

    pub(crate) fn from_config(
        config: &crate::legacy_core::config::Config,
        thread_id: ThreadId,
    ) -> Option<Self> {
        if config.features.enabled(codex_features::Feature::Artifact) {
            return Self::from_workspace(config.codex_home.as_path(), config.cwd.as_path());
        }
        Self::new(config.codex_home.as_path(), thread_id)
    }

    fn from_workspace(codex_home: &Path, workspace_dir: &Path) -> Option<Self> {
        if !workspace_dir.is_absolute() {
            return None;
        }
        Some(Self {
            visualizations_dir: workspace_dir.to_path_buf(),
            thread_dir: workspace_dir.to_path_buf(),
            preview_cache_dir: codex_home.join("cache").join("tui-visualizations"),
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
            Some("png") => Url::from_file_path(self.image_for(file)?.path).ok(),
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

    pub(crate) fn image_for(&self, file: &str) -> Option<ValidatedVisualizationImage> {
        let relative = single_file(file)?;
        if relative
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("png")
        {
            return None;
        }
        let (source_path, _thread_dir) = self.canonical_thread_file(relative)?;
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

        let digest = format!("{:x}", Sha256::digest(&bytes));
        fs::create_dir_all(&self.preview_cache_dir).ok()?;
        let cached_path = self.preview_cache_dir.join(format!("{digest}.png"));
        if !cached_path.is_file() {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.preview_cache_dir).ok()?;
            temporary.write_all(&bytes).ok()?;
            temporary.flush().ok()?;
            if let Err(error) = temporary.persist_noclobber(&cached_path)
                && (error.error.kind() != std::io::ErrorKind::AlreadyExists
                    || fs::read(&cached_path).ok().as_deref() != Some(bytes.as_slice()))
            {
                return None;
            }
        } else if fs::read(&cached_path).ok().as_deref() != Some(bytes.as_slice()) {
            return None;
        }

        Some(ValidatedVisualizationImage {
            path: cached_path,
            file_name: file.to_string(),
            width,
            height,
        })
    }

    pub(crate) fn latest_image(&self, markdown: &str) -> Option<ValidatedVisualizationImage> {
        directive_files_outside_code_blocks(markdown)
            .into_iter()
            .filter_map(|file| self.image_for(file))
            .next_back()
    }

    #[cfg(test)]
    pub(crate) fn thread_dir(&self) -> &Path {
        &self.thread_dir
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

fn thread_visualization_dir(codex_home: &Path, thread_id: ThreadId) -> Option<PathBuf> {
    let thread_id = thread_id.to_string();
    let uuid = Uuid::parse_str(&thread_id).ok()?;
    let timestamp = uuid.get_timestamp()?;
    let (seconds, nanos) = timestamp.to_unix();
    let created_at = DateTime::from_timestamp(i64::try_from(seconds).ok()?, nanos)?;
    Some(
        codex_home
            .join("visualizations")
            .join(created_at.format("%Y/%m/%d").to_string())
            .join(thread_id),
    )
}

pub(crate) struct InlineVisualizationRewrite<'a> {
    pub(crate) markdown: Cow<'a, str>,
    // Markdown rendering only recognizes web links. Random placeholders let the renderer build the
    // link ranges normally, then allow the caller to retarget only links created from directives.
    pub(crate) trusted_file_links: HashMap<String, TrustedFileLink>,
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
    if !markdown.contains(DIRECTIVE_PREFIX) {
        return InlineVisualizationRewrite {
            markdown: Cow::Borrowed(markdown),
            trusted_file_links: HashMap::new(),
        };
    }

    let code_block_ranges = code_block_ranges(markdown);
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
    }
}

fn code_block_ranges(markdown: &str) -> Vec<Range<usize>> {
    let mut code_block_ranges = Vec::new();
    let mut code_block_start = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    code_block_ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }
    if let Some(start) = code_block_start {
        code_block_ranges.push(start..markdown.len());
    }
    code_block_ranges
}

fn directive_files_outside_code_blocks(markdown: &str) -> Vec<&str> {
    let code_block_ranges = code_block_ranges(markdown);
    let mut files = Vec::new();
    let mut source_offset = 0;
    for source_line in markdown.split_inclusive('\n') {
        let line_start = source_offset;
        source_offset += source_line.len();
        let trimmed = source_line.trim();
        let is_code = code_block_ranges
            .iter()
            .any(|range| range.start < source_offset && line_start < range.end);
        if !is_code && let Some(file) = parse_directive_file(trimmed) {
            files.push(file);
        }
    }
    files
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
    let mut bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    format!("https://codex.invalid/inline-visualization/{token}")
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
