# things-mcp

English · [简体中文](README.zh-CN.md)

`things-mcp` is an independent Things Cloud client and MCP server for macOS and Linux. It lets agents read and update your Things tasks through Things Cloud, with a local SQLite copy for listing and searching.

- Browse tasks, projects, areas, tags, and checklists. Search task titles and notes.
- Create and update tasks, organize them into projects and areas, and mark them complete. Write tools support `dry_run: true` to preview changes.

## Install

Run this command on macOS or Linux.

```sh
installer=$(curl -fsSL https://github.com/lxz2677/things-mcp/releases/latest/download/install.sh) && sh -c "$installer"
```

The installer selects the matching release binary, verifies its SHA-256 checksum, and installs it in `~/.local/bin`.

### Install with an agent

> Install things-mcp using the one-line installer in [https://github.com/lxz2677/things-mcp](https://github.com/lxz2677/things-mcp), add it to my MCP client using the printed binary path and the `serve` argument, and show me how to enter my Things Cloud credentials locally.

## Connect your account

Run `mkdir -p ~/.things-mcp`, then save your Things Cloud email and password in `~/.things-mcp/credentials.json`.

```json
{
  "email": "you@example.com",
  "password": "your-things-cloud-password"
}
```

The file stores plaintext credentials. Restrict access with `chmod 600 ~/.things-mcp/credentials.json`. You can also supply `THINGS_EMAIL` and `THINGS_PASSWORD` together as environment variables.

### Add the MCP server

Set your MCP client to launch the installed binary over stdio. Replace the command with the full path on your machine.

```json
{
  "mcpServers": {
    "things": {
      "command": "/path/to/your/home/.local/bin/things-mcp",
      "args": ["serve"]
    }
  }
}
```

Restart the client and ask the agent to list your Today tasks.

The Things Cloud protocol implementation draws on [things-cloud-sdk](https://github.com/pdurlej/things-cloud-sdk).
