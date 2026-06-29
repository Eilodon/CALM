use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ci",
    about = "Code Intelligence — MCP server for codebase analysis"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server over stdio
    Serve {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        /// Database file path
        #[arg(long)]
        db_path: Option<PathBuf>,
    },
    /// One-shot index of the project (stub)
    Index {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
    /// Validate config, DB, tree-sitter, git
    Doctor {
        /// Project root directory
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            project_root,
            db_path,
        } => {
            let root = std::fs::canonicalize(&project_root)?;
            let db = db_path.unwrap_or_else(|| ci_server::default_db_path(&root));
            tracing::info!("Starting MCP server for {}", root.display());
            ci_server::serve_stdio(root, db).await?;
        }
        Commands::Index { project_root } => {
            let root = std::fs::canonicalize(&project_root)?;
            tracing::info!("Indexing {} (stub — not yet implemented)", root.display());
            println!("Index command not yet implemented. Use 'ci serve' to start the MCP server.");
        }
        Commands::Doctor { project_root } => {
            let root = std::fs::canonicalize(&project_root)?;
            ci_server::doctor(&root)?;
        }
    }

    Ok(())
}
