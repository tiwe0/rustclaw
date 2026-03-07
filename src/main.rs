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
    let mut raw_args = env::args().skip(1).collect::<Vec<_>>();
    let mut once_image_data_url: Option<String> = None;
    let mut once_session: Option<String> = None;
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--config" | "-c" => {
                if index + 1 >= raw_args.len() {
                    return Err(anyhow::anyhow!("参数错误：--config 需要一个路径"));
                }
                let config_path = raw_args.remove(index + 1);
                raw_args.remove(index);
                config::set_config_path_override(&config_path)?;
            }
            _ if raw_args[index].starts_with("--config=") => {
                let config_path = raw_args[index]
                    .strip_prefix("--config=")
                    .unwrap_or_default()
                    .to_string();
                raw_args.remove(index);
                config::set_config_path_override(&config_path)?;
            }
            "--image-data-url" => {
                if index + 1 >= raw_args.len() {
                    return Err(anyhow::anyhow!("参数错误：--image-data-url 需要一个值"));
                }
                once_image_data_url = Some(raw_args.remove(index + 1));
                raw_args.remove(index);
            }
            _ if raw_args[index].starts_with("--image-data-url=") => {
                let value = raw_args[index]
                    .strip_prefix("--image-data-url=")
                    .unwrap_or_default()
                    .to_string();
                raw_args.remove(index);
                if value.trim().is_empty() {
                    return Err(anyhow::anyhow!("参数错误：--image-data-url 不能为空"));
                }
                once_image_data_url = Some(value);
            }
            "--session" => {
                if index + 1 >= raw_args.len() {
                    return Err(anyhow::anyhow!("参数错误：--session 需要一个值"));
                }
                once_session = Some(raw_args.remove(index + 1));
                raw_args.remove(index);
            }
            _ if raw_args[index].starts_with("--session=") => {
                let value = raw_args[index]
                    .strip_prefix("--session=")
                    .unwrap_or_default()
                    .to_string();
                raw_args.remove(index);
                if value.trim().is_empty() {
                    return Err(anyhow::anyhow!("参数错误：--session 不能为空"));
                }
                once_session = Some(value);
            }
            _ => {
                index += 1;
            }
        }
    }

    let mut args = raw_args.into_iter();
    if let Some(first) = args.next() {
        if first == "--once" {
            let prompt = args.collect::<Vec<_>>().join(" ");
            if prompt.trim().is_empty() {
                eprintln!("用法: cargo run -- --once [--session sid] [--image-data-url data_url] \"你的问题\"");
                return Ok(());
            }

            let output = if let Some(image_data_url) = once_image_data_url.as_deref() {
                if let Some(session_id) = once_session.as_deref() {
                    app::call_once_with_image_data_url_and_session(&prompt, image_data_url, Some(session_id)).await?
                } else {
                    app::call_once_with_image_data_url(&prompt, image_data_url).await?
                }
            } else if let Some(session_id) = once_session.as_deref() {
                app::call_once_with_session(&prompt, Some(session_id)).await?
            } else {
                app::call_once(&prompt).await?
            };

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
