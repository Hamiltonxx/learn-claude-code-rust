# learn-agent-rust

用 Rust 重写 [learn-claude-code](https://github.com/shareAI-lab/learn-claude-code) 的全部 12 个 session，深入理解 AI Agent 原理，同时真正把 Rust 用起来。

项目网站：https://learncc.cirray.cn
完整计划：`~/projects/ssg/myblog/content/posts/learn-agent-rust-two-week-plan.md`

---

## 技术栈

- **语言**：Rust（tokio + reqwest + serde）
- **API**：Anthropic，通过代理 `https://api.ofox.ai/anthropic/v1/messages`，认证头用 `Authorization: Bearer`（不是官方的 `x-api-key`）
- **模型**：`claude-haiku-4-5-20251001`（开发调试阶段）
- **公共类型**：`src/lib.rs`，包含 `Message`、`ApiRequest`、`ApiResponse`、`ContentBlock`
- **每个 session**：`src/bin/s0N_xxx.rs`，用 `cargo run --bin s0N_xxx` 运行

---

## 进度总览

| Day | 内容 | 状态 |
|-----|------|------|
| 1 | Rust 项目初始化 + S01 agent loop（上下文记忆） | ✅ |
| 2 | ECS 部署，网站上线 https://learncc.cirray.cn | ✅ |
| 3 | S02 Tool Dispatch（trait object + HashMap） | ⬜ |
| 4 | S03 TodoWrite + S04 Subagent | ⬜ |
| 5 | S05 Skill Loading + S06 Context Compact | ⬜ |
| 6-7 | 缓冲 + 注释 + 文档 | ⬜ |
| 8 | S07 Task System | ⬜ |
| 9 | S08 Background Tasks | ⬜ |
| 10 | S09 Agent Teams | ⬜ |
| 11 | S10 Protocols + S11 Autonomous | ⬜ |
| 12 | S12 Worktree Isolation + s_full.rs | ⬜ |
| 13 | 网站改造（展示 Rust 代码） | ⬜ |
| 14 | 发布日 | ⬜ |

**当前进展**：看进度表里第一个 ⬜ 即为今天的任务。

---

## 每日任务详情

### Day 3 — S02 Tool Dispatch
**目标**：从硬编码 match 变成 HashMap 动态分发

- 定义 `Tool` trait（`name` / `definition` / `async execute`）
- 实现 4 个工具：`BashTool`、`ReadFileTool`、`WriteFileTool`、`EditFileTool`
- 用 `HashMap<String, Box<dyn Tool>>` 做 dispatch
- 改 agent loop：处理 `tool_use` → 执行 → 返回 `tool_result` → 继续循环直到 `end_turn`
- 测试："读取 Cargo.toml 的内容然后总结"

**Rust 核心**：`trait object`、`Box<dyn Trait>`、`async_trait`、`HashMap`

---

### Day 4 — S03 TodoWrite + S04 Subagent
**目标**：agent 能先规划再执行，能拆子任务

S03：
- 实现 `TodoManager` struct（`Vec<TodoItem>` + CRUD）
- 实现 `TodoWriteTool`，system prompt 加规则"先列计划再执行"
- 测试："帮我创建一个 hello world Rust 项目并运行"

S04：
- 实现 subagent：独立的 `agent_loop`，拥有干净的 `messages[]`
- 主 agent 通过 `dispatch_agent` tool 派生子任务
- 关键：子 agent 的 messages 和主 agent 完全隔离

**Rust 核心**：struct 方法、Vec 操作、嵌套 async 函数

---

### Day 5 — S05 Skill Loading + S06 Context Compact
**目标**：按需加载知识 + 上下文不会爆

S05：
- 实现 `SkillLoaderTool`：读取 `skills/` 下的 `.md` 文件
- 知识通过 `tool_result` 注入，不是启动时塞进 system prompt

S06：
- 超过 20 条消息时，把前 10 条压缩成摘要
- 用模型做摘要，保留最近 N 条原文

**Rust 核心**：`std::fs` 文件 I/O、Vec 切片操作

---

### Day 6-7 — 缓冲 + 注释 + 文档
- 修复前 5 天遗留 bug
- 给 s01-s06 加完整中文注释
- ECS 上 `git pull` 更新网站

---

### Day 8 — S07 Task System
- `Task` struct：id、title、status、deps
- `TaskManager`：CRUD + 拓扑排序
- 任务持久化为 JSON 文件

**Rust 核心**：serde 序列化到文件、图的拓扑排序、`Result` 错误处理

---

### Day 9 — S08 Background Tasks
- `tokio::spawn` 后台执行耗时命令
- `mpsc channel` 通知主循环任务完成
- agent 不阻塞，可以继续处理其他事情

**Rust 核心**：`tokio::spawn`、`mpsc`、`Arc<Mutex<T>>`

---

### Day 10 — S09 Agent Teams
- 定义 `Teammate` struct（name、role、system_prompt）
- 主 agent 可以 `send_message` 给 teammate
- teammate 收到后独立运行自己的 `agent_loop`

**Rust 核心**：多 tokio task 并发、channel 通信

---

### Day 11 — S10 Protocols + S11 Autonomous
S10：统一消息格式 + shutdown / plan approval 协议（用 enum 表示状态）
S11：teammate 空闲时自动扫描任务板，自动认领符合自己角色的任务

**Rust 核心**：enum 状态机、`loop + select!`

---

### Day 12 — S12 Worktree Isolation + s_full.rs
- 每个 teammate 在独立 CWD 下操作，防止互相干扰
- 写 `s_full.rs`：把全部 12 个机制组合到一个文件
- 端到端测试

---

### Day 13 — 网站改造
- 修改网站源码查看器：`.py` → `.rs` 文件路径
- 代码高亮换成 Rust
- 首页文案体现 Rust 特色
- ECS 上重新构建部署

---

### Day 14 — 发布日
- 所有 session 能编译能跑
- 去原仓库提 issue，请求在 README 加链接
- （可选）在 Rust 社区发帖：r/rust、V2EX
