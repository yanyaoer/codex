//! Native rendering for assistant-authored D2, Mermaid, and LaTeX fences.

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
use std::env;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::process::Command;

use super::InlineVisualizationContext;

const RENDER_POLICY_VERSION: u8 = 1;
const LATEX_RENDER_POLICY_VERSION: u8 = 2;
const MAX_ARTIFACTS_PER_MESSAGE: usize = 8;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: u64 = 20 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_LOG_BYTES: u64 = 8 * 1024;
const NATIVE_FILE_PREFIX: &str = "codex-inline-viz-";
const MERMAID_CONFIG: &str = r#"{"securityLevel":"strict","htmlLabels":false,"deterministicIds":true,"deterministicIDSeed":"codex-inline-viz","maxEdges":500,"flowchart":{"htmlLabels":false}}"#;
pub(super) const PUPPETEER_CONFIG: &str = r#"{"headless":true,"args":["--disable-background-networking","--disable-component-update","--disable-default-apps","--disable-sync","--no-default-browser-check","--no-first-run","--proxy-server=http://127.0.0.1:9","--proxy-bypass-list=<-loopback>"]}"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeArtifactFormat {
    D2,
    Mermaid,
    Latex,
}

impl NativeArtifactFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::D2 => "d2",
            Self::Mermaid => "mmd",
            Self::Latex => "tex",
        }
    }

    fn label(self) -> &'static [u8] {
        match self {
            Self::D2 => b"d2",
            Self::Mermaid => b"mermaid",
            Self::Latex => b"latex",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeArtifact {
    pub(super) format: NativeArtifactFormat,
    pub(super) source: String,
}

#[derive(Clone, Debug)]
struct RenderCommands {
    renderer: PathBuf,
    rasterizer: PathBuf,
}

pub(super) fn format_from_language(language: &str) -> Option<NativeArtifactFormat> {
    let language = language.split([',', ' ', '\t']).next()?;
    if language.eq_ignore_ascii_case("d2") {
        Some(NativeArtifactFormat::D2)
    } else if language.eq_ignore_ascii_case("mermaid") {
        Some(NativeArtifactFormat::Mermaid)
    } else if language.eq_ignore_ascii_case("latex") {
        Some(NativeArtifactFormat::Latex)
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
    markdown.contains("```") && !artifact_blocks(markdown).is_empty()
}

pub(super) fn artifact_file(format: NativeArtifactFormat, source: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"codex-inline-viz\0");
    digest.update([match format {
        NativeArtifactFormat::Latex => LATEX_RENDER_POLICY_VERSION,
        NativeArtifactFormat::D2 | NativeArtifactFormat::Mermaid => RENDER_POLICY_VERSION,
    }]);
    digest.update(format.label());
    digest.update([0]);
    digest.update(source.as_bytes());
    let digest = digest.finalize();
    format!("{NATIVE_FILE_PREFIX}{digest:x}.png")
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
    let commands = RenderCommands::resolve(artifact.format, &context.managed_bin_dir)?;
    let file = artifact_file(artifact.format, &artifact.source);
    let created = render_artifact(&context.thread_dir, &commands, artifact).await?;
    Ok((file, created))
}

impl RenderCommands {
    fn resolve(format: NativeArtifactFormat, managed_bin_dir: &Path) -> Result<Self> {
        let (variable, command, managed_dir) = match format {
            NativeArtifactFormat::D2 => ("CODEX_INLINE_VIZ_D2_COMMAND", "d2", None),
            NativeArtifactFormat::Mermaid => ("CODEX_INLINE_VIZ_MMDC_COMMAND", "mmdc", None),
            NativeArtifactFormat::Latex => (
                "CODEX_INLINE_VIZ_RATEX_COMMAND",
                ratex_binary_name(),
                Some(managed_bin_dir),
            ),
        };
        Ok(Self {
            renderer: resolve_configured(variable, command, managed_dir)?,
            rasterizer: resolve_configured("CODEX_INLINE_VIZ_RSVG_COMMAND", "rsvg-convert", None)?,
        })
    }
}

async fn render_artifact(
    thread_dir: &Path,
    commands: &RenderCommands,
    artifact: &NativeArtifact,
) -> Result<bool> {
    validate_source(artifact)?;
    let destination = thread_dir.join(artifact_file(artifact.format, &artifact.source));
    if destination.is_file() {
        return Ok(false);
    }
    let working = tempfile::Builder::new()
        .prefix(".inline-viz-render-")
        .tempdir_in(thread_dir)
        .context("create inline visualization work directory")?;
    let source = working
        .path()
        .join(format!("source.{}", artifact.format.extension()));
    let svg = working.path().join("output.svg");
    let png = working.path().join("output.png");
    fs::write(&source, &artifact.source).context("write inline visualization source")?;

    match artifact.format {
        NativeArtifactFormat::D2 => {
            run_command(
                &commands.renderer,
                &[
                    "--theme=0".into(),
                    "--pad=24".into(),
                    source.as_os_str().into(),
                    svg.as_os_str().into(),
                ],
                working.path(),
                /*use_user_home*/ false,
            )
            .await?;
        }
        NativeArtifactFormat::Mermaid => {
            let mermaid_config = working.path().join("mermaid.json");
            let puppeteer_config = working.path().join("puppeteer.json");
            fs::write(&mermaid_config, MERMAID_CONFIG)?;
            fs::write(&puppeteer_config, PUPPETEER_CONFIG)?;
            run_command(
                &commands.renderer,
                &[
                    "--input".into(),
                    source.as_os_str().into(),
                    "--output".into(),
                    svg.as_os_str().into(),
                    "--outputFormat".into(),
                    "svg".into(),
                    "--backgroundColor".into(),
                    "transparent".into(),
                    "--configFile".into(),
                    mermaid_config.as_os_str().into(),
                    "--puppeteerConfigFile".into(),
                    puppeteer_config.as_os_str().into(),
                    "--svgId".into(),
                    "codex-inline-viz".into(),
                    "--quiet".into(),
                ],
                working.path(),
                /*use_user_home*/ true,
            )
            .await?;
        }
        NativeArtifactFormat::Latex => {
            let output_dir = working.path().join("latex");
            fs::create_dir(&output_dir)?;
            run_command(
                &commands.renderer,
                &[
                    "--input".into(),
                    source.as_os_str().into(),
                    "--output-dir".into(),
                    output_dir.as_os_str().into(),
                    "--dpr".into(),
                    "4".into(),
                    "--color".into(),
                    "black".into(),
                    "--office-compatible-colors".into(),
                ],
                working.path(),
                /*use_user_home*/ false,
            )
            .await?;
            fs::rename(output_dir.join("0001.svg"), &svg)
                .context("RaTeX did not produce one SVG")?;
        }
    }

    let mut rasterizer_args = vec![
        "--dpi-x".into(),
        "144".into(),
        "--dpi-y".into(),
        "144".into(),
        "--zoom".into(),
        "1".into(),
    ];
    if artifact.format != NativeArtifactFormat::Latex {
        rasterizer_args.extend(["--background-color".into(), "white".into()]);
    }
    rasterizer_args.extend([
        "--output".into(),
        png.as_os_str().into(),
        svg.as_os_str().into(),
    ]);
    run_command(
        &commands.rasterizer,
        &rasterizer_args,
        working.path(),
        /*use_user_home*/ false,
    )
    .await?;

    let bytes = fs::read(&png).context("read rendered inline visualization")?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_OUTPUT_BYTES
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        bail!("renderer did not produce a bounded PNG");
    }
    let mut staged = NamedTempFile::new_in(thread_dir)?;
    staged.write_all(&bytes)?;
    staged.flush()?;
    match staged.persist_noclobber(&destination) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error.into()),
    }
}

