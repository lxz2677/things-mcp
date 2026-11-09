# things-mcp

[English](README.md) · 简体中文 · [更新日志](CHANGELOG.md)

`things-mcp` 是一个独立的 Things Cloud 客户端和 MCP 服务，支持 macOS 和 Linux。它让 Agent 通过 Things Cloud 读取和修改 Things 任务，并把数据同步到本地 SQLite，供列表查询和搜索使用。

- 浏览任务、项目、区域、标签和清单，搜索任务标题和备注。
- 创建和更新任务，将任务归入项目和区域，或标记完成。
- 为有开始日期的任务和项目设置或清除提醒，时间使用本地 `HH:MM` 格式。
- 创建、重命名和删除标签与区域，也可以调整标签的父级。写入工具支持用 `dry_run: true` 预览变更。

## 安装

首次在 macOS 或 Linux 上安装时，运行下面的命令。

```sh
(installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) && sh -c "$installer" install.sh)
```

看到目录提示时，直接回车会安装到 `$HOME/things-mcp`。也可以输入自定义目录，例如 `~/bin`。非交互运行时同样默认安装到主目录，可以用 `THINGS_MCP_INSTALL_DIR` 指定其他目录并跳过提示。安装器会校验预编译包，并输出版本号和程序的绝对路径。

### 更新

已经安装过 things-mcp 时，用 `--update` 更新到最新稳定版。

```sh
(installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) && sh -c "$installer" install.sh --update)
```

看到 `Existing install directory` 时，如果原来装在 `$HOME/things-mcp`，直接回车。如果装在其他位置，输入程序所在的目录。可以从 MCP 客户端配置的 `command` 路径找到它。

也可以在命令里设置 `THINGS_MCP_INSTALL_DIR` 跳过提示。下面的例子会更新 `$HOME/bin/things-mcp`。

```sh
(
  installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) &&
    THINGS_MCP_INSTALL_DIR="$HOME/bin" sh -c "$installer" install.sh --update
)
```

更新器校验通过后才会在原路径替换程序，并输出版本号和路径。已是最新版本时会提示 `Already up to date`。更新后重启 MCP 客户端即可加载新版。

### 让 Agent 安装

> 请按 [https://github.com/lxz2677/things-mcp](https://github.com/lxz2677/things-mcp) 的一行安装命令安装 things-mcp，用输出的程序路径和 `serve` 参数把它添加到我的 MCP 客户端，再告诉我如何在本机填写 Things Cloud 凭据。

## 连接账号

运行 `mkdir -p ~/.things-mcp`，然后把 Things Cloud 的邮箱和密码保存到 `~/.things-mcp/credentials.json`。

```json
{
  "email": "you@example.com",
  "password": "your-things-cloud-password"
}
```

凭据以明文保存。用 `chmod 600 ~/.things-mcp/credentials.json` 限制文件权限。也可以成对提供 `THINGS_EMAIL` 和 `THINGS_PASSWORD` 环境变量。

### 添加 MCP 服务

在 MCP 客户端里，把 `command` 设为安装器输出的绝对路径，参数填 `serve`。

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

重启客户端，请 Agent 列出你的 Today 任务。

设置提醒时，任务或项目需要有开始日期。例如，运行 `things-mcp create "打电话" --when tomorrow --remind 09:00`，或在 MCP 的 `create_task` 工具中传入 `when: "tomorrow"` 和 `reminder: "09:00"`。要清除提醒，用 CLI 的 `update --clear-remind`，或在 `update_task` 中传入 `clear_reminder: true`。

## 兼容性

每次 sync 都会把云端的版本标记和本构建支持的范围比对：

- `schema` / `latest-schema-version` 必须是 `301`
- 实体代际不超过 `Task7`、`Area3`、`Tag4`、`ChecklistItem3`、`Settings5`、`Tombstone2`
- 未知实体类型、未知动作 `t` 一律视为不兼容

一旦发现更新：sync 立即停止并把状态写入本地；之后 CLI 和 MCP 的所有数据操作都会返回 `INCOMPATIBLE_VERSION` 警告并终止，不读也不写副本。`sync`、`verify`、`doctor`、`account` 仍可运行，方便升级后重新检测。`verify`、`doctor`、`sync_status` 会显示上次检测结果。

检测发生在 sync 时；写操作本来就会先 sync，所以会在 commit 前被拦住。

## 卸载

默认安装位置用下面这条命令卸载。如果安装时选了其他目录，把命令里的路径换成安装器输出的绝对路径。

```sh
rm -f "$HOME/things-mcp"
```

Things Cloud 协议实现参考了 [things-cloud-sdk](https://github.com/pdurlej/things-cloud-sdk)。
