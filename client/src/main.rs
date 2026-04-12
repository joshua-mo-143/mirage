mod app;
mod args;
mod backend;
mod config;
mod streaming;
mod transcript;
mod tui;

use clap::Parser;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use mirage_core::tools::cursor_session::CursorSessionStore;
use std::{error::Error, sync::Arc};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    app::App,
    args::Args,
    backend::{BackendEvent, StopServerMethod, build_backend, launch_local_server, stop_server},
    config::{ClientConfig, RemoteServerConfig, maybe_prompt_to_save_remote},
    tui::Tui,
};

/// Parses configuration, selects the active backend, and runs the Mirage TUI loop.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.local && (args.start_server || args.stop_server || args.restart_server) {
        return Err(
            "`--local` cannot be combined with `--start-server`, `--stop-server`, or `--restart-server`"
                .into(),
        );
    }
    if args.stop_server && args.restart_server {
        return Err("`--stop-server` and `--restart-server` cannot be combined".into());
    }

    let mut client_config = ClientConfig::load_or_default()?;
    if args.stop_server {
        let remote = remote_config_for_start(&args, &client_config);
        let stop_result = stop_server(&remote).await?;
        match stop_result.method {
            StopServerMethod::HttpShutdown => {
                println!(
                    "Stopped Mirage server at {} via admin shutdown.",
                    remote.server_url
                );
            }
            StopServerMethod::LocalProcessKill => {
                println!(
                    "Stopped Mirage server at {} by terminating the local process.",
                    remote.server_url
                );
            }
            StopServerMethod::NotRunning => {
                println!("No Mirage server was running at {}.", remote.server_url);
            }
        }
        return Ok(());
    }

    let launched_remote = if args.start_server || args.restart_server {
        let remote = remote_config_for_start(&args, &client_config);
        if args.restart_server {
            let stop_result = stop_server(&remote).await?;
            if stop_result.stopped {
                match stop_result.method {
                    StopServerMethod::HttpShutdown => {
                        println!(
                            "Stopped existing Mirage server at {} via admin shutdown.",
                            remote.server_url
                        );
                    }
                    StopServerMethod::LocalProcessKill => {
                        println!(
                            "Stopped existing Mirage server at {} by terminating the local process.",
                            remote.server_url
                        );
                    }
                    StopServerMethod::NotRunning => {}
                }
            }
        }
        let launch_result = launch_local_server(&remote, args.debug_stream_log.as_deref()).await?;
        if let Some(path) = maybe_prompt_to_save_remote(&mut client_config, &remote)? {
            println!("Saved Mirage remote config to {}", path.display());
        } else if launch_result.already_running {
            println!("Using existing Mirage server at {}", remote.server_url);
        }
        Some(remote)
    } else {
        None
    };

    let remote = resolve_remote_config(&args, &client_config, launched_remote)?;
    let cursor_sessions = Arc::new(CursorSessionStore::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (service, mut backend) =
        build_backend(&args, cursor_sessions.clone(), tx.clone(), remote).await?;
    let mut app = App::from_service(
        service,
        args.prompt.clone().unwrap_or_default(),
        cursor_sessions,
        backend.description(),
    );
    let mut tui = Tui::new()?;
    let mut events = EventStream::new();

    if app.can_submit() {
        app.process_enter(&mut backend);
    }

    while !app.should_quit {
        tui.draw(&mut app)?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind.is_press() => {
                        app.handle_key(key, &mut backend);
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        app.handle_mouse(mouse);
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        app.apply_remote_error(format!("terminal event error: {error}"));
                    }
                    None => break,
                }
            }
            maybe_backend = rx.recv() => {
                match maybe_backend {
                    Some(BackendEvent::Stream(event)) => app.apply_stream_event(event),
                    Some(BackendEvent::Subagent(event)) => app.apply_subagent_event(event),
                    Some(BackendEvent::RemoteSnapshot(snapshot)) => app.apply_remote_snapshot(snapshot),
                    Some(BackendEvent::RemoteError(error)) => app.apply_remote_error(error),
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// Resolves the remote server configuration from flags, saved config, or a just-launched server.
fn resolve_remote_config(
    args: &Args,
    client_config: &ClientConfig,
    launched_remote: Option<RemoteServerConfig>,
) -> Result<Option<RemoteServerConfig>, Box<dyn Error>> {
    if args.local {
        return Ok(None);
    }

    if let Some(remote) = launched_remote {
        return Ok(Some(remote));
    }

    let saved = client_config.remote.as_ref();
    let server_url = args
        .server_url
        .clone()
        .or_else(|| saved.map(|remote| remote.server_url.clone()));
    let admin_api_key = args
        .admin_key
        .clone()
        .or_else(|| saved.map(|remote| remote.admin_api_key.clone()));

    match (server_url, admin_api_key) {
        (Some(server_url), Some(admin_api_key)) => Ok(Some(RemoteServerConfig {
            server_url,
            admin_api_key,
        })),
        (None, None) => Ok(None),
        (Some(_), None) => {
            Err("Mirage server URL is configured but no admin key was provided or saved".into())
        }
        (None, Some(_)) => Err("Mirage admin key was provided without a server URL".into()),
    }
}

/// Computes the remote server configuration used for start or restart flows.
fn remote_config_for_start(args: &Args, client_config: &ClientConfig) -> RemoteServerConfig {
    let saved = client_config.remote.as_ref();
    let server_url = args
        .server_url
        .clone()
        .or_else(|| saved.map(|remote| remote.server_url.clone()))
        .unwrap_or_else(|| "http://127.0.0.1:3000".to_owned());
    let admin_api_key = args
        .admin_key
        .clone()
        .or_else(|| saved.map(|remote| remote.admin_api_key.clone()))
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    RemoteServerConfig {
        server_url,
        admin_api_key,
    }
}
