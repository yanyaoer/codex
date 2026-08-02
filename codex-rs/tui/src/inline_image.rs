//! Kitty Unicode-placeholder transport for transcript-anchored artifact images.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

pub(crate) const MARKDOWN_LANGUAGE: &str = "codex-inline-image";

const ESC: &str = "\x1b";
const ST: &str = "\x1b\\";
const KITTY_CHUNK_SIZE: usize = 4096;
const KITTY_PLACEHOLDER: char = '\u{10eeee}';
const MAX_INLINE_COLUMNS: u16 = 100;
const MAX_INLINE_ROWS: u16 = 18;
const CELL_ASPECT_CORRECTION: f64 = 0.52;

static DIACRITICS: LazyLock<Vec<char>> = LazyLock::new(|| {
    let diacritics = "
        0305 030D 030E 0310 0312 033D 033E 033F 0346 034A 034B 034C 0350 0351 0352 0357
        035B 0363 0364 0365 0366 0367 0368 0369 036A 036B 036C 036D 036E 036F 0483 0484
        0485 0486 0487 0592 0593 0594 0595 0597 0598 0599 059C 059D 059E 059F 05A0 05A1
        05A8 05A9 05AB 05AC 05AF 05C4 0610 0611 0612 0613 0614 0615 0616 0617 0657 0658
        0659 065A 065B 065D 065E 06D6 06D7 06D8 06D9 06DA 06DB 06DC 06DF 06E0 06E1 06E2
        06E4 06E7 06E8 06EB 06EC 0730 0732 0733 0735 0736 073A 073D 073F 0740 0741 0743
        0745 0747 0749 074A 07EB 07EC 07ED 07EE 07EF 07F0 07F1 07F3 0816 0817 0818 0819
        081B 081C 081D 081E 081F 0820 0821 0822 0823 0825 0826 0827 0829 082A 082B 082C
        082D 0951 0953 0954 0F82 0F83 0F86 0F87 135D 135E 135F 17DD 193A 1A17 1A75 1A76
        1A77 1A78 1A79 1A7A 1A7B 1A7C 1B6B 1B6D 1B6E 1B6F 1B70 1B71 1B72 1B73 1CD0 1CD1
        1CD2 1CDA 1CDB 1CE0 1DC0 1DC1 1DC3 1DC4 1DC5 1DC6 1DC7 1DC8 1DC9 1DCB 1DCC 1DD1
        1DD2 1DD3 1DD4 1DD5 1DD6 1DD7 1DD8 1DD9 1DDA 1DDB 1DDC 1DDD 1DDE 1DDF 1DE0 1DE1
        1DE2 1DE3 1DE4 1DE5 1DE6 1DFE 20D0 20D1 20D4 20D5 20D6 20D7 20DB 20DC 20E1 20E7
        20E9 20F0 2CEF 2CF0 2CF1 2DE0 2DE1 2DE2 2DE3 2DE4 2DE5 2DE6 2DE7 2DE8 2DE9 2DEA
        2DEB 2DEC 2DED 2DEE 2DEF 2DF0 2DF1 2DF2 2DF3 2DF4 2DF5 2DF6 2DF7 2DF8 2DF9 2DFA
        2DFB 2DFC 2DFD 2DFE 2DFF A66F A67C A67D A6F0 A6F1 A8E0 A8E1 A8E2 A8E3 A8E4 A8E5
    "
    .split_whitespace()
    .filter_map(|value| u32::from_str_radix(value, 16).ok().and_then(char::from_u32))
    .collect::<Vec<_>>();
    assert_eq!(diacritics.len(), 256, "invalid Kitty placeholder table");
    diacritics
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KittyTransport {
    Direct,
    TmuxPassthrough { depth: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InlineImage {
    path: PathBuf,
    image_id: u32,
    columns: u16,
    rows: u16,
    transport: KittyTransport,
}

impl InlineImage {
    pub(crate) fn new(
        path: PathBuf,
        digest: &[u8],
        width_px: u32,
        height_px: u32,
        available_width: usize,
        transport: KittyTransport,
    ) -> Option<Self> {
        let available_columns = u16::try_from(available_width)
            .unwrap_or(u16::MAX)
            .min(MAX_INLINE_COLUMNS);
        if available_columns == 0 || width_px == 0 || height_px == 0 {
            return None;
        }

        let aspect = f64::from(height_px) / f64::from(width_px) * CELL_ASPECT_CORRECTION;
        let rows = (f64::from(available_columns) * aspect)
            .round()
            .max(1.0)
            .min(f64::from(MAX_INLINE_ROWS)) as u16;
        let columns = (f64::from(rows) / aspect)
            .round()
            .max(1.0)
            .min(f64::from(available_columns)) as u16;

        Self::new_with_grid(path, digest, columns, rows, transport)
    }

    pub(crate) fn new_with_grid(
        path: PathBuf,
        digest: &[u8],
        columns: u16,
        rows: u16,
        transport: KittyTransport,
    ) -> Option<Self> {
        if columns == 0
            || rows == 0
            || usize::from(columns) > DIACRITICS.len()
            || usize::from(rows) > DIACRITICS.len()
        {
            return None;
        }

        let id_bytes: [u8; 4] = digest.get(..4)?.try_into().ok()?;
        let image_id = u32::from_be_bytes(id_bytes).max(1);
        Some(Self {
            path,
            image_id,
            columns,
            rows,
            transport,
        })
    }

    // Kitty's Unicode placeholder protocol reserves the foreground RGB bytes for the image ID.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn placeholder_lines(&self) -> Vec<Line<'static>> {
        let image_id = self.image_id.to_be_bytes();
        let color = Color::Rgb(image_id[1], image_id[2], image_id[3]);
        let high_byte = DIACRITICS[usize::from(image_id[0])];

        (0..self.rows)
            .map(|row| {
                let mut placeholders = String::new();
                for column in 0..self.columns {
                    placeholders.push(KITTY_PLACEHOLDER);
                    placeholders.push(DIACRITICS[usize::from(row)]);
                    placeholders.push(DIACRITICS[usize::from(column)]);
                    placeholders.push(high_byte);
                }
                Line::from(Span::styled(placeholders, Style::new().fg(color)))
            })
            .collect()
    }

    pub(crate) fn write_transmission(&self, writer: &mut impl Write) -> std::io::Result<()> {
        let png = fs::read(&self.path)?;
        let payload = general_purpose::STANDARD.encode(png);
        let chunks = payload.as_bytes().chunks(KITTY_CHUNK_SIZE);
        let chunk_count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let chunk = std::str::from_utf8(chunk).map_err(std::io::Error::other)?;
            let has_more = usize::from(index + 1 < chunk_count);
            let command = if index == 0 {
                format!(
                    "{ESC}_Ga=T,U=1,t=d,f=100,i={},c={},r={},q=2,C=1,m={has_more};{chunk}{ST}",
                    self.image_id, self.columns, self.rows
                )
            } else {
                format!("{ESC}_Gm={has_more};{chunk}{ST}")
            };
            match self.transport {
                KittyTransport::Direct => writer.write_all(command.as_bytes())?,
                KittyTransport::TmuxPassthrough { depth } => {
                    writer.write_all(wrap_tmux_command(&command, depth).as_bytes())?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn detect_kitty_transport() -> Option<KittyTransport> {
    match active_tmux_client() {
        None => (nested_kitty_terminal_hint()
            || kitty_compatible_terminal_name(env::var("TERM_PROGRAM").ok().as_deref())
            || kitty_compatible_terminal_name(env::var("TERM").ok().as_deref()))
        .then_some(KittyTransport::Direct),
        Some(client_termname) => {
            if !tmux_allows_passthrough() {
                return None;
            }
            tmux_passthrough_depth(Some(&client_termname), nested_kitty_terminal_hint())
                .map(|depth| KittyTransport::TmuxPassthrough { depth })
        }
    }
}

fn active_tmux_client() -> Option<String> {
    let pane = env::var("TMUX_PANE").ok().filter(|pane| !pane.is_empty())?;
    env::var_os("TMUX")?;

    let value = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{pane_tty}\t#{client_termname}",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    let Some(value) = value else {
        return active_tmux_fallback();
    };
    let mut fields = value.trim_end().splitn(2, '\t');
    let Some(pane_tty) = fields.next().filter(|value| !value.is_empty()) else {
        return active_tmux_fallback();
    };
    let pane_matches = tty_paths_match(Path::new("/dev/fd/1"), Path::new(pane_tty));
    if pane_matches == Some(false) {
        return None;
    }
    if pane_matches.is_none() && !term_looks_like_tmux() {
        return None;
    }
    Some(fields.next().map(str::trim).unwrap_or_default().to_string())
}

fn active_tmux_fallback() -> Option<String> {
    term_looks_like_tmux().then(String::new)
}

fn term_looks_like_tmux() -> bool {
    env::var("TERM")
        .ok()
        .is_some_and(|term| term.contains("tmux") || term.contains("screen"))
}

#[cfg(unix)]
fn tty_paths_match(current: &Path, pane: &Path) -> Option<bool> {
    use std::os::unix::fs::FileTypeExt as _;
    use std::os::unix::fs::MetadataExt as _;

    let current = fs::metadata(current).ok()?;
    let pane = fs::metadata(pane).ok()?;
    (current.file_type().is_char_device() && pane.file_type().is_char_device())
        .then(|| current.rdev() == pane.rdev())
}

#[cfg(not(unix))]
fn tty_paths_match(_current: &Path, _pane: &Path) -> Option<bool> {
    None
}

fn tmux_allows_passthrough() -> bool {
    std::process::Command::new("tmux")
        .args(["show-options", "-gv", "allow-passthrough"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|value| matches!(value.trim(), "on" | "all"))
}

fn tmux_passthrough_depth(
    client_termname: Option<&str>,
    nested_kitty_terminal: bool,
) -> Option<u8> {
    if kitty_compatible_terminal_name(client_termname) {
        return Some(1);
    }
    let nested_tmux_client = terminal_field_contains(client_termname, "tmux")
        || terminal_field_contains(client_termname, "screen");
    (nested_tmux_client && nested_kitty_terminal).then_some(2)
}

fn nested_kitty_terminal_hint() -> bool {
    env::var_os("KITTY_WINDOW_ID").is_some() || env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
}

fn kitty_compatible_terminal_name(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "kitty" | "ghostty" | "xterm-kitty" | "xterm-ghostty"
        )
    })
}

fn terminal_field_contains(value: Option<&str>, needle: &str) -> bool {
    value.is_some_and(|value| value.to_ascii_lowercase().contains(needle))
}

fn wrap_tmux_command(command: &str, depth: u8) -> String {
    let mut wrapped = command.to_string();
    for _ in 0..depth {
        let escaped = wrapped.replace(ESC, "\x1b\x1b");
        wrapped = format!("{ESC}Ptmux;{escaped}{ST}");
    }
    wrapped
}

#[cfg(test)]
#[path = "inline_image_tests.rs"]
mod tests;
