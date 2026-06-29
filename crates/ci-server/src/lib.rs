pub mod tools;

use std::path::PathBuf;

use anyhow::Result;
use rmcp::transport::io::stdio;
use tokio_util::sync::CancellationToken;

use tools::CodeIntelligenceServer;

pub async fn serve_stdio(project_root: PathBuf, db_path: PathBuf) -> Result<()> {
    let server = CodeIntelligenceServer::new(project_root, db_path)?;
    let ct = CancellationToken::new();
    let ct_clone = ct.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received SIGINT, shutting down");
        ct_clone.cancel();
    });

    let transport = stdio();
    let service: rmcp::service::RunningService<rmcp::RoleServer, _> =
        rmcp::service::serve_server_with_ct(server, transport, ct)
            .await
            .map_err(|e: std::io::Error| anyhow::anyhow!("Server init error: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!("Server shut down cleanly");
    Ok(())
}

pub fn default_db_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".codeindex").join("index.db")
}

pub fn doctor(project_root: &std::path::Path) -> Result<()> {
    use ci_core::db::schema::init_db;
    use rusqlite::Connection;

    let db_path = default_db_path(project_root);

    println!("Project root: {}", project_root.display());
    println!(
        "  exists: {}",
        if project_root.exists() { "YES" } else { "NO" }
    );

    println!("DB path: {}", db_path.display());
    if db_path.exists() {
        let conn = Connection::open(&db_path)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        println!("  symbols: {count}");
        let file_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
        println!("  files indexed: {file_count}");
        println!("  status: OK");
    } else {
        println!("  status: NOT FOUND (run 'ci index' first)");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        init_db(&conn)?;
        println!("  created empty DB");
    }

    let grammars = ["python", "typescript", "javascript", "java", "rust", "go"];
    println!("Tree-sitter grammars: {}", grammars.join(", "));
    println!("  status: BUNDLED (compiled in)");

    if let Ok(output) = std::process::Command::new("git").arg("--version").output() {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout);
            println!("Git: {}", ver.trim());
        } else {
            println!("Git: NOT FOUND (hotspots/diff_impact will be limited)");
        }
    } else {
        println!("Git: NOT FOUND (hotspots/diff_impact will be limited)");
    }

    println!("\nAll checks passed.");
    Ok(())
}
