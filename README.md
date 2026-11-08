# things-mcp

一个 **Rust 写的 Things Cloud 无头客户端**，最终以 MCP Server 形态给 Agent 用。

当前进度：**Phase 0+1（只读）已验收 + Phase 2 写路径已实现**（34 键金标准编码、安全闸、`POST /commit`、dry-run、MCP 29 个工具）。写操作的真实账号端到端验收待跑。

设计依据：本地的 6 份方案文档 + `things4Android-share-20261108/reverse-engineering/` 的抓包实证（这些材料未随仓库发布）。实现逻辑参照 [pdurlej/things-cloud-sdk](https://github.com/pdurlej/things-cloud-sdk)，但把它的 `Task6` 协议升级到当前云端在用的 `Task7`。

---

## 为什么是重写而不是直接用那个 Go SDK

它的 `ItemKind` 只认到 `Task6`（`types.go`），而当前 Things Cloud 下发的是 `Task7`：

- 内存态：`Task7` 落到 `default: // Unsupported kind: skip` → 所有任务被静默丢弃
- 写入：还硬编码 `"Task6"`
- 表现：登录成功、同步不报错、列表全空

所以这里保留了它的**分层与语义**（client / state / list 规则 / 工具面），只把协议常量、实体版本、金标准字段集换成 2026-10 抓包实测的版本。

---

## 关键协议事实（与上游文档的差异已修正）

| 项 | 结论 |
|---|---|
| 游标 | `current-item-index` 是**服务端 head**，不是本页末尾；游标按 `start_index + len(items 数组)` 推进，循环到 head 为止 |
| 越界 | `start-index > head` 会返回 **HTTP 500**，拉取前先用 `GET /history/{key}` 取 head |
| 拉取顺序 | 同一 uuid 多次出现时后者覆盖前者；一个 batch 可能展开成多个实体 |
| 推送（未实现） | `POST /commit?ancestor-index=N&_cnt=1`；create 用 34 键金标准快照，update 只发变化字段 |
| 两个易错值 | `ss`：完成 = **3**；`st`：`0=inbox / 1=started / 2=deferred` |
| notes | `nt` 是对象；`ch` = CRC32(正文 UTF-8)；补丁 `p`/`l` 按**字节**计 |
| 重复规则 | `rr`/`rp` 双写；这里**原样保存 JSON**，不做有损解析 |

与上游文档不同的两处列表规则（已在代码里修正并加了测试）：

- **Someday** 必须加 `start_date IS NULL`，否则会把 Upcoming 的任务也算进来
- **Anytime** 是 `st=1 且无日期` 或 `st=0 且在项目/区域内`；文档里 `start=0` 的写法会漏掉绝大多数 Anytime 任务

---

## 构建

```bash
cargo build --release
cargo test
```

TLS 使用 `rustls` + `ring`（纯 Rust），根证书走平台信任库；构建和运行都**不需要 OpenSSL、cmake 或 nasm**。

## 跨平台

- **纯 Rust**：不需要 Things.app、AppleScript、macOS API；Linux / macOS / Windows 都能跑
- **唯一的平台分支**是默认数据目录：macOS `~/Library/Application Support/things-mcp/`、Linux `$XDG_DATA_HOME/things-mcp/`、Windows `%APPDATA%\\things-mcp\\`，都可用 `THINGS_DB` 覆盖
- 协议里的 `App-Id: com.culturedcode.ThingsMac`、`User-Agent: ThingsMac/...`、`Things-Client-Info` 里的 `on: macOS` 是**客户端身份标识**（模拟官方 Mac 客户端），不是运行时依赖
- CI 在 Ubuntu / macOS / Windows 三个平台上跑 `cargo test`

## 配置

| 环境变量 | 用途 |
|---|---|
| `THINGS_EMAIL` | 账号邮箱（也接受 `THINGS_USERNAME`） |
| `THINGS_PASSWORD` | 密码（也接受 `THINGS_TOKEN`；**不落盘、不写日志**） |
| `THINGS_DB` | SQLite 路径。默认：macOS `~/Library/Application Support/things-mcp/things.sqlite`、Linux `$XDG_DATA_HOME/things-mcp/things.sqlite`、Windows `%APPDATA%\things-mcp\things.sqlite` |
| `THINGS_BASE_URL` | 覆盖云端地址（测试用） |
| `THINGS_ENABLE_WRITES` | `1`/`true`/`yes`/`on` 才允许真实推送；未设置时只能 `--dry-run` |
| `RUST_LOG` | 日志级别（输出到 stderr） |

## 用法

```bash
# 首次：全量拉取（约 17MB / 2.3 万 item，只读）
cargo run --release -- sync --full

# 之后增量同步；多进程安全（MCP server 和 CLI 可同时跑）
cargo run --release -- sync

# 一键验收：连续同步两次，第二次必须 no-op，并跑解码/引用审计
cargo run --release -- check

# 各列表计数 —— 用它和 Things.app 对拍
cargo run --release -- verify

# 解码/引用审计（22 项）
cargo run --release -- doctor

# 单个列表 / 搜索 / 详情
cargo run --release -- list today --limit 20
cargo run --release -- list project-<id>
cargo run --release -- list tag-<id>
cargo run --release -- search "发票" --project <id>
cargo run --release -- show <task-id>

# 写操作预览（不需要凭据、不联网、不写库）
cargo run --release -- create "买牛奶" --when today --notes "两盒" --dry-run

# 真实写入（需要 THINGS_ENABLE_WRITES=1）
export THINGS_ENABLE_WRITES=1
cargo run --release -- create "买牛奶" --when today
cargo run --release -- update <id> --title "新标题" --when tomorrow
cargo run --release -- complete <id>
cargo run --release -- trash <id>
cargo run --release -- delete <id>

# 账号信息
cargo run --release -- account

# 启动 MCP server（stdio；日志走 stderr，stdout 只走协议）
cargo run --release -- serve
```

所有输出都是结构化 JSON（`lists`/`verify`/`sync` 的人类可读输出除外）。

## MCP server

`things-mcp serve` 通过 stdio 提供 29 个工具（17 个读 + 12 个写）：

| 工具 | 说明 |
|---|---|
| `list_tasks` | 任意视图：inbox/today/upcoming/anytime/someday/logbook/deadlines/repeating/trash/all/project-\<id\>/area-\<id\>/tag-\<id\> |
| `get_task` | 详情：notes、项目/区域/分节、标签、清单项、日期、重复规则 |
| `search` | 标题/备注搜索 + project/area/tag/status 过滤 |
| `list_projects` | 项目 + 未完成/已完成计数 |
| `list_areas` | 区域 + 项目/任务计数 |
| `list_tags` | 标签（平铺 + parent 引用 + 任务计数） |
| `list_logbook` | 已完成/已取消 |
| `list_deadlines` | 有截止日的任务 |
| `list_upcoming` | 排期在未来的任务 |
| `list_by_project` / `list_by_area` / `list_by_tag` | 按容器列任务 |
| `get_project` | 项目详情：备注、分节（含每节未完成数）、任务 |
| `get_area` | 区域详情：项目列表 + 任务 |
| `get_tag` | 标签详情：父标签、子标签、带该标签的任务 |
| `sync_now` | 立即拉取（`full: true` 可全量重来） |
| `sync_status` | history key、游标、上次同步、各列表计数 |
| `create_task` / `create_project` / `create_area` / `create_tag` | 创建实体（34/4/5 键快照） |
| `update_task` | 最小增量更新（title/notes/when/deadline/容器/标签） |
| `complete_task` / `cancel_task` / `reopen_task` | 完成 / 取消 / 重新打开 |
| `trash_task` / `restore_task` / `delete_task` | 软删 / 恢复 / 硬删 |
| `add_checklist_item` | 添加清单项（9 键快照） |

写工具都支持 `dry_run: true`：只返回将要推送的精确 payload，不联网、不写库。

行为说明：

- 读工具在数据超过 `THINGS_REFRESH_SECS`（默认 60 秒，0 = 不自动刷新）时**先做一次增量拉取**；网络失败只加 `warning` 字段，仍然返回本地数据。
- 没有配置凭据时自动进入**本地只读模式**，不访问网络。
- 输出同时带 `structuredContent` 和文本；列表结果含 `total` / `has_more`，便于分页。

Claude Desktop / Cursor 配置示例（`claude_desktop_config.json`）：

```json
{
  "mcpServers": {
    "things": {
      "command": "/absolute/path/to/things-mcp/target/release/things-mcp",
      "args": ["serve"],
      "env": {
        "THINGS_EMAIL": "you@example.com",
        "THINGS_PASSWORD": "your-password",
        "THINGS_DB": "/Users/you/Library/Application Support/things-mcp/things.sqlite"
      }
    }
  }
}
```

> 密码放在客户端配置里不是最佳实践；更稳的做法是让客户端继承 shell 环境变量，或用系统的 secret 管理器注入。

## 写路径安全模型

1. **金标准校验**：Task7 create 必须包含全部 34 键（ChecklistItem3 = 9、Area3 = 4、Tag4 = 5），且类型正确；缺一即拒绝推送
2. **dry-run 零副作用**：不需要凭据，不联网，不写 SQLite
3. **环境开关**：`THINGS_ENABLE_WRITES=1` 才允许真实 commit；默认关闭
4. **先推送后落库**：commit 成功后才应用到本地副本，避免「本地有、云端无」
5. **冲突重试**：commit 返回 `OutdatedAncestor` → 先增量拉取再重试一次
6. **最小增量**：update 只发变化字段；只有内容/状态变化才带 `md`，纯排序不带
7. **游标不跳**：commit 后不推进 pull 游标（只记 `commit_head`），下次拉取仍会重放自己的写入（幂等）

### 本地写路径端到端（不需要真实账号）

仓库自带一个内存版 Things Cloud mock（`scripts/mock-cloud.py`）：它实现认证、拉取、commit、祖先索引检查和重放，能完整跑一遍写路径而**不碰真实账号**。

```bash
cargo build --release
bash scripts/e2e-write.sh
```

脚本会依次执行：`sync --full` → 真实 commit 创建 → 增量重放自己的写入 → complete → trash → delete → 再次重放 → `doctor` → `check`，并断言最后列表为空、第二次 sync 是 no-op。

## 结构

```
src/
├── main.rs              # CLI: account/sync/lists/list/search/show/projects/areas/tags/verify
├── config.rs            # 环境变量 + 默认路径
├── fmt.rs               # epoch-day / 小数秒 → ISO
├── ids.rs               # base58 实体 id、App-Instance-Id
├── notes.rs             # nt 解码：全文 / CRC32 / 字节补丁
├── store/
│   ├── schema.sql       # SQLite schema（可查询列 + raw JSON）
│   ├── mod.rs           # 打开/迁移、原子 apply_page、meta
│   └── query.rs         # 列表语义、搜索、项目/区域/标签、详情
├── mcp.rs               # stdio MCP server：17 读 + 12 写工具
├── write.rs             # 写路径：安全闸、commit、冲突重试、dry-run
├── sync.rs              # 分页拉取、游标、并发接管、历史重置
├── testing.rs           # FakeTransport（测试用）
└── things/
    ├── client.rs        # 认证头、things-response、重试、分页端点
    ├── model.rs         # Task / Area / Tag / ChecklistItem + 枚举常量
    └── wire.rs          # 缩写字段 → 实体（Task7 全字段）
```

## 持久化设计

- 每个实体表存**可查询列** + 一列 `raw`（保留全部 34 键），所以既有索引查询、也不丢未知字段
- `apply_page` 在**一个事务**里同时提交实体和游标，并且只在游标未被其它进程推进时才提交
- WAL + `busy_timeout`：MCP server 常驻时仍可手动跑 CLI
- 游标越界/云历史被替换 → 自动清空本地副本并全量重拉

## 测试

```bash
cargo test
```

覆盖：Task7 解码、notes 字节补丁（含 emoji/中文边界）、分页（**截断页**、head 边界、stalled、历史重置）、游标并发接管、列表互斥性、搜索、字段级合并，**完整历史重放**（create/update/notes 补丁/硬删/tombstone/Settings）、认证回退（raw → percent-encode）、429 重试、历史 key 轮换、双连接并发写入，以及**真实二进制**的 stdio MCP 端到端（`tools/list` 29 个工具 + 读/写 `tools/call`，写操作用 dry-run）。

本地写路径端到端（mock cloud）：

```bash
bash scripts/e2e-write.sh
```

另有一个需联网的 TLS 冒烟测试（默认 ignored）：

```bash
cargo test --test tls_smoke -- --ignored
```

## 验收记录

真实账号（47 条任务 / 5 标签 / 10 清单项）：

```
first sync:    pages 1 items 2 (upserted 2, deleted 0)    ← 增量拉到新变更
second sync:   pages 0 items 0 (upserted 0, deleted 0)     ← no-op
tasks:         47 -> 47
no-op: PASS    data stable: PASS    audit: PASS
```

## 下一步

1. **写路径真机验收**：先 `--dry-run` 看 payload，再创建一条一次性任务，确认 Mac/iOS 能正常显示且不崩；然后 complete → trash → delete，最后跑 `check`
2. **operation_log + 三方合并**：多端同时编辑时的字段级 LWW、离线优先、撤销
3. **重复规则写入**：`set_repeat` / `clear_repeat`（`rr`/`rp` 双写）
4. **移动/排序**：`move_task`（项目/区域/分节 + 索引重排）、`set_index`

## 免责声明

- 本项目与 Cultured Code 无关，Things® 是其商标。
- 它通过逆向得到的私有协议直接访问 Things Cloud，**可能违反其服务条款**，协议也可能随时变更。
- 仅建议用于**个人账号、个人设备**的自用自动化；不要用于批量操作、压测或面向多用户的公开服务。
- 写操作（Phase 2）会直接影响你所有已登录设备的同步数据，请先 `dry_run` 并小规模对拍。
