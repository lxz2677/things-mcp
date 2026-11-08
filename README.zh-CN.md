# things-mcp

[English](README.md) · 简体中文

`things-mcp` 是一个独立的 Things Cloud 客户端和 MCP 服务，支持 macOS 和 Linux。它让 Agent 通过 Things Cloud 读取和修改 Things 任务，并把数据同步到本地 SQLite，供列表查询和搜索使用。

- 浏览任务、项目、区域、标签和清单，搜索任务标题和备注。
- 创建和更新任务，将任务归入项目和区域，或标记完成。写入工具支持用 `dry_run: true` 预览变更。

## 安装

在 macOS 或 Linux 上运行这一行。

```sh
(installer=$(curl -fsSL https://raw.githubusercontent.com/lxz2677/things-mcp/main/scripts/install.sh) && eval "$installer")
```

安装时可以输入目录，直接回车就使用 Home。无交互终端时也默认用 Home，自动安装可以通过 `THINGS_MCP_INSTALL_DIR` 指定目录。安装器会校验预编译包，并输出 `things-mcp` 的绝对路径。

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

## 卸载

默认安装位置用下面这条命令卸载。如果安装时选了其他目录，把命令里的路径换成安装器输出的绝对路径。

```sh
rm -f "$HOME/things-mcp"
```

Things Cloud 协议实现参考了 [things-cloud-sdk](https://github.com/pdurlej/things-cloud-sdk)。
