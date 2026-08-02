use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use codex_inline_viz_renderer::MAX_SOURCE_BYTES;
use codex_inline_viz_renderer::RenderFormat;

const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024;

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Render one visualization read from standard input.
    Render {
        #[arg(long, value_enum)]
        format: RenderFormat,

        #[arg(long)]
        output: PathBuf,
    },
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("inline visualization renderer failed unexpectedly");
    }));

    match std::panic::catch_unwind(run) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            print_bounded_error(&format!("{error:#}"));
            ExitCode::FAILURE
        }
        Err(_) => ExitCode::FAILURE,
    }
}

fn run() -> anyhow::Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Command::Render { format, output } => {
            let mut source = Vec::new();
            std::io::stdin()
                .take((MAX_SOURCE_BYTES + 1) as u64)
                .read_to_end(&mut source)?;
            if source.len() > MAX_SOURCE_BYTES {
                anyhow::bail!("source exceeds the {MAX_SOURCE_BYTES}-byte limit");
            }
            let source = std::str::from_utf8(&source)?;
            codex_inline_viz_renderer::render_to_path(source, format, &output)?;
        }
    }
    Ok(())
}

fn print_bounded_error(message: &str) {
    let mut end = message.len().min(MAX_DIAGNOSTIC_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    eprintln!("{}", &message[..end]);
}
