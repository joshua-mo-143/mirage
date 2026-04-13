//! Interactive Mirage terminal client and local runtime bootstrap flow.
#![warn(missing_docs)]

mod app;
mod args;
mod backend;
mod config;
mod resume;
mod skills;
mod streaming;
mod transcript;
mod tui;

use clap::Parser;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use mirage_core::tools::{
    cursor_session::CursorSessionStore,
    playwright_tool::{
        PlaywrightRuntimeStatus, ensure_managed_playwright_driver_files, playwright_browsers_dir,
        playwright_runtime_status,
    },
};
use reqwest::Url;
use std::{
    error::Error,
    io::{self, IsTerminal, Write},
    sync::Arc,
};
use tokio::{process::Command, sync::mpsc};
use uuid::Uuid;

use crate::{
    app::App,
    args::Args,
    backend::{BackendEvent, StopServerMethod, build_backend, launch_local_server, stop_server},
    config::{ClientConfig, RemoteServerConfig, maybe_prompt_to_save_remote},
    resume::{PersistedLastSession, load_last_session},
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
    let requested_resume = if args.resume_last {
        Some(load_last_session()?.ok_or("no previous Mirage TUI conversation was saved")?)
    } else {
        None
    };
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

    let remote = resolve_remote_config(
        &args,
        &client_config,
        launched_remote,
        requested_resume.as_ref(),
    )?;
    let resume_remote_session_id = match requested_resume.as_ref() {
        Some(PersistedLastSession::Remote { session_id, .. }) if remote.is_some() => {
            Some(session_id.clone())
        }
        _ => None,
    };
    if should_preflight_local_playwright(remote.as_ref()) {
        maybe_prepare_local_playwright_runtime().await?;
    }
    let cursor_sessions = Arc::new(CursorSessionStore::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (mut service, mut backend) = build_backend(
        &args,
        cursor_sessions.clone(),
        tx.clone(),
        remote,
        resume_remote_session_id,
    )
    .await?;
    let resumed_active_skill = match requested_resume.as_ref() {
        Some(PersistedLastSession::Local {
            session,
            active_skill,
        }) => {
            if !matches!(backend, crate::backend::ClientBackend::Local(_)) {
                return Err(
                    "the last saved Mirage conversation was local; restart with `--local --resume-last`"
                        .into(),
                );
            }
            service.apply_persisted_state(session.clone());
            service.session_mut().status =
                "Reattached to the last local Mirage conversation.".to_owned();
            active_skill.clone()
        }
        Some(PersistedLastSession::Remote { active_skill, .. }) => {
            if !matches!(backend, crate::backend::ClientBackend::Remote(_)) {
                return Err(
                    "the last saved Mirage conversation was remote; restart without `--local` when using `--resume-last`"
                        .into(),
                );
            }
            active_skill.clone()
        }
        None => None,
    };
    let mut app = App::from_service(
        service,
        args.prompt.clone().unwrap_or_default(),
        cursor_sessions,
        backend.description(),
    );
    if args.resume_last {
        app.set_active_skill(resumed_active_skill);
    }
    let mut tui = Tui::new()?;
    let mut events = EventStream::new();

    if app.can_submit() && app.process_enter(&mut backend) {
        persist_last_session(&backend, &app).await;
    }

    while !app.should_quit {
        tui.draw(&mut app)?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind.is_press() => {
                        if app.handle_key(key, &mut backend) {
                            persist_last_session(&backend, &app).await;
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        app.handle_mouse(mouse);
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        app.apply_remote_error(format!("terminal event error: {error}"));
                        persist_last_session(&backend, &app).await;
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
                persist_last_session(&backend, &app).await;
            }
        }
    }

    persist_last_session(&backend, &app).await;
    Ok(())
}

/// Resolves the remote server configuration from flags, saved config, or a just-launched server.
fn resolve_remote_config(
    args: &Args,
    client_config: &ClientConfig,
    launched_remote: Option<RemoteServerConfig>,
    requested_resume: Option<&PersistedLastSession>,
) -> Result<Option<RemoteServerConfig>, Box<dyn Error>> {
    if args.local {
        return Ok(None);
    }

    if let Some(remote) = launched_remote {
        return Ok(Some(remote));
    }

    if matches!(requested_resume, Some(PersistedLastSession::Local { .. }))
        && args.server_url.is_none()
        && args.admin_key.is_none()
    {
        return Ok(None);
    }

    let saved = client_config.remote.as_ref();
    let resumed_remote = requested_resume.and_then(resume_remote_config);
    let server_url = args
        .server_url
        .clone()
        .or_else(|| resumed_remote.map(|remote| remote.server_url.clone()))
        .or_else(|| saved.map(|remote| remote.server_url.clone()));
    let admin_api_key = args
        .admin_key
        .clone()
        .or_else(|| resumed_remote.map(|remote| remote.admin_api_key.clone()))
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

/// Returns the saved remote configuration embedded in a persisted resume record, if any.
fn resume_remote_config(session: &PersistedLastSession) -> Option<&RemoteServerConfig> {
    match session {
        PersistedLastSession::Remote { remote, .. } => Some(remote),
        PersistedLastSession::Local { .. } => None,
    }
}

/// Persists the current TUI session as the latest conversation Mirage can reattach to later.
async fn persist_last_session(backend: &crate::backend::ClientBackend, app: &App) {
    if let Err(error) = backend
        .persist_last_session(&app.service, app.active_skill())
        .await
    {
        eprintln!("{error}");
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

/// Returns whether the current execution target will use a local Mirage runtime for browser automation.
fn should_preflight_local_playwright(remote: Option<&RemoteServerConfig>) -> bool {
    remote.is_none() || remote.is_some_and(is_local_remote_config)
}

/// Returns whether a configured remote actually points at a local Mirage server.
fn is_local_remote_config(remote: &RemoteServerConfig) -> bool {
    Url::parse(&remote.server_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"))
}

/// Prompts for and performs the local Playwright install flow when the managed runtime is missing.
async fn maybe_prepare_local_playwright_runtime() -> Result<(), Box<dyn Error>> {
    match playwright_runtime_status().await {
        PlaywrightRuntimeStatus::Ready => Ok(()),
        PlaywrightRuntimeStatus::MissingPackage | PlaywrightRuntimeStatus::MissingBrowser => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                eprintln!(
                    "Playwright browser automation is not installed and Mirage cannot prompt in this terminal. Browser automation will remain unavailable."
                );
                return Ok(());
            }

            print!(
                "Mirage browser automation needs to install Playwright Chromium and supporting Node packages into its managed local runtime. Continue? [Y/n]: "
            );
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let normalized = answer.trim().to_ascii_lowercase();
            if !normalized.is_empty() && normalized != "y" && normalized != "yes" {
                println!("Skipping Playwright install. Browser automation will be unavailable.");
                return Ok(());
            }

            install_local_playwright_runtime().await?;
            match playwright_runtime_status().await {
                PlaywrightRuntimeStatus::Ready => {
                    println!("Playwright browser automation is ready.");
                    Ok(())
                }
                status => Err(format!(
                    "Playwright install completed but Mirage still cannot use the runtime: {}",
                    describe_playwright_runtime_status(&status)
                )
                .into()),
            }
        }
        status => {
            eprintln!(
                "Playwright browser automation is unavailable: {}",
                describe_playwright_runtime_status(&status)
            );
            Ok(())
        }
    }
}

/// Installs the managed local Playwright runtime used by Mirage.
async fn install_local_playwright_runtime() -> Result<(), Box<dyn Error>> {
    let package_dir = ensure_managed_playwright_driver_files()?;
    let browsers_dir = playwright_browsers_dir()?;
    std::fs::create_dir_all(&browsers_dir)?;

    let npm_status = Command::new("npm")
        .arg("install")
        .current_dir(&package_dir)
        .env("PLAYWRIGHT_BROWSERS_PATH", &browsers_dir)
        .status()
        .await?;
    if !npm_status.success() {
        return Err(format!("`npm install` failed with status {npm_status}").into());
    }

    let npx_status = Command::new("npx")
        .arg("playwright")
        .arg("install")
        .arg("chromium")
        .current_dir(&package_dir)
        .env("PLAYWRIGHT_BROWSERS_PATH", &browsers_dir)
        .status()
        .await?;
    if !npx_status.success() {
        return Err(
            format!("`npx playwright install chromium` failed with status {npx_status}").into(),
        );
    }

    Ok(())
}

/// Formats a human-readable explanation of the current local Playwright runtime status.
fn describe_playwright_runtime_status(status: &PlaywrightRuntimeStatus) -> String {
    match status {
        PlaywrightRuntimeStatus::Ready => "ready".to_owned(),
        PlaywrightRuntimeStatus::MissingNode => {
            "Node.js is not installed or is not on PATH".to_owned()
        }
        PlaywrightRuntimeStatus::MissingDriverEntrypoint(path) => format!(
            "the Playwright driver entrypoint is missing at {}",
            path.display()
        ),
        PlaywrightRuntimeStatus::MissingPackage => {
            "the local Playwright package is not installed".to_owned()
        }
        PlaywrightRuntimeStatus::MissingBrowser => {
            "the managed Chromium browser binary is not installed".to_owned()
        }
        PlaywrightRuntimeStatus::CheckFailed(error) => error.clone(),
    }
}
