# Rustpilot

> 本项目参考 [Claude Code](https://claude.com/claude-code) 
> 
> Anthropic 推出的 AI 编程助手 CLI 工具的设计理念实现。

一个基于 Rust 的 AI 编码代理工具，提供终端会话管理、任务管理和 Git Worktree 集成功能。

## 项目概述

Rustpilot 是一个用于辅助编程的 CLI 工具，它通过与 LLM（如 OpenAI 兼容的 API）交互，为开发者提供智能化的编码辅助。项目采用 Rust 语言开发，具有高性能和跨平台特性。

### 设计理念

本项目**参考 [Claude Code](https://claude.com/claude-code)** 实现。Claude Code 是 Anthropic 推出的 AI 编程助手 CLI 工具，本项目借鉴了其核心设计理念：

- **任务与执行分离** - 任务（Task）作为控制面，Worktree 作为执行面
- **长期会话管理** - 支持创建和管理长期运行的终端会话
- **工具驱动** - 通过工具扩展 AI 助手的能力
- **Skills 扩展** - 支持自定义技能扩展

### 核心特性

- **终端会话管理** - 创建和管理长期运行的终端会话
- **任务管理** - 内置任务板支持并行和高风险工作处理
- **Git Worktree 集成** - 支持 Git Worktree 操作
- **Shell 与文件操作** - 内置 shell 命令执行和文件操作工具
- **Skills 扩展** - 支持自定义技能扩展

## 技术架构

### 模块结构

```
src/
├── main.rs              # 程序入口
├── lib.rs               # 库入口，导出所有模块
├── agent.rs             # Agent 循环和工具调度
├── agent_tools.rs       # 内置工具定义和调度
├── cli.rs               # CLI 命令处理
├── config.rs            # 配置管理
├── constants.rs         # 常量定义
├── openai_compat.rs     # OpenAI 兼容接口
├── runtime_env.rs       # 运行时环境检测
├── skills.rs            # 技能注册表
├── activity.rs          # 活动/进度渲染
├── shell_file_tools.rs  # Shell 和文件操作工具
├── terminal_session.rs  # 终端会话管理
└── project_tools/       # 项目工具模块
    ├── context.rs       # 项目上下文
    ├── event.rs         # 事件管理
    ├── task.rs          # 任务管理
    ├── tools.rs         # 项目工具定义
    ├── util.rs          # 工具函数
    └── worktree.rs      # Git Worktree 管理
```

### 核心技术栈

- **Rust 2024 Edition** - 最新 Rust 特性
- **Tokio** - 异步运行时
- **Axum** - Web 框架（用于可能的 API 服务）
- **Reqwest** - HTTP 客户端（用于 LLM API 调用）
- **Portable-PTY** - 跨平台 PTY 支持

## 功能说明

本项目的功能设计参考 Claude Code，主要包含以下核心功能模块：

### 1. 终端会话管理（参考 Claude Code `terminal_*` 工具）

项目支持创建和管理长期运行的终端会话，具有以下功能：

- `terminal_create` - 创建新会话
- `terminal_write` - 向会话写入输入
- `terminal_read` - 读取会话输出
- `terminal_list` - 列出所有会话
- `terminal_status` - 查看会话状态
- `terminal_kill` - 终止会话
- `terminal_resize` - 调整终端大小（PTY 模式）

**会话持久化**：
- 会话元数据持久化到磁盘
- 会话输出日志保存
- 重启后可恢复历史会话

### 2. 任务管理（参考 Claude Code `task_*` 工具）

通过内置任务板管理并行和高风险工作：

- `task_create` - 创建任务
- `task_list` - 列出所有任务
- `task_get` - 获取任务详情
- `task_update` - 更新任务状态
- `task_bind_worktree` - 绑定任务到 Worktree

团队通信工具：

- `team_send` - 向指定成员发送 mailbox 消息
- `team_inbox` - 读取指定成员的最近消息
- `team_poll` - 按游标轮询某成员的新消息（适合持续消费）
- `team_ack` - 对指定消息发送 ACK（生成 `task.ack` 回执）

### 3. Git Worktree 集成（参考 Claude Code `worktree_*` 工具）

支持 Git Worktree 操作：

- `worktree_create` - 创建 Worktree
- `worktree_list` - 列出所有 Worktree
- `worktree_status` - 查看 Worktree 状态
- `worktree_keep` - 保留 Worktree
- `worktree_remove` - 移除 Worktree
- `worktree_run` - 在 Worktree 中执行命令
- `worktree_events` - 查看 Worktree 生命周期事件

### 4. Shell 和文件操作

提供基础的 shell 命令执行和文件操作：

- `bash` - 执行 Shell 命令
- 文件读写操作
- 路径操作

### 5. Skills 扩展（参考 Claude Code `/skills` 命令）

支持自定义技能扩展：

- `/skills` - 列出所有可用技能
- `/skill <name>` - 使用指定技能

## 配置说明

### 环境变量

项目使用 `.env` 文件配置，参考 `.env.example`：

```bash
# LLM 配置
LLM_PROVIDER=minimax
LLM_API_BASE_URL=https://api.minimaxi.com/v1
LLM_API_KEY=your-api-key
LLM_MODEL=MiniMax-M2.5

# API 超时设置（秒）
LLM_TIMEOUT_SECS=120
```

### 配置文件

- `.env` - 本地环境配置
- `.env.example` - 配置模板

## 使用方法

### 启动程序

```bash
cargo run
```

### 基本交互

程序启动后会显示交互式提示符，系统提示词参考 Claude Code 的设计理念：

```
> 你好
[Agent 响应...]
```

### CLI 命令

支持以下命令：

- `q` / `quit` / `exit` - 退出程序
- `/tasks` - 查看任务列表
- `/worktrees` - 查看 Worktree 列表
- `/events` - 查看最近事件
- `/status` - 查看当前执行状态
- `/ask <内容>` - 与主代理直接对话（不入团队任务队列）
- `/focus lead` - 切换到主 agent 交互模式（自然输入直接对话）
- `/focus team` - 切换到团队队列模式（自然输入自动入队任务）
- `/focus worker <task_id>` - 切换到指定子 agent 交互模式（将输入路由到子会话）
- `/focus status` - 查看当前交互焦点
- `/reply <task_id> <补充信息>` - 回复被阻塞任务并重新排队
- `/team run <需求>` - 一键创建团队任务并自动调度执行
- `/team start [max_parallel]` - 启动团队调度器（拉起临时 teammate 子进程）
- `/team stop` - 停止团队调度器
- `/team status` - 查看团队调度器状态
- `/skills` - 列出技能
- `/skill <name>` - 查看指定技能内容
- `/skill-tool-init <name>` - 创建外部工具 skill 模板
- `/mcp-tool-init <name>` - 创建 MCP 工具模板

### 工具调用

Agent 可以自动调用各种工具执行任务，也可以手动触发任务和 Worktree 操作。

团队调度默认会自动起停：
- 检测到有 `pending` 任务时自动启动调度器
- 队列为空且无运行中的 teammate 时自动停止调度器
- 普通自然语言输入会自动入队为团队任务（无需 `/team run`）
- 子 agent 会自动向 `lead` 汇报 `task.started/task.result/task.failed`
- 对 `task.result/task.failed` 会自动 ACK，并注入主 agent 会话上下文
- 子 agent 在执行模型轮次与工具调用时会持续上报 `task.progress`
- 收到 `task.request_clarification` 时任务会自动置为 `blocked`，使用 `/reply` 后会恢复（运行中 worker -> `in_progress`，否则 -> `pending`）
- 若子 agent 仍在运行，`/reply` 会直接投递给该子 agent（不退出进程、不重新拉起）
- `/focus worker <task_id>` 依赖 `.team/agents.json` 映射（调度器自动维护）

子 agent 窗口模式：
- 默认 `RUSTPILOT_TEAM_SPAWN=auto`：优先 `tmux`，无 `tmux` 时回退为独立 terminal session（PTY）
- `RUSTPILOT_TEAM_SPAWN=tmux`：强制 tmux 窗口
- `RUSTPILOT_TEAM_SPAWN=terminal`：强制独立 terminal session
- `RUSTPILOT_TEAM_SPAWN=inherit`：回退到主终端混合输出

### 外部工具加载

通过 Skill 对接外部工具。默认扫描 `skills/`（或 `SKILLS_DIR` 指定目录）下的技能目录。

每个技能目录就是一个完整外部工具，推荐模板：

```
skills/
  echo-tool/
    SKILL.md
    tests/
      smoke.json
    tool.sh
```

`SKILL.md` frontmatter 示例：

```yaml
---
name: echo_external
description: 回显输入参数
tool_language: bash
tool_runtime: bash 5
tool_command: bash
tool_args_json: ["./tool.sh"]
---
```

`tests/smoke.json` 示例：

```json
{
  "name": "smoke",
  "arguments": { "input": "hello" },
  "expect_status": 0,
  "expect_stdout_contains": "hello"
}
```

加载规则：
- 必须有 `SKILL.md` 和 `tests/`
- `tests/` 目录必须至少有 1 个 `.json` 测试用例
- 工具加载时会主动执行测试，失败则该工具跳过
- 目录内容发生变化时，系统会重新测试并重载工具

执行约定：
- 调用参数 JSON 会写入外部命令 `stdin`
- 同时注入环境变量 `RUSTPILOT_TOOL_NAME`、`RUSTPILOT_TOOL_ARGS`

### MCP 支持

支持通过 MCP（Model Context Protocol）加载外部工具。采用“每个 JSON 一个 MCP 工具”的目录化方式，默认扫描 `mcps/`（可用 `MCPS_DIR` 覆盖）。

目录模板：

```
mcps/
  filesystem-read/
    mcp.json
    tests/
      smoke.json
```

可通过命令快速初始化：

```text
/mcp-tool-init filesystem-read
```

示例 `mcps/filesystem-read/mcp.json`：

```json
{
  "name": "mcp_fs_read_file",
  "description": "Read file through MCP filesystem server",
  "parameters": {
    "type": "object",
    "properties": {
      "path": { "type": "string" }
    },
    "required": ["path"]
  },
  "mcp_tool": "read_file",
  "server": {
    "name": "filesystem",
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
  }
}
```

示例 `mcps/filesystem-read/tests/smoke.json`：

```json
{
  "name": "smoke",
  "arguments": { "path": "README.md" },
  "expect_error": false,
  "expect_text_contains": "Rustpilot"
}
```

加载规则：
- 每个工具目录必须有 `mcp.json` 和 `tests/`
- `tests/` 至少包含 1 个 `.json` 用例
- 加载时主动运行测试，通过后才注册工具
- 目录内容变化时，才会触发重测和重载

注册后的工具名称就是 `mcp.json` 里的 `name`，例如：

```text
mcp_fs_read_file
```

调用时参数会转发到对应 server 的 MCP `tools/call`：

```json
{
  "name": "read_file",
  "arguments": { "path": "README.md" }
}
```

如果你希望忽略 `parameters`，也可以不写，系统会默认:

```json
{
  "type": "object"
}
```

## 开发指南

### 构建项目

```bash
cargo build
```

### 运行测试

```bash
cargo test
```

### 代码结构

项目遵循以下开发规范：

1. 模块化设计 - 职责分离
2. 错误处理 - 使用 `anyhow` 进行错误传播
3. 异步编程 - 使用 Tokio 进行异步操作
4. 持久化 - 会话和任务状态持久化

### 开发流程

参考 `DEVLOG.md` 了解项目的开发历程和状态：

1. 每次开发前回顾目标
2. 根据当前状态调整下一步
3. 记录开发变更

## 未来计划

根据 `TODO.md`，可能的扩展方向：

- [ ] 转发可视化窗口
- [ ] 直接显示并唤起新的命令行窗口
- [ ] 指定程序窗口的远端查看
- [ ] 命令执行结果的优化展示
- [ ] 不同平台下的兼容方案

## CI/CD

项目使用 GitHub Actions 实现自动构建和发布：

### 构建工作流

- **触发条件**：
  - 推送至 `main` 分支
  - 推送 `v*` 标签
  - 提交 Pull Request 到 `main` 分支

- **构建平台**：
  - Linux x86_64
  - Linux ARM64
  - macOS x86_64
  - macOS ARM64 (Apple Silicon)
  - Windows x86_64

### 发布流程

1. 推送标签到 GitHub：
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

2. GitHub Actions 会自动构建所有平台的二进制文件

3. 构建完成后自动创建 GitHub Release

## 依赖项

主要依赖：

- `anyhow` - 错误处理
- `axum` - Web 框架
- `dotenvy` - 环境变量加载
- `portable-pty` - PTY 支持
- `reqwest` - HTTP 客户端
- `serde` / `serde_json` - 序列化
- `tokio` - 异步运行时
- `tower` - HTTP 中间件

## 关于 Claude Code

[Claude Code](https://claude.com/claude-code) 是 Anthropic 公司推出的 AI 编程助手 CLI 工具，它能够：

- 在终端中与开发者进行交互式对话
- 通过工具执行各种开发任务
- 管理长期运行的终端会话
- 支持任务和 Git Worktree 管理
- 通过自定义技能扩展功能

本项目参考 Claude Code 的设计理念，使用 Rust 语言实现了一个本地化的 AI 编程辅助工具。

## 许可证

MIT License
