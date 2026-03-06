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
LLM_API_BASE=http://localhost:11434/v1
LLM_API_KEY=your-api-key
LLM_MODEL=claude-3-sonnet-20240229

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

参考 Claude Code 的命令风格：

- `/help` - 显示帮助
- `/skills` - 列出技能（参考 Claude Code）
- `/skill <name>` - 使用技能
- `/clear` - 清除对话历史
- `/exit` 或 `/quit` - 退出程序

### 工具调用

Agent 可以自动调用各种工具执行任务，也可以手动触发任务和 Worktree 操作。

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
