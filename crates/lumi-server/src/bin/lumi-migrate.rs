//! Forward-only PostgreSQL migration command for Lumi deploys.

use anyhow::Context;
use lumi_server::{run_migrations, AppConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env();
    run_migrations(config.database_url())
        .await
        .context("failed to apply Lumi PostgreSQL migrations")
}
