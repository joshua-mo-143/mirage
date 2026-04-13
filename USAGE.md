# Mirage Usage

This document covers the day-to-day ways to run and configure Mirage.

## Prerequisites

Mirage expects:

- `VENICE_API_KEY` for model access
- `node` and `npm` only if you want browser automation through Playwright

## Local TUI

Run Mirage directly in the terminal:

```bash
export VENICE_API_KEY="your-key-here"
mirage
```

You can also pass an initial prompt:

```bash
mirage "summarize this repository"
```

Useful local flags:

- `--uncensored`: request Venice's built-in uncensoring prompt
- `--max-turns <n>`: cap multi-turn tool depth
- `--resume-last`: reopen the most recent saved TUI conversation
- `--start-server`: launch a local Mirage server before opening the TUI
- `--restart-server`: restart the local Mirage server before opening the TUI
- `--stop-server`: stop the configured Mirage server and exit

## Remote Server Mode

Mirage can run as a private server and be controlled by the TUI or Telegram.

For a locally managed server, let the TUI start it for you:

```bash
export VENICE_API_KEY="your-key-here"
export MIRAGE_ADMIN_API_KEY="replace-me"
mirage --start-server
```

For a standalone server process, run the dedicated server binary:

```bash
export VENICE_API_KEY="your-key-here"
export MIRAGE_ADMIN_API_KEY="replace-me"
mirage-server
```

If you are working from a source checkout, `cargo run -p mirage-server` works too. If you only installed the `mirage` binary, prefer `--start-server` unless you also installed `mirage-server` separately.

Useful server-side environment variables:

- `MIRAGE_SERVER_BIND`: bind address, default `0.0.0.0:3000`
- `MIRAGE_ADMIN_API_KEY`: required admin key for the server API
- `MIRAGE_UNCENSORED`: server-owned uncensoring toggle
- `MIRAGE_MAX_TURNS`: default max tool-calling depth
- `MIRAGE_AUTHORITY`: override Venice API authority
- `MIRAGE_BASE_PATH`: override Venice API base path
- `VENICE_MODEL`: default model name

Connect the TUI to a server:

```bash
mirage --server-url "http://127.0.0.1:3000" --admin-key "$MIRAGE_ADMIN_API_KEY"
```

Use `--local` to force the local backend even if `MIRAGE_SERVER_URL` is already configured in the environment.

## Runtime Identity And Instructions

Mirage has a built-in identity as an autonomous assistant. You can extend that identity with runtime-owned configuration.

These settings are runtime-owned:

- users do not override them per chat or per request
- remote clients do not send their own system prompt to the server
- Mirage does not inject the full runtime configuration into the opening conversation transcript

### Additional runtime instructions

Mirage resolves extra runtime instructions in this order:

1. `MIRAGE_SYSTEM_PROMPT`
2. `VENICE_SYSTEM_PROMPT` as a compatibility fallback

Example:

```bash
export MIRAGE_SYSTEM_PROMPT="Prefer concise progress updates and finish tasks end-to-end."
mirage
```

### Personality

Mirage resolves personality in this order:

1. `MIRAGE_PERSONALITY`
2. `MIRAGE_PERSONALITY_FILE`
3. `~/.config/mirage/PERSONALITY.md` or `$XDG_CONFIG_HOME/mirage/PERSONALITY.md`

Example:

```bash
mkdir -p ~/.config/mirage
cat > ~/.config/mirage/PERSONALITY.md <<'EOF'
Dry, calm, practical, and direct.
EOF
```

## Uncensored Mode

`uncensored` controls whether Mirage asks Venice to include the provider's built-in uncensoring prompt.

Local usage:

```bash
mirage --uncensored
```

Server usage:

```bash
MIRAGE_UNCENSORED=true mirage-server
```

Notes:

- in local mode, `--uncensored` is chosen when you launch the TUI
- in remote mode, uncensoring is owned by the server runtime, not by each connected chat
- `/status` in the TUI shows `uncensored: enabled` or `uncensored: disabled`

## TUI Commands

Mirage supports these built-in slash commands in the terminal UI:

- `/help`: show command help
- `/status`: show current backend, model, uncensored status, runtime prompt/personality status, and other session details
- `/clear`: start a fresh conversation
- `/reattach`: reopen the most recent compatible saved conversation
- `/skills`: list available local skills
- `/skills <name|number>`: activate a skill for future prompts in the current TUI session
- `/skills clear`: disable skill injection
- `/quit` or `/exit`: leave the TUI

Skills are loaded from `~/.config/mirage/skills` by default, or from `MIRAGE_SKILLS_DIR` if set.

## Telegram

Mirage can also serve a private Telegram bot from the server runtime.

Relevant environment variables:

- `TELEGRAM_BOT_TOKEN`: bot token from BotFather
- `TELEGRAM_ALLOWED_CHAT_IDS`: comma-separated allowlist of chat ids
- `TELEGRAM_DEFAULT_CHAT_ID`: default chat id; if `TELEGRAM_ALLOWED_CHAT_IDS` is unset, this becomes the allowlist

Behavior:

- if `TELEGRAM_ALLOWED_CHAT_IDS` is set, only those chats are accepted
- if it is unset, Mirage falls back to `TELEGRAM_DEFAULT_CHAT_ID`
- if neither is set, Telegram chat access is effectively disabled

Supported Telegram commands:

- `/start`
- `/help`
- `/status`
- `/new`
- `/clear`

`/new` and `/clear` both start a fresh Mirage conversation for that Telegram chat.

### Getting your Telegram chat id

One simple approach:

1. Message your bot once.
2. Open `https://api.telegram.org/bot<YOUR_BOT_TOKEN>/getUpdates` in a browser.
3. Find `message.chat.id` in the returned JSON.

That value can be used for `TELEGRAM_DEFAULT_CHAT_ID` or included in `TELEGRAM_ALLOWED_CHAT_IDS`.

## Debugging

Set `MIRAGE_DEBUG_STREAM_LOG` to write raw streamed events as JSONL:

```bash
export MIRAGE_DEBUG_STREAM_LOG="$PWD/stream-debug.jsonl"
mirage
```

This is useful when investigating stalls, stream termination issues, or tool-call sequencing.
