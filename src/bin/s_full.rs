// s_full.rs — 全部 12 个机制的综合演示
//
// 机制一览：
//   S01  agent loop + 上下文记忆
//   S02  tool dispatch（HashMap + trait object）
//   S03  TodoManager（先规划再执行）
//   S04  subagent（独立 messages，隔离上下文）
//   S05  skill loading（按需从文件注入知识）
//   S06  context compact（超过阈值自动压缩）
//   S07  task system（依赖拓扑排序）
//   S08  background tasks（tokio::spawn + mpsc）
//   S09  agent teams（多 teammate 并发）
//   S10  protocols（统一消息格式 + shutdown 协议）
//   S11  autonomous（teammate 自主认领任务）
//   S12  worktree isolation（每个 teammate 独立 CWD）

use async_trait::async_trait;
use learn_claude_code_rust::{ApiRequest, ApiResponse, ContentBlock, Message, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

const API_URL: &str = "https://api.ofox.ai/anthropic/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";
const COMPACT_THRESHOLD: usize = 20; // S06：超过此条数触发压缩

// ════════════════════════════════════════════════════════════════════════════
// API 辅助
// ════════════════════════════════════════════════════════════════════════════

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[Message],
    tools: &[Value],
    system: &str,
) -> ApiResponse {
    let req = ApiRequest {
        model: MODEL.to_string(),
        max_tokens: 8096,
        system: system.to_string(),
        messages: messages.to_vec(),
        tools: if tools.is_empty() { None } else { Some(tools.to_vec()) },
    };
    client.post(API_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&req).send().await.unwrap()
        .json::<ApiResponse>().await.unwrap()
}

// ════════════════════════════════════════════════════════════════════════════
// S06 — Context Compact
// ════════════════════════════════════════════════════════════════════════════

/// 超过阈值时把前半段压缩成摘要，保留后半段原文
async fn maybe_compact(
    client: &reqwest::Client,
    api_key: &str,
    messages: &mut Vec<Message>,
) {
    if messages.len() < COMPACT_THRESHOLD {
        return;
    }
    let half = messages.len() / 2;
    let old: Vec<Message> = messages.drain(..half).collect();

    let summary_text = old.iter()
        .filter_map(|m| m.content.as_str().map(|s| format!("[{}] {}", m.role, s)))
        .collect::<Vec<_>>().join("\n");

    let summary_req = vec![Message {
        role: "user".to_string(),
        content: json!(format!("请用中文把以下对话压缩成简洁摘要：\n{}", summary_text)),
    }];
    let summary_resp = call_api(client, api_key, &summary_req, &[], "你是摘要助手").await;
    let summary = summary_resp.content.iter()
        .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
        .collect::<Vec<_>>().join("");

    messages.insert(0, Message {
        role: "user".to_string(),
        content: json!(format!("[对话摘要] {}", summary)),
    });
    messages.insert(1, Message {
        role: "assistant".to_string(),
        content: json!("已了解历史对话摘要，继续处理。"),
    });

    println!("[S06 压缩] 已将 {} 条消息压缩为摘要", half);
}

// ════════════════════════════════════════════════════════════════════════════
// S03 — TodoManager
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TodoItem {
    id: u32,
    content: String,
    done: bool,
}

#[derive(Default)]
struct TodoManager {
    items: Vec<TodoItem>,
    next_id: u32,
}

impl TodoManager {
    fn add(&mut self, content: &str) -> u32 {
        self.next_id += 1;
        self.items.push(TodoItem { id: self.next_id, content: content.to_string(), done: false });
        self.next_id
    }
    fn complete(&mut self, id: u32) -> bool {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.done = true; true
        } else { false }
    }
    fn list(&self) -> String {
        if self.items.is_empty() { return "待办为空".to_string(); }
        self.items.iter()
            .map(|i| format!("[{}] #{} {}", if i.done { "x" } else { " " }, i.id, i.content))
            .collect::<Vec<_>>().join("\n")
    }
}

type SharedTodo = Arc<Mutex<TodoManager>>;

// ════════════════════════════════════════════════════════════════════════════
// S07 — Task System（带依赖的任务，拓扑排序）
// ════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskItem {
    id: u32,
    title: String,
    deps: Vec<u32>,
    done: bool,
}

#[allow(dead_code)]
struct TaskManager {
    tasks: Vec<TaskItem>,
    next_id: u32,
}

