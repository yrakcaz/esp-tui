mod agent;
mod examples;

use anyhow::Context as _;
use clap::{builder::PossibleValuesParser, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Build automation for esp-tui")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build tasks
    Build {
        #[command(subcommand)]
        target: BuildTarget,
    },
    /// Remove all build artifacts (workspace target/, examples/rust/target/,
    /// examples/c/build/)
    Clean,
}

#[derive(Subcommand)]
enum BuildTarget {
    /// Build the esp-agent static library
    Agent {
        /// Target triple to build; builds all targets when omitted
        #[arg(long, value_parser = PossibleValuesParser::new(agent::TARGETS))]
        target: Option<String>,
    },
    /// Build the C and/or Rust example projects
    Examples {
        /// Which example to build: c or rust (builds both when omitted)
        #[arg(value_parser = PossibleValuesParser::new(["c", "rust"]))]
        lang: Option<String>,
        /// Target triple to build; builds all ESP-IDF targets when omitted
        #[arg(long, value_parser = PossibleValuesParser::new(agent::TARGETS))]
        target: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            target: BuildTarget::Agent { target },
        } => agent::build(target.as_deref()),
        Command::Build {
            target: BuildTarget::Examples { lang, target },
        } => examples::build(lang.as_deref(), target.as_deref()),
        Command::Clean => clean(),
    }
}

fn clean() -> anyhow::Result<()> {
    let root = agent::workspace_root();
    for dir in [
        root.join("target"),
        root.join("examples").join("rust").join("target"),
        root.join("examples").join("c").join("build"),
    ] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
            println!("removed {}", dir.display());
        }
    }
    Ok(())
}
