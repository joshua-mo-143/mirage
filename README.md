# Mirage

Mirage is a powerful, uncensored AI assistant with a focus on local-first, privacy-respecting operation. It features a rich terminal-based user interface (TUI) and supports both local and remote deployment modes.

## ✨ Features

- **Claw-style agent architecture** - Flexible tool usage with built-in tools for file operations, shell commands, and subagent delegation
- **Venice backend integration** - Uncensored completions via [Venice AI](https://venice.ai)
- **Dual-mode operation** - Run entirely locally or connect to a remote server
- **Rich TUI interface** - Built with Ratatui for an intuitive terminal experience
- **Session persistence** - Save and restore conversations
- **Debug stream logging** - Optional detailed logging for troubleshooting
- **Tool discovery** - The agent automatically discovers and suggests relevant tools

## 📦 Installation

### Prerequisites
- Rust 1.70+ with cargo
- OS: Linux, macOS, or Windows (WSL2 recommended)

### Install via Cargo
```bash
cargo install mirage-client
```

### Build from Source
```bash
git clone https://github.com/yourusername/mirage.git
cd mirage
cargo build --release
```

## 🚀 Quick Start

### First Run
```bash
mirage
```

The first time you run Mirage, it will:
1. Create a default configuration
2. Prompt you to set up the Venice API key
3. Launch the TUI interface

### Setting Up Venice
Mirage uses Venice AI as its default model provider. You'll need an API key:

```bash
# Set your Venice API key
export VENICE_API_KEY="your-api-key-here"
```

Or save it in the config:
```bash
mirage --set venice.api_key=your-api-key
```

## 🎮 Usage Modes

### Local Mode (Default)
Runs the agent directly in your terminal with no external dependencies:
```bash
mirage --local
```

### Remote Server Mode
Launch a local server and connect to it:
```bash
# Start the server (runs in background)
mirage --start-server

# Connect to the server in another terminal
mirage
```

Or connect to an existing remote server:
```bash
mirage --server-url http://127.0.0.1:3000 --admin-key YOUR_ADMIN_KEY
```

## 🔧 Command-Line Options

```bash
mirage --help
```

Key options:
- `--local` - Force local mode (ignores remote config)
- `--start-server` - Launch a local server before connecting
- `--stop-server` - Stop a running local server
- `--restart-server` - Restart the local server
- `--server-url` - Specify custom server URL
- `--admin-key` - Specify admin API key for server control
- `--model` - Choose AI model (default: venice-uncensored)
- `--max-turns` - Maximum conversation turns
- `--temperature` - Creativity/consistency balance (0.1-1.0)
- `--max-completion-tokens` - Token limit per completion
- `--system-prompt` - Custom system prompt
- `--uncensored` - Enable uncensored responses (requires Venice)
- `--debug-stream-log` - Enable debug logging to file
- `--authority` - Override Venice API authority
- `--base-path` - Override Venice API base path

## 🧠 Built-in Tools

Mirage includes a powerful set of tools that the agent can use autonomously:

### 🛠️ Core Tools
- **`bash`** - Execute shell commands with full environment access
- **`read_file`** - Read file contents safely
- **`write_file`** - Create or overwrite files
- **`edit_file`** - Modify existing files
- **`subagent`** - Delegate complex tasks to a child agent
- **`prompt_cursor`** - Interact with the local Cursor agent

### 🔍 Tool Discovery
The agent follows this guidance (visible in the preamble):
- Use `bash` for arbitrary shell commands and environment inspection
- Use `read_file` to inspect files before editing
- Prefer `edit_file` for targeted modifications
- Use `write_file` only for creating new files or full replacements
- Use `subagent` for deeper investigation or planning tasks

## 📁 Configuration

Configuration is automatically loaded from:
1. Environment variables
2. `.envrc` file (if present)
3. `~/.config/mirage/client.json` (or `%APPDATA%\mirage\client.json` on Windows)

### Configuration Options
- `venice.api_key` - Venice AI API key
- `remote.server_url` - Remote server URL
- `remote.admin_api_key` - Admin key for server management
- Various client preferences

## 🔐 Security Considerations

- **Local-first**: All processing stays on your machine by default
- **Encrypted connections**: Remote mode uses HTTPS
- **API key management**: Keys are stored encrypted and never exposed in logs
- **Uncensored mode**: Available only with Venice AI; other providers may filter content

## 🐛 Troubleshooting

### Server Won't Start
```bash
# Check if another instance is running
mirage --stop-server

# Clean build artifacts
cargo clean -p mirage-server
```

### API Key Not Working
- Ensure `VENICE_API_KEY` is set correctly
- Test with: `curl https://api.venice.ai/api/v1/health -H "Authorization: Bearer YOUR_KEY"`

### Debug Logging
```bash
mirage --debug-stream-log=/tmp/mirage-debug.log
```

### Reset Configuration
Delete the config file:
```bash
rm ~/.config/mirage/client.json  # Linux/macOS
del %APPDATA%\mirage\client.json  # Windows
```

## 📄 License

Apache License 2.0. See [LICENSE](LICENSE) for details.

## 🙏 Acknowledgments

Mirage builds on top of these excellent Rust crates:
- [Rig](https://github.com/ridgeworks/rig) - Agent and tool framework
- [Ratatui](https://github.com/Rustacean-Toolkit/ratatui) - TUI library
- [Axum](https://github.com/tokio-rs/axum) - HTTP server
- [Reqwest](https://github.com/seanmonstar/reqwest) - HTTP client

## 📷 Screenshots

_Coming soon: Screenshots of the TUI interface and agent in action._

## 🏗️ Project Structure

```
mirage/
├── core/        - Core abstractions and Venice integration
├── client/      - TUI client application
├── service/     - Business logic and session management
├── server/      - HTTP server for remote mode
├── Cargo.toml   - Workspace manifest
└── README.md    - This file
```

## 🤝 Contributing

1. Fork the repository
2. Create your feature branch
3. Test thoroughly (`cargo test`)
4. Submit a pull request

We welcome contributions of all sizes, from bug fixes to new tools!

## ⚠️ Disclaimer

Mirage operates as a tool for information and assistance. Users are responsible for their use of generated content. The developers are not liable for any actions taken based on outputs from the system.
