//! Entry point (port of index.ts). Load config, resolve secrets, build store + manager, serve the API.
mod agent;
mod api;
mod config;
mod dispatcher;
mod library;
mod llm;
mod manager;
mod mc;
mod recipes;
mod routines;
mod rules;
mod secrets;
mod skill;
mod skills;
mod store;
mod types;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut config = config::load_config()?;
    let (anthropic, openai) =
        tokio::join!(secrets::resolve_api_key(), secrets::resolve_openai_key());
    config.llm.api_key = anthropic?;
    config.llm.openai_api_key = openai?;

    let store = Arc::new(store::Store::new(&config.db_path)?);
    let manager = Arc::new(manager::BotManager::new(config.clone(), store));
    manager.start_all();

    let port = config.port;
    let app = api::create_api(manager.clone());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(
        "minecraft-agents listening on :{port} — dispatcher \"{}\" -> {}:{}",
        config.dispatcher_name,
        config.mc.host,
        config.mc.port
    );
    axum::serve(listener, app).await?;
    Ok(())
}