async fn run_command(
    executable: &Path,
    args: &[OsString],
    cwd: &Path,
    use_user_home: bool,
) -> Result<()> {
    let log_path = cwd.join("command.log");
    let stdout = File::create(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", runtime_path())
        .env(
            "HOME",
            use_user_home
                .then(dirs::home_dir)
                .flatten()
                .unwrap_or_else(|| cwd.to_path_buf()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    copy_environment(&mut command, "LANG");
    copy_environment(&mut command, "LC_ALL");
    copy_environment(&mut command, "LC_CTYPE");
    if let Some(cache) = puppeteer_cache_dir() {
        command.env("PUPPETEER_CACHE_DIR", cache);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("launch {}", executable.display()))?;
    let status = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("{} timed out", executable.display());
        }
    };
    if !status.success() {
        bail!(
            "{} failed: {}",
            executable.display(),
            bounded_file_text(&log_path)
        );
    }
    Ok(())
}

fn validate_source(artifact: &NativeArtifact) -> Result<()> {
    let source = artifact.source.as_str();
    if source.trim().is_empty() {
        bail!("artifact source is empty");
    }
    if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
        bail!("artifact source is invalid or too large");
    }
    if source
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        bail!("artifact source contains a control character");
    }
    let lower = source.to_ascii_lowercase();
    match artifact.format {
        NativeArtifactFormat::D2 => {
            if source.lines().any(|line| {
                let code = line.split('#').next().unwrap_or_default();
                code.trim_start().starts_with('@') || code.contains(": @") || code.contains("icon:")
            }) {
                bail!("D2 imports and icons are disabled for automatic rendering");
            }
        }
        NativeArtifactFormat::Mermaid => {
            let forbidden = [
                "http:",
                "https:",
                "file:",
                "data:",
                "javascript:",
                "@import",
                "url(",
                "<script",
                "<iframe",
                "<img",
                "<object",
                "<embed",
                "%%{",
            ];
            if source.trim_start().starts_with("---")
                || forbidden.iter().any(|needle| lower.contains(needle))
                || source
                    .lines()
                    .any(|line| line.trim_start().to_ascii_lowercase().starts_with("click "))
            {
                bail!("Mermaid external resources and directives are disabled");
            }
        }
        NativeArtifactFormat::Latex => {
            let forbidden = [
                "\\input",
                "\\include",
                "\\write",
                "\\read",
                "\\openout",
                "\\catcode",
                "\\newcommand",
                "\\def",
                "\\href",
                "\\url",
                "\\html",
                "\\class",
                "\\style",
            ];
            if forbidden.iter().any(|needle| lower.contains(needle)) {
                bail!("LaTeX I/O, links, macros, and HTML are disabled");
            }
        }
    }
    Ok(())
}

