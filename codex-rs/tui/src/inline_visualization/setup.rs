//! Installer and diagnostics for native inline-visualization dependencies.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use sha2::Digest as _;
use sha2::Sha256;
use std::env;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::process::Command;

use super::native;

const MERMAID_CLI_VERSION: &str = "11.16.0";
const RATEX_VERSION: &str = "0.1.14";
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Eq, PartialEq)]
pub struct InlineVisualizationSetupReport {
    pub ready: bool,
    pub lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencyId {
    D2,
    Mermaid,
    Latex,
    Rasterizer,
}

#[derive(Debug)]
struct DependencyStatus {
    id: DependencyId,
    label: &'static str,
    ready: bool,
    detail: String,
}

pub async fn setup_inline_visualizations(
    codex_home: &Path,
    check_only: bool,
) -> Result<InlineVisualizationSetupReport> {
    let initial = probe_dependencies(codex_home).await;
    if check_only || initial.iter().all(|status| status.ready) {
        return Ok(report(initial));
    }

    let mut brew_packages = Vec::new();
    if is_missing(&initial, DependencyId::D2) {
        brew_packages.push("d2");
    }
    if is_missing(&initial, DependencyId::Rasterizer) {
        brew_packages.push("librsvg");
    }
    if !brew_packages.is_empty() {
        let brew = native::resolve_executable("brew", None)
            .context("Homebrew is required to install D2 and librsvg")?;
        let mut arguments = vec!["install"];
        arguments.extend(brew_packages);
        run_installer(&brew, &arguments).await?;
    }
    if is_missing(&initial, DependencyId::Mermaid) {
        let npm = native::resolve_executable("npm", None)
            .context("npm is required to install Mermaid CLI")?;
        let package = format!("@mermaid-js/mermaid-cli@{MERMAID_CLI_VERSION}");
        run_installer(&npm, &["install", "--global", &package]).await?;
    }
    if is_missing(&initial, DependencyId::Latex) {
        install_ratex(codex_home).await?;
    }

    Ok(report(probe_dependencies(codex_home).await))
}

async fn probe_dependencies(codex_home: &Path) -> Vec<DependencyStatus> {
    let managed_bin = native::managed_bin_dir(codex_home);
    let specifications = [
        (
            DependencyId::D2,
            "D2",
            configured_command("CODEX_INLINE_VIZ_D2_COMMAND", "d2"),
            None,
            "--version",
        ),
        (
            DependencyId::Mermaid,
            "Mermaid CLI",
            configured_command("CODEX_INLINE_VIZ_MMDC_COMMAND", "mmdc"),
            None,
            "--version",
        ),
        (
            DependencyId::Latex,
            "RaTeX",
            configured_command(
                "CODEX_INLINE_VIZ_RATEX_COMMAND",
                native::ratex_binary_name(),
            ),
            Some(managed_bin.as_path()),
            "--help",
        ),
        (
            DependencyId::Rasterizer,
            "SVG rasterizer",
            configured_command("CODEX_INLINE_VIZ_RSVG_COMMAND", "rsvg-convert"),
            None,
            "--version",
        ),
    ];
    let mut statuses = Vec::new();
    for (id, label, command, managed_dir, argument) in specifications {
        let Some(executable) = native::resolve_executable(&command, managed_dir) else {
            statuses.push(DependencyStatus {
                id,
                label,
                ready: false,
                detail: format!("{command} was not found"),
            });
            continue;
        };
        let probe = if id == DependencyId::Mermaid {
            probe_mermaid(&executable).await
        } else {
            probe_command(&executable, argument).await
        };
        match probe {
            Ok(output)
                if id != DependencyId::Latex
                    || output.to_ascii_lowercase().contains("embedded fonts") =>
            {
                statuses.push(DependencyStatus {
                    id,
                    label,
                    ready: true,
                    detail: format!("{} ({})", executable.display(), first_line(&output)),
                });
            }
            Ok(_) if id == DependencyId::Latex => statuses.push(DependencyStatus {
                id,
                label,
                ready: false,
                detail: "render-svg does not contain embedded fonts".to_string(),
            }),
            Ok(_) => unreachable!("non-LaTeX probe accepted above"),
            Err(error) => statuses.push(DependencyStatus {
                id,
                label,
                ready: false,
                detail: error.to_string(),
            }),
        }
    }
    statuses
}

async fn probe_mermaid(executable: &Path) -> Result<String> {
    let version = probe_command(executable, "--version").await?;
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("probe.mmd");
    let output = temporary.path().join("probe.svg");
    let puppeteer_config = temporary.path().join("puppeteer.json");
    fs::write(&source, "flowchart LR\na --> b\n")?;
    fs::write(&puppeteer_config, native::PUPPETEER_CONFIG)?;
    let mut command = Command::new(executable);
    command
        .arg("--input")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .arg("--puppeteerConfigFile")
        .arg(&puppeteer_config)
        .arg("--quiet")
        .stdin(Stdio::null());
    if let Some(cache) = native::puppeteer_cache_dir() {
        command.env("PUPPETEER_CACHE_DIR", cache);
    }
    let result = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .context("Mermaid render probe timed out")??;
    if !result.status.success() || fs::metadata(output).map_or(true, |metadata| metadata.len() == 0)
    {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let detail = first_line(&stderr);
        bail!("Mermaid render probe failed: {detail}");
    }
    Ok(version)
}

async fn probe_command(executable: &Path, argument: &str) -> Result<String> {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        Command::new(executable)
            .arg(argument)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .with_context(|| format!("{} timed out", executable.display()))??;
    if !output.status.success() {
        bail!("{} exited with {}", executable.display(), output.status);
    }
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

async fn run_installer(executable: &Path, arguments: &[&str]) -> Result<()> {
    let status = tokio::time::timeout(
        INSTALL_TIMEOUT,
        Command::new(executable)
            .args(arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status(),
    )
    .await
    .with_context(|| format!("{} timed out", executable.display()))??;
    if !status.success() {
        bail!("{} exited with {status}", executable.display());
    }
    Ok(())
}

async fn install_ratex(codex_home: &Path) -> Result<()> {
    let (target, checksum) = ratex_asset().context("RaTeX has no binary for this platform")?;
    let archive_name = format!("ratex-cli-v{RATEX_VERSION}-{target}.tar.gz");
    let url = format!(
        "https://github.com/erweixin/RaTeX/releases/download/v{RATEX_VERSION}/{archive_name}"
    );
    let temporary = tempfile::tempdir()?;
    let archive = temporary.path().join(&archive_name);
    let extracted = temporary.path().join("extracted");
    fs::create_dir(&extracted)?;

    let curl =
        native::resolve_executable("curl", None).context("curl is required to install RaTeX")?;
    let archive_arg = archive.to_string_lossy().into_owned();
    run_installer(
        &curl,
        &[
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--output",
            &archive_arg,
            &url,
        ],
    )
    .await?;
    let metadata = fs::metadata(&archive)?;
    if metadata.len() == 0 || metadata.len() > MAX_ARCHIVE_BYTES {
        bail!("RaTeX archive size is invalid");
    }
    let bytes = fs::read(&archive)?;
    if format!("{:x}", Sha256::digest(&bytes)) != checksum {
        bail!("RaTeX archive checksum mismatch");
    }

    let tar =
        native::resolve_executable("tar", None).context("tar is required to install RaTeX")?;
    let extracted_arg = extracted.to_string_lossy().into_owned();
    run_installer(&tar, &["-xf", &archive_arg, "-C", &extracted_arg]).await?;
    let source = extracted
        .join(format!("ratex-cli-v{RATEX_VERSION}-{target}"))
        .join(native::ratex_binary_name());
    let help = probe_command(&source, "--help").await?;
    if !help.to_ascii_lowercase().contains("embedded fonts") {
        bail!("downloaded render-svg does not contain embedded fonts");
    }

    let bin_dir = native::managed_bin_dir(codex_home);
    fs::create_dir_all(&bin_dir)?;
    let destination = bin_dir.join(native::ratex_binary_name());
    let mut staged = NamedTempFile::new_in(&bin_dir)?;
    staged.write_all(&fs::read(source)?)?;
    staged.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    staged.persist(&destination).map_err(|error| error.error)?;
    Ok(())
}

fn ratex_asset() -> Option<(&'static str, &'static str)> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Some((
            "aarch64-apple-darwin",
            "efe114397c9bb7664581e5ab8464ebae1e3608af427826e6962a9817902bb5f3",
        )),
        ("macos", "x86_64") => Some((
            "x86_64-apple-darwin",
            "ad94444c16a647bae8ea1d57d706aa62d813cb049b04bf3c8cdb155d3e54aa1e",
        )),
        ("linux", "aarch64") => Some((
            "aarch64-unknown-linux-musl",
            "3d13f6192f2a00253a2c6bfa611df682a65414422585b4f88d41c19826d7495e",
        )),
        ("linux", "x86_64") => Some((
            "x86_64-unknown-linux-musl",
            "bbf0db4bcc8df7a5db360713c3dc2002b95896c97bce04e7116ed3034a05af60",
        )),
        _ => None,
    }
}

fn configured_command(variable: &str, default: &str) -> String {
    env::var(variable).unwrap_or_else(|_| default.to_string())
}

fn is_missing(statuses: &[DependencyStatus], id: DependencyId) -> bool {
    statuses
        .iter()
        .any(|status| status.id == id && !status.ready)
}

fn report(statuses: Vec<DependencyStatus>) -> InlineVisualizationSetupReport {
    let ready = statuses.iter().all(|status| status.ready);
    let mut lines = vec!["Codex inline visualization setup".to_string()];
    lines.extend(statuses.into_iter().map(|status| {
        format!(
            "{:<7} {}: {}",
            if status.ready { "READY" } else { "MISSING" },
            status.label,
            status.detail
        )
    }));
    lines.push(if ready {
        "All inline visualization dependencies are ready.".to_string()
    } else {
        "One or more inline visualization dependencies are missing.".to_string()
    });
    InlineVisualizationSetupReport { ready, lines }
}

fn first_line(output: &str) -> &str {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ready")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_requires_every_dependency() {
        let report = report(vec![
            DependencyStatus {
                id: DependencyId::D2,
                label: "D2",
                ready: true,
                detail: "/bin/d2".to_string(),
            },
            DependencyStatus {
                id: DependencyId::Rasterizer,
                label: "SVG rasterizer",
                ready: false,
                detail: "rsvg-convert was not found".to_string(),
            },
        ]);

        assert!(!report.ready);
        assert!(report.lines.iter().any(|line| line.starts_with("READY")));
        assert!(report.lines.iter().any(|line| line.starts_with("MISSING")));
    }
}