#[allow(dead_code)]
impl TaskManager {
    fn new() -> Self { TaskManager { tasks: vec![], next_id: 0 } }
    fn add(&mut self, title: &str, deps: Vec<u32>) -> u32 {
        self.next_id += 1;
        self.tasks.push(TaskItem { id: self.next_id, title: title.to_string(), deps, done: false });
        self.next_id
    }
    /// 拓扑排序：返回可执行（依赖全部完成）的任务列表
    fn ready(&self) -> Vec<&TaskItem> {
        let done_ids: std::collections::HashSet<u32> = self.tasks.iter()
            .filter(|t| t.done).map(|t| t.id).collect();
        self.tasks.iter()
            .filter(|t| !t.done && t.deps.iter().all(|d| done_ids.contains(d)))
            .collect()
    }
    fn complete(&mut self, id: u32) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) { t.done = true; }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// S11 — 共享任务板（自主认领）
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BoardStatus {
    Open,
    InProgress { claimed_by: String },
    Done { result: String },
}

impl std::fmt::Display for BoardStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardStatus::Open => write!(f, "open"),
            BoardStatus::InProgress { claimed_by } => write!(f, "in_progress({})", claimed_by),
            BoardStatus::Done { .. } => write!(f, "done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoardTask {
    id: u32,
    title: String,
    role_hint: String,
    status: BoardStatus,
}

type TaskBoard = Arc<Mutex<Vec<BoardTask>>>;

fn board_add(board: &TaskBoard, title: &str, role_hint: &str) -> u32 {
    let mut tasks = board.lock().unwrap();
    let id = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    tasks.push(BoardTask { id, title: title.to_string(), role_hint: role_hint.to_string(), status: BoardStatus::Open });
    id
}

fn board_claim(board: &TaskBoard, role: &str) -> Option<BoardTask> {
    let mut tasks = board.lock().unwrap();
    for task in tasks.iter_mut() {
        if task.status == BoardStatus::Open && (task.role_hint == role || task.role_hint == "any") {
            task.status = BoardStatus::InProgress { claimed_by: role.to_string() };
            return Some(task.clone());
        }
    }
    None
}

fn board_complete(board: &TaskBoard, id: u32, result: &str) {
    let mut tasks = board.lock().unwrap();
    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
        t.status = BoardStatus::Done { result: result.to_string() };
    }
}

// ════════════════════════════════════════════════════════════════════════════
// S12 — 带 cwd 隔离的工具
// ════════════════════════════════════════════════════════════════════════════

struct BashTool { cwd: PathBuf }
#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn definition(&self) -> Value {
        json!({ "name": "bash", "description": "执行 bash 命令（在 teammate 隔离目录内）",
            "input_schema": { "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] } })
    }
    async fn execute(&self, input: Value) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c").arg(input["command"].as_str().unwrap_or(""))
            .current_dir(&self.cwd).output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
            Err(e) => format!("执行失败: {}", e),
        }
    }
}