pub(super) fn managed_bin_dir(codex_home: &Path) -> PathBuf {
    codex_home.join("inline-viz").join("bin")
}

pub(super) fn ratex_binary_name() -> &'static str {
    if cfg!(windows) {
        "render-svg.exe"
    } else {
        "render-svg"
    }
}

pub(super) fn puppeteer_cache_dir() -> Option<PathBuf> {
    env::var_os("PUPPETEER_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache/puppeteer")))
}

pub(super) fn resolve_executable(command: &str, managed_dir: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return executable(path).then(|| path.to_path_buf());
    }
    let path = env::var_os("PATH");
    let home = dirs::home_dir();
    managed_dir
        .into_iter()
        .map(Path::to_path_buf)
        .chain(path.as_deref().into_iter().flat_map(env::split_paths))
        .chain(home.iter().flat_map(|home| {
            [
                home.join(".local/bin"),
                home.join(".local/share/npm/bin"),
                home.join(".cargo/bin"),
            ]
        }))
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .map(|directory| directory.join(command))
        .find(|candidate| executable(candidate))
}

fn resolve_configured(
    variable: &str,
    default: &str,
    managed_dir: Option<&Path>,
) -> Result<PathBuf> {
    let command = env::var(variable).unwrap_or_else(|_| default.to_string());
    resolve_executable(&command, managed_dir)
        .with_context(|| format!("{command} is missing; run `codex inline-viz-setup`"))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn runtime_path() -> OsString {
    env::var_os("PATH")
        .unwrap_or_else(|| OsString::from("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
}

fn copy_environment(command: &mut Command, name: &str) {
    if let Some(value) = env::var_os(name) {
        command.env(name, value);
    }
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
