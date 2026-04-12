pub mod tools;

mod app;
mod args;
mod streaming;
mod transcript;
mod tui;

use clap::Parser;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use mirage_core::{VeniceClient, VeniceConfig, session::StreamEvent};
use std::{error::Error, sync::Arc};
use tokio::sync::mpsc;

use crate::{
    app::App,
    args::Args,
    tools::{
        bash_tool::BashTool,
        cursor_session::CursorSessionStore,
        file_tools::{EditFileTool, ReadFileTool, WriteFileTool},
        prompt_cursor_tool::PromptCursorTool,
        subagent_tool::SubagentTool,
    },
    tui::Tui,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let config = VeniceConfig::from_env()?
        .with_authority(args.authority.clone())
        .with_base_path(args.base_path.clone());
    let client = VeniceClient::new(config)?;
    let mut agent_builder = client
        .agent(args.model.clone())
        .default_max_turns(args.max_turns);

    agent_builder = agent_builder.additional_params(serde_json::json!({
        "venice_parameters": {
            "include_venice_system_prompt": args.uncensored
        }
    }));

    if let Some(system_prompt) = args.system_prompt.as_deref() {
        agent_builder = agent_builder.preamble(system_prompt);
    }

    agent_builder = agent_builder.append_preamble(
        "Tool usage guidance:
- Prefer discovering capabilities by using `bash` instead of assuming what commands, binaries, files, or directories are available.
- Use `bash` freely for arbitrary shell commands, environment inspection, and capability discovery.
- Use `subagent` when you want to delegate a deeper investigation or planning task to a child Cursor agent and incorporate its final answer.
- Use `read_file` to inspect files before editing them when needed.
- Prefer `edit_file` for modifying part of an existing file.
- Use `write_file` only when creating a new file, replacing an entire file, or appending whole-file content intentionally.
- Use `prompt_cursor` when you want the local Cursor agent CLI (`agent -p`) to answer or inspect something.",
    );

    if let Some(temperature) = args.temperature {
        agent_builder = agent_builder.temperature(f64::from(temperature));
    }

    if let Some(max_tokens) = args.max_completion_tokens {
        agent_builder = agent_builder.max_tokens(u64::from(max_tokens));
    }

    let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
    let cursor_sessions = Arc::new(CursorSessionStore::default());
    let agent = agent_builder
        .tool(BashTool)
        .tool(PromptCursorTool::new(cursor_sessions.clone()))
        .tool(SubagentTool::new(subagent_tx, cursor_sessions.clone()))
        .tool(ReadFileTool)
        .tool(EditFileTool)
        .tool(WriteFileTool)
        .build();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(&args, cursor_sessions);
    let mut tui = Tui::new()?;
    let mut events = EventStream::new();

    if app.can_submit() {
        app.process_enter(agent.clone(), tx.clone());
    }

    while !app.should_quit {
        tui.draw(&mut app)?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind.is_press() => {
                        app.handle_key(key, agent.clone(), tx.clone());
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        app.handle_mouse(mouse);
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        app.apply_stream_event(StreamEvent::Error(format!("terminal event error: {error}")));
                    }
                    None => break,
                }
            }
            maybe_stream = rx.recv() => {
                if let Some(event) = maybe_stream {
                    app.apply_stream_event(event);
                } else {
                    break;
                }
            }
            maybe_subagent = subagent_rx.recv() => {
                if let Some(event) = maybe_subagent {
                    app.apply_subagent_event(event);
                }
            }
        }
    }

    Ok(())
}
