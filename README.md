# things-mcp

English · [简体中文](README.zh-CN.md) · [Changelog](CHANGELOG.md)

`things-mcp` is an independent Things Cloud client and MCP server for macOS and Linux. It lets agents read and update your Things tasks through Things Cloud, with a local SQLite copy for listing and searching.

- Browse tasks, projects, areas, tags, and checklists. Search task titles and notes.
- Create and update tasks, organize them into projects and areas, and mark them complete.
- Create, rename, and delete tags and areas, and move tags under a different parent. Write tools support `dry_run: true` to preview changes.

## Install

For a first install on macOS or Linux, run this command.

```sh
(installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) && sh -c "$installer" install.sh)
```

At the prompt, press Enter to install to `$HOME/things-mcp`, or enter a custom directory such as `~/bin`. Noninteractive installs also default to your home directory. Set `THINGS_MCP_INSTALL_DIR` to use a custom directory without a prompt. The installer verifies the release checksum and prints the installed version and full path.

### Update

If things-mcp is already installed, run the installer with `--update`.

```sh
(installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) && sh -c "$installer" install.sh --update)
```

Press Enter at `Existing install directory` if you installed to `$HOME/things-mcp`. For a custom install, enter the directory that contains the executable. You can find it from the `command` path in your MCP client configuration.

To skip the prompt, set `THINGS_MCP_INSTALL_DIR` in the update command. This example updates `$HOME/bin/things-mcp`.

```sh
(
  installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) &&
    THINGS_MCP_INSTALL_DIR="$HOME/bin" sh -c "$installer" install.sh --update
)
```

The updater verifies the latest stable release before replacing the executable at the same path. It prints the version and path, and reports when you already have the latest build. Restart your MCP client to load the updated executable.

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

Use the absolute path printed by the installer as the MCP command and pass the `serve` argument.

```json
{
  "mcpServers": {
    "things": {
      "command": "/path/to/your/home/things-mcp",
      "args": ["serve"]
    }
  }
}
```

Restart the client and ask the agent to list your Today tasks.

## Uninstall

For the default location, run this command. If you chose a different directory, use the full path printed by the installer.

```sh
rm -f "$HOME/things-mcp"
```

The Things Cloud protocol implementation draws on [things-cloud-sdk](https://github.com/pdurlej/things-cloud-sdk).
