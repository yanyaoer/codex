//! Bounded helper-process rendering for top-level visualization fences.

use std::fs;
use std::fs::File;
use std::io::Cursor;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use sha2::Digest as _;
use sha2::Sha256;
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use super::InlineVisualizationContext;

pub(super) const MAX_ARTIFACTS_PER_MESSAGE: usize = 8;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8_192;
const MAX_IMAGE_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_LOG_BYTES: u64 = 8 * 1024;
const NATIVE_FILE_PREFIX: &str = "codex-inline-viz-";

// Keep this identity synchronized with the sidecar's public render policy, bundled font, and
// output-affecting options. It deliberately lives in the TUI instead of linking the renderer
// library, which would pull the renderer dependency graph into the primary Codex process.
const RENDER_IDENTITY: &str = concat!(
    "inline-viz-v1;",
    "font=noto-sans-bfb7bb691513f12e734dc346c03a03f784912432d7e3fa8e56efcf906fe86b3d;",
    "mermaid=modern-transparent;",
    "latex=display-128-padding16-stroke2-glyphs;",
    "dot=layout-rs-bounded-v1;",
    "raster=resvg-transparent-v1",
);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum NativeArtifactFormat {
    Mermaid,
    Latex,
    Dot,
}

impl NativeArtifactFormat {
    fn argument(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Latex => "latex",
            Self::Dot => "dot",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeArtifact {
    pub(super) format: NativeArtifactFormat,
    pub(super) source: String,
}

pub(super) fn format_from_language(language: &str) -> Option<NativeArtifactFormat> {
    let language = language.split([',', ' ', '\t']).next()?;
    if language.eq_ignore_ascii_case("mermaid") {
        Some(NativeArtifactFormat::Mermaid)
    } else if language.eq_ignore_ascii_case("latex") {
        Some(NativeArtifactFormat::Latex)
    } else if language.eq_ignore_ascii_case("dot") {
        Some(NativeArtifactFormat::Dot)
    } else {
        None
    }
}

pub(super) fn artifact_blocks(markdown: &str) -> Vec<NativeArtifact> {
    let mut artifacts = Vec::new();
    let mut container_depth = 0usize;
    let mut candidate: Option<NativeArtifact> = None;
    for event in Parser::new_ext(markdown, Options::empty()) {
        match event {
            Event::Start(
                Tag::BlockQuote | Tag::List(_) | Tag::Item | Tag::FootnoteDefinition(_),
            ) => container_depth += 1,
            Event::End(
                TagEnd::BlockQuote | TagEnd::List(_) | TagEnd::Item | TagEnd::FootnoteDefinition,
            ) => container_depth = container_depth.saturating_sub(1),
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language)))
                if container_depth == 0 =>
            {
                candidate = format_from_language(&language).map(|format| NativeArtifact {
                    format,
                    source: String::new(),
                });
            }
            Event::Text(text) => {
                if let Some(candidate) = candidate.as_mut() {
                    candidate.source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(candidate) = candidate.take() {
                    artifacts.push(candidate);
                }
            }
            _ => {}
        }
    }
    artifacts
}

pub(super) fn contains_artifact_blocks(markdown: &str) -> bool {
    (markdown.contains("```") || markdown.contains("~~~")) && !artifact_blocks(markdown).is_empty()
}

pub(super) fn artifact_file(format: NativeArtifactFormat, source: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"codex-inline-viz\0");
    digest.update(RENDER_IDENTITY.as_bytes());
    digest.update([0]);
    digest.update(format.argument().as_bytes());
    digest.update([0]);
    digest.update(source.as_bytes());
    format!("{NATIVE_FILE_PREFIX}{:x}.png", digest.finalize())
}