struct WriteFileTool { cwd: PathBuf }
#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn definition(&self) -> Value {
        json!({ "name": "write_file", "description": "在 teammate 隔离目录写文件",
            "input_schema": { "type": "object", "properties": {
                "path": { "type": "string" }, "content": { "type": "string" }
            }, "required": ["path", "content"] } })
    }
    async fn execute(&self, input: Value) -> String {
        let path = self.cwd.join(input["path"].as_str().unwrap_or("output.txt"));
        match std::fs::write(&path, input["content"].as_str().unwrap_or("")) {
            Ok(_) => format!("已写入 {}", path.display()),
            Err(e) => format!("失败: {}", e),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// S04 — subagent（独立 agent loop）
// ════════════════════════════════════════════════════════════════════════════

async fn run_subagent(
    client: &reqwest::Client,
    api_key: &str,
    task: &str,
    cwd: &PathBuf,
    system: &str,
) -> String {
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("bash".to_string(), Box::new(BashTool { cwd: cwd.clone() }));
    tools.insert("write_file".to_string(), Box::new(WriteFileTool { cwd: cwd.clone() }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let mut messages = vec![Message { role: "user".to_string(), content: json!(task) }];

    loop {
        let resp = call_api(client, api_key, &messages, &tool_defs, system).await;
        if resp.stop_reason.as_deref() == Some("tool_use") {
            messages.push(Message { role: "assistant".to_string(), content: json!(resp.content) });
            let mut results = vec![];
            for block in &resp.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let r = tools[name].execute(input.clone()).await;
                    results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": r }));
                }
            }
            messages.push(Message { role: "user".to_string(), content: json!(results) });
        } else {
            return resp.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("\n");
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// S09/S11/S12 — teammate worker（自主认领 + worktree 隔离）
// ════════════════════════════════════════════════════════════════════════════

async fn teammate_worker(
    name: String,
    role: String,
    system_prompt: String,
    cwd: PathBuf,
    board: TaskBoard,
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    mut shutdown: mpsc::Receiver<()>,
    done_tx: mpsc::Sender<String>, // S08：任务完成通知
) {
    println!("[{}] 上线 | 目录: {}", name, cwd.display());
    loop {
        tokio::select! {
            _ = sleep(Duration::from_secs(2)) => {
                if let Some(task) = board_claim(&board, &role) {
                    println!("[{}] 认领 #{}: {}", name, task.id, task.title);
                    // S04：用独立 subagent 完成任务
                    let result = run_subagent(&client, &api_key, &task.title, &cwd, &system_prompt).await;
                    board_complete(&board, task.id, &result);
                    let msg = format!("[{}] 完成任务 #{}: {}", name, task.id, &result[..result.len().min(80)]);
                    println!("{}", msg);
                    // S08：通知主循环
                    let _ = done_tx.send(msg).await;
                }
            }
            _ = shutdown.recv() => {
                println!("[{}] 退出。", name);
                break;
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 主 agent 工具集
// ════════════════════════════════════════════════════════════════════════════

struct TodoTool { todo: SharedTodo }
#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str { "todo" }
    fn definition(&self) -> Value {
        json!({ "name": "todo", "description": "管理待办事项。action: add/complete/list",
            "input_schema": { "type": "object", "properties": {
                "action": { "type": "string", "enum": ["add", "complete", "list"] },
                "content": { "type": "string" },
                "id": { "type": "number" }
            }, "required": ["action"] } })
    }
    async fn execute(&self, input: Value) -> String {
        let mut mgr = self.todo.lock().unwrap();
        match input["action"].as_str().unwrap_or("list") {
            "add" => { let id = mgr.add(input["content"].as_str().unwrap_or("")); format!("已添加 #{}", id) }
            "complete" => { let id = input["id"].as_u64().unwrap_or(0) as u32; format!("#{} 完成: {}", id, mgr.complete(id)) }
            _ => mgr.list(),
        }
    }
}

struct AddBoardTaskTool { board: TaskBoard }
#[async_trait]
impl Tool for AddBoardTaskTool {
    fn name(&self) -> &str { "add_board_task" }
    fn definition(&self) -> Value {
        json!({ "name": "add_board_task", "description": "往任务板添加任务，teammate 自动认领",
            "input_schema": { "type": "object", "properties": {
                "title": { "type": "string" },
                "role_hint": { "type": "string", "enum": ["coder", "reviewer", "any"] }
            }, "required": ["title", "role_hint"] } })
    }
    async fn execute(&self, input: Value) -> String {
        let id = board_add(&self.board, input["title"].as_str().unwrap_or("task"), input["role_hint"].as_str().unwrap_or("any"));
        format!("任务 #{} 已添加到任务板", id)
    }
}

struct ShowBoardTool { board: TaskBoard }
#[async_trait]
impl Tool for ShowBoardTool {
    fn name(&self) -> &str { "show_board" }
    fn definition(&self) -> Value {
        json!({ "name": "show_board", "description": "查看任务板状态",
            "input_schema": { "type": "object", "properties": {} } })
    }
    async fn execute(&self, _: Value) -> String {
        let tasks = self.board.lock().unwrap().clone();
        if tasks.is_empty() { return "任务板为空".to_string(); }
        tasks.iter().map(|t| format!("#{} [{}] ({}) {}", t.id, t.status, t.role_hint, t.title))
            .collect::<Vec<_>>().join("\n")
    }
}

struct DispatchAgentTool {
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    cwd: PathBuf,
}
#[async_trait]
impl Tool for DispatchAgentTool {
    fn name(&self) -> &str { "dispatch_agent" }
    fn definition(&self) -> Value {
        json!({ "name": "dispatch_agent", "description": "派生子 agent 独立完成复杂任务",
            "input_schema": { "type": "object", "properties": { "task": { "type": "string" } }, "required": ["task"] } })
    }
    async fn execute(&self, input: Value) -> String {
        let task = input["task"].as_str().unwrap_or("").to_string();
        println!("[S04 subagent] 启动: {}", &task[..task.len().min(50)]);
        run_subagent(&self.client, &self.api_key, &task, &self.cwd, "你是一个专注的子 agent。完成任务后用文字汇报结果。").await
    }
}

// ════════════════════════════════════════════════════════════════════════════
// main
// ════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY"));
    let client = Arc::new(reqwest::Client::new());

    // S12：建隔离工作目录
    let base = std::env::temp_dir().join("s_full_worktree");
    let coder_dir = base.join("coder");
    let reviewer_dir = base.join("reviewer");
    let main_dir = base.join("main");
    for d in [&coder_dir, &reviewer_dir, &main_dir] {
        std::fs::create_dir_all(d).unwrap();
    }

    // S11：共享任务板
    let board: TaskBoard = Arc::new(Mutex::new(vec![]));

    // S08：完成通知 channel
    let (done_tx, mut done_rx) = mpsc::channel::<String>(16);

    // S10：shutdown channels
    let (shutdown_coder_tx, shutdown_coder_rx) = mpsc::channel::<()>(1);
    let (shutdown_reviewer_tx, shutdown_reviewer_rx) = mpsc::channel::<()>(1);

    // S09：启动 teammate workers
    tokio::spawn(teammate_worker(
        "coder".into(), "coder".into(),
        "你是 Rust 代码生成专家。用 write_file 把代码写到当前目录。".into(),
        coder_dir, board.clone(), client.clone(), api_key.clone(),
        shutdown_coder_rx, done_tx.clone(),
    ));
    tokio::spawn(teammate_worker(
        "reviewer".into(), "reviewer".into(),
        "你是代码审查专家。用 write_file 把审查意见写到 review.md。".into(),
        reviewer_dir, board.clone(), client.clone(), api_key.clone(),
        shutdown_reviewer_rx, done_tx.clone(),
    ));

    // S03：共享 todo
    let todo: SharedTodo = Arc::new(Mutex::new(TodoManager::default()));

    // 主 agent 工具集（S02：HashMap dispatch）
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("todo".into(), Box::new(TodoTool { todo: todo.clone() }));
    tools.insert("add_board_task".into(), Box::new(AddBoardTaskTool { board: board.clone() }));
    tools.insert("show_board".into(), Box::new(ShowBoardTool { board: board.clone() }));
    tools.insert("dispatch_agent".into(), Box::new(DispatchAgentTool {
        client: client.clone(), api_key: api_key.clone(), cwd: main_dir,
    }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是全能 agent 协调者，拥有以下能力：\
        1. todo: 管理待办列表（先规划再执行）\
        2. add_board_task: 分发任务给 teammate（coder/reviewer/any）\
        3. show_board: 查看任务板进度\
        4. dispatch_agent: 派生子 agent 处理复杂任务\
        工作原则：复杂任务先用 todo 列计划，再逐步执行。";

    println!("=== s_full — 12 个机制综合演示 ===");
    println!("命令: '退出' | '状态' 查看任务板 | '待办' 查看 todo\n");

    // S01：agent loop + 上下文记忆
    let mut messages: Vec<Message> = vec![];

    loop {
        // S08：非阻塞检查后台完成通知
        while let Ok(msg) = done_rx.try_recv() {
            println!("\n[后台通知] {}", msg);
        }

        print!("用户> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        match input {
            // S10：shutdown 协议
            "退出" | "quit" | "exit" => {
                println!("[S10] 发送 shutdown 信号...");
                let _ = shutdown_coder_tx.send(()).await;
                let _ = shutdown_reviewer_tx.send(()).await;
                sleep(Duration::from_millis(500)).await;
                println!("已关闭。");
                break;
            }
            "状态" | "board" => {
                let tasks = board.lock().unwrap().clone();
                println!("\n── 任务板 ──");
                for t in &tasks { println!("  #{} [{}] ({}) {}", t.id, t.status, t.role_hint, t.title); }
                println!("────────────\n");
                continue;
            }
            "待办" | "todo" => {
                println!("\n── 待办 ──\n{}\n──────────\n", todo.lock().unwrap().list());
                continue;
            }
            "" => continue,
            _ => {}
        }

        messages.push(Message { role: "user".to_string(), content: json!(input) });

        // S06：上下文压缩
        maybe_compact(&client, &api_key, &mut messages).await;

        // S01：agent loop（S02：tool dispatch）
        loop {
            let resp = call_api(&client, &api_key, &messages, &tool_defs, system).await;

            if resp.stop_reason.as_deref() == Some("tool_use") {
                messages.push(Message { role: "assistant".to_string(), content: json!(resp.content) });
                let mut results = vec![];
                for block in &resp.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        println!("[工具] {}", name);
                        let r = tools[name].execute(input.clone()).await;
                        results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": r }));
                    }
                }
                messages.push(Message { role: "user".to_string(), content: json!(results) });
            } else {
                for block in &resp.content {
                    if let ContentBlock::Text { text } = block {
                        println!("\nAgent: {}", text);
                    }
                }
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: json!(resp.content.iter()
                        .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                        .collect::<Vec<_>>().join("")),
                });
                break;
            }
        }
    }
}
