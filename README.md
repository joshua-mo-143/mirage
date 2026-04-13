# Mirage

Mirage is a privacy-first terminal agent with a rich TUI, a private remote server mode, and a small set of built-in tools for coding, shell work, browser automation, and delegated subagents.

By default, Mirage uses [Venice AI](https://venice.ai) for inference. Venice hosts a range of private models as well as anonymised models, applying ZDR policies across the platform so that no conversation history is stored and total privacy is ensured.

> [!WARNING]
> This repository is extremely unstable. Use at your own risk. Additionally, the code contained in this repository is generated primarily with LLMs.

## Features
- Run locally or as a server deployment
- Talk to Mirage from the terminal (through TUI) or Telegram
- Sub-agent support
- Cron job support
- Skills support
- First class support for coding harneesses like Cursor
- First class support for using Playwright/Puppeteer
- Uncensoring support (through Venice AI)
- Zero user telemetry tracking

## Architecture

Mirage is split into a few focused crates:

- `core`: shared session logic, tool implementations, Venice integration, skills, and browser runtime helpers
- `service`: orchestration layer over the session reducer
- `server`: private Axum server with SSE, scheduling, and Telegram integration
- `client`: terminal UI, local backend, remote backend, skill selection, and resume flows

## Installation
To install Mirage, you'll need to install it from source:

```
cargo install --git https://www.github.com/joshua-mo-143/mirage.git
```

## Quick Start

### Prerequisites

- Rust + Cargo
- A `VENICE_API_KEY`
- `node` and `npm` only if you want Playwright browser automation

### Basic usage

```bash
export VENICE_API_KEY="your-key-here"
cargo run 
```

For day-to-day operation, remote mode, runtime identity/personality, uncensoring, TUI commands, and Telegram setup, see [`USAGE.md`](./USAGE.md).

## Configuration

Mirage has a built-in identity as an autonomous assistant. You can optionally give it a runtime personality of its own.

Personality is resolved in this order:

1. `MIRAGE_PERSONALITY`
2. `MIRAGE_PERSONALITY_FILE`
3. `~/.config/mirage/PERSONALITY.md` or `$XDG_CONFIG_HOME/mirage/PERSONALITY.md`

Mirage's own runtime instructions are resolved from `MIRAGE_SYSTEM_PROMPT`, with `VENICE_SYSTEM_PROMPT` still accepted as a compatibility fallback. They are runtime-owned configuration, not something passed or overridden per request.

## Why?
Mirage is my attempt at an autonomous assistant that can live inside a real technical workflow instead of sitting beside it.

The core idea is simple:

- Mirage is for people who want an assistant that works directly in the environment they already use.
- Existing options often require too much setup before they become useful, or they do not compose well with tools like Cursor.
- Mirage tries to solve that by being local-first, lightweight, and able to delegate work efficiently instead of forcing everything through one context window.

In practice, that means Mirage can interact with your current working directory, hand off coding tasks to Cursor or another harness when that is the better fit, and also take on adjacent workflow tasks like browser automation, scheduled jobs, and Telegram reporting.

I also ran into teething issues with similar tools, especially around long conversations: flickering UIs, hanging terminals, and sluggish session handling. Building Mirage in Rust is largely an attempt to make that experience feel more stable and responsive.

## Who is Mirage for?
Mirage is for people who want an autonomous assistant embedded in their technical workflow with very little setup required. It is especially useful for software engineers, since coding harnesses like Cursor are treated as first-class tools rather than as an afterthought.

## License
MIT