pub(super) fn is_native_artifact_file(file: &str) -> bool {
    file.strip_prefix(NATIVE_FILE_PREFIX)
        .and_then(|file| file.strip_suffix(".png"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(super) async fn render_artifacts(
    context: &InlineVisualizationContext,
    markdown: &str,
) -> usize {
    if !context.native_rendering_enabled {
        return 0;
    }
    let artifacts = artifact_blocks(markdown);
    if artifacts.is_empty() || fs::create_dir_all(&context.thread_dir).is_err() {
        return 0;
    }

    let mut rendered = 0;
    for artifact in artifacts.into_iter().take(MAX_ARTIFACTS_PER_MESSAGE) {
        match compile_artifact(context, &artifact).await {
            Ok((_, true)) => rendered += 1,
            Ok((_, false)) => {}
            Err(error) => tracing::warn!(
                format = ?artifact.format,
                %error,
                "failed to render inline visualization"
            ),
        }
    }
    rendered
}

pub(super) async fn compile_artifact(
    context: &InlineVisualizationContext,
    artifact: &NativeArtifact,
) -> Result<(String, bool)> {
    if !context.native_rendering_enabled {
        bail!("inline visualization rendering is disabled");
    }
    fs::create_dir_all(&context.thread_dir)
        .context("create inline visualization thread directory")?;
    let file = artifact_file(artifact.format, &artifact.source);
    let created = render_artifact(&context.thread_dir, &context.renderer_program, artifact).await?;
    Ok((file, created))
}

async fn render_artifact(
    thread_dir: &Path,
    renderer_program: &Path,
    artifact: &NativeArtifact,
) -> Result<bool> {
    validate_source(&artifact.source)?;
    let destination = thread_dir.join(artifact_file(artifact.format, &artifact.source));
    if destination.is_file() {
        validated_png_bytes(&destination, thread_dir)
            .context("validate existing inline visualization artifact")?;
        return Ok(false);
    }

    let working = tempfile::Builder::new()
        .prefix(".inline-viz-render-")
        .tempdir_in(thread_dir)
        .context("create inline visualization work directory")?;
    let output = working.path().join("output.png");
    let log_path = working.path().join("stderr.log");
    let stderr = File::create(&log_path)?;
    let mut command = Command::new(renderer_program);
    command
        .args(["render", "--format", artifact.format.argument(), "--output"])
        .arg(&output)
        .current_dir(working.path())
        .env_clear()
        .env("HOME", working.path())
        .env("TMPDIR", working.path())
        .env("TMP", working.path())
        .env("TEMP", working.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("launch {}", renderer_program.display()))?;
    let status = tokio::time::timeout(COMMAND_TIMEOUT, async {
        let mut stdin = child
            .stdin
            .take()
            .context("open inline visualization renderer stdin")?;
        stdin.write_all(artifact.source.as_bytes()).await?;
        stdin.shutdown().await?;
        child.wait().await.map_err(anyhow::Error::from)
    })
    .await;
    let status = match status {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("inline visualization renderer timed out after 30 seconds");
        }
    };
    if !status.success() {
        bail!(
            "inline visualization renderer failed: {}",
            bounded_file_text(&log_path)
        );
    }

    let bytes = validated_png_bytes(&output, working.path())?;
    let mut staged = NamedTempFile::new_in(thread_dir)?;
    staged.write_all(&bytes)?;
    staged.flush()?;
    match staged.persist_noclobber(&destination) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(&destination).ok().as_deref() == Some(bytes.as_slice()) {
                Ok(false)
            } else {
                bail!("inline visualization cache collision")
            }
        }
        Err(error) => Err(error.error.into()),
    }
}

fn validate_source(source: &str) -> Result<()> {
    if source.trim().is_empty() {
        bail!("visualization source is empty");
    }
    if source.len() > MAX_SOURCE_BYTES {
        bail!("visualization source exceeds the {MAX_SOURCE_BYTES}-byte limit");
    }
    if source.contains('\0') {
        bail!("visualization source contains a NUL byte");
    }
    Ok(())
}

fn validated_png_bytes(output: &Path, working_dir: &Path) -> Result<Vec<u8>> {
    let working_dir = working_dir
        .canonicalize()
        .context("resolve inline visualization work directory")?;
    let output = output
        .canonicalize()
        .context("renderer did not create its PNG output")?;
    if !output.starts_with(&working_dir) {
        bail!("renderer output escaped its parent-owned temporary directory");
    }
    let metadata = fs::metadata(&output)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OUTPUT_BYTES {
        bail!("renderer did not produce a bounded PNG");
    }
    let bytes = fs::read(&output)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len()
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        bail!("renderer did not produce a valid PNG file");
    }

    let mut reader =
        image::ImageReader::with_format(Cursor::new(bytes.as_slice()), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().context("decode renderer PNG")?;
    if decoded.width() == 0 || decoded.height() == 0 {
        bail!("renderer produced an empty PNG");
    }
    Ok(bytes)
}

fn bounded_file_text(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return "no diagnostic output".to_string();
    };
    let mut text = String::new();
    let _ = file.take(COMMAND_LOG_BYTES).read_to_string(&mut text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        "no diagnostic output".to_string()
    } else {
        text
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
