use anyhow::Result;
use std::env;
use std::path::PathBuf;

mod app;
mod channel;
mod conversation;
mod config;
mod cron;
mod interrupt;
mod log;
mod memory;
mod model;
mod react_agent;
mod session;
mod skills;
mod tools;
mod types;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    if let Some(first) = args.next() {
        if first == "--once" {
            let prompt = args.collect::<Vec<_>>().join(" ");
            if prompt.trim().is_empty() {
                eprintln!("用法: cargo run -- --once \"你的问题\"");
                return Ok(());
            }
            let output = app::call_once(&prompt).await?;
            println!("{}", output);
            return Ok(());
        }

        if first == "--channel" {
            if let Some(provider) = args.next() {
                let mode = conversation::ConversationMode::parse(&provider)
                    .ok_or_else(|| anyhow::anyhow!("不支持的 channel: {}", provider))?;
                conversation::run_mode(mode).await?;
            } else {
                conversation::run_configured_channel().await?;
            }
            return Ok(());
        }

        if first == "--conversation" {
            let mode_raw = args.next().unwrap_or_else(|| "tui".to_string());
            let mode = conversation::ConversationMode::parse(&mode_raw)
                .ok_or_else(|| anyhow::anyhow!("不支持的 conversation: {}", mode_raw))?;
            conversation::run_mode(mode).await?;
            return Ok(());
        }

        if first == "--cron" {
            cron::run().await?;
            return Ok(());
        }

        if first == "--session-export" {
            let workspace_root = env::current_dir()?;
            let config_path = config::resolve_config_path();
            let cfg = config::load_config(&config_path)?;
            let app_base_dir = config::resolve_app_base_dir(&workspace_root, &cfg.base);
            log::init(&cfg.log, &app_base_dir)?;
            let output = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| app_base_dir.join(".sessions").join("sessions_export.json"));

            log::info("loaded directories:");
            log::info(format!("  - base_dir: {}", app_base_dir.display()));
            log::info(format!(
                "  - session.dir: {}",
                session::session_dir_path(&app_base_dir).display()
            ));
            log::info(format!(
                "  - session.db: {}",
                session::session_db_path(&app_base_dir).display()
            ));
            log::info(format!("  - session.export: {}", output.display()));
            if cfg.log.file_enabled {
                log::info(format!(
                    "  - log.file: {}",
                    app_base_dir.join(&cfg.log.file_name).display()
                ));
            } else {
                log::info("  - log.file: <disabled>");
            }

            let manager = session::SessionManager::new(&app_base_dir)?;
            let count = manager.export_all_to_json_file(&output)?;
            println!(
                "session export done: count={} file={}",
                count,
                output.display()
            );
            return Ok(());
        }
    }

    conversation::run_mode(conversation::ConversationMode::Tui).await
}
