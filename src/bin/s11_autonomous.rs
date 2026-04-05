// S11 Autonomous — teammate 自主扫描任务板并认领任务
//
// 核心概念：
//   - 共享任务板 TaskBoard（Arc<Mutex<Vec<Task>>>），多个 teammate 并发访问
//   - 每个 teammate 是一个 tokio task，用 loop + select! 在"轮询任务"和"等待退出"之间切换
//   - teammate 只认领与自己 role 匹配的任务（task.role_hint）
//   - 主 agent 负责往任务板上添加任务，用户也可以通过命令添加
//
// Rust 重点：Arc<Mutex<T>>、tokio::spawn、loop + select!、enum 状态机

use async_trait::async_trait;
use learn_claude_code_rust::{ContentBlock, Message, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

// ── 任务板数据结构 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    /// 待认领
    Open,
    /// 已被某 teammate 认领，正在处理
    InProgress { claimed_by: String },
    /// 已完成
    Done { result: String },
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Open => write!(f, "open"),
            TaskStatus::InProgress { claimed_by } => write!(f, "in_progress({})", claimed_by),
            TaskStatus::Done { .. } => write!(f, "done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    id: u32,
    title: String,
    /// 哪种角色适合处理（"coder" / "reviewer" / "any"）
    role_hint: String,
    status: TaskStatus,
}

/// 共享任务板
type TaskBoard = Arc<Mutex<Vec<Task>>>;

fn make_board() -> TaskBoard {
    Arc::new(Mutex::new(vec![]))
}

/// 往任务板加一个 Open 任务，返回新任务 id
fn board_add(board: &TaskBoard, title: &str, role_hint: &str) -> u32 {
    let mut tasks = board.lock().unwrap();
    let id = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    tasks.push(Task {
        id,
        title: title.to_string(),
        role_hint: role_hint.to_string(),
        status: TaskStatus::Open,
    });
    println!("[任务板] 新增任务 #{} ({}) — {}", id, role_hint, title);
    id
}

/// 认领一个 Open 且 role_hint 匹配的任务，返回认领的 Task（已设为 InProgress）
fn board_claim(board: &TaskBoard, role: &str) -> Option<Task> {
    let mut tasks = board.lock().unwrap();
    for task in tasks.iter_mut() {
        if task.status == TaskStatus::Open
            && (task.role_hint == role || task.role_hint == "any")
        {
            task.status = TaskStatus::InProgress { claimed_by: role.to_string() };
            return Some(task.clone());
        }
    }
    None
}

/// 把 InProgress 任务标为 Done
fn board_complete(board: &TaskBoard, id: u32, result: &str) {
    let mut tasks = board.lock().unwrap();
    if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
        task.status = TaskStatus::Done { result: result.to_string() };
    }
}

/// 打印任务板概览
fn board_print(board: &TaskBoard) {
    let tasks = board.lock().unwrap();
    println!("\n── 任务板 ──────────────────────────────────");
    for task in tasks.iter() {
        println!("  #{} [{}] ({}) {}", task.id, task.status, task.role_hint, task.title);
    }
    println!("────────────────────────────────────────────\n");
}

// ── API 工具类（供 teammate agent loop 使用）─────────────────────────────────

struct BashTool;
#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn definition(&self) -> Value {
        json!({
            "name": "bash",
            "description": "执行 bash 命令",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c").arg(input["command"].as_str().unwrap_or(""))
            .output().expect("failed");
        String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr)
    }
}

// ── teammate agent loop ───────────────────────────────────────────────────────

async fn run_task(
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: &str,
    task_title: &str,
) -> String {
    let tools: HashMap<String, Box<dyn Tool>> = {
        let mut m: HashMap<String, Box<dyn Tool>> = HashMap::new();
        m.insert("bash".to_string(), Box::new(BashTool));
        m
    };
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let mut messages = vec![Message {
        role: "user".to_string(),
        content: json!(task_title),
    }];

    loop {
        let response = call_api(client, api_key, &messages, &tool_defs, system_prompt).await;
        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(Message { role: "assistant".to_string(), content: json!(response.content) });
            let mut results = vec![];
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let result = tools[name].execute(input.clone()).await;
                    results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                }
            }
            messages.push(Message { role: "user".to_string(), content: json!(results) });
        } else {
            return response.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("\n");
        }
    }
}

// ── teammate 自主轮询逻辑 ─────────────────────────────────────────────────────

/// 每个 teammate 作为独立 tokio task 运行
/// - 每隔 POLL_INTERVAL 扫描任务板
/// - 发现匹配的 Open 任务就认领并处理
/// - 收到 shutdown 信号后退出
async fn teammate_worker(
    name: String,
    role: String,
    system_prompt: String,
    board: TaskBoard,
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    mut shutdown: mpsc::Receiver<()>,
) {
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    println!("[{}] 上线，开始扫描任务板 (role={})", name, role);

    loop {
        // select! 同时等待：定时器到期 OR 收到 shutdown
        tokio::select! {
            _ = sleep(POLL_INTERVAL) => {
                // 尝试认领一个任务
                if let Some(task) = board_claim(&board, &role) {
                    println!("[{}] 认领任务 #{}: {}", name, task.id, task.title);
                    let result = run_task(&client, &api_key, &system_prompt, &task.title).await;
                    board_complete(&board, task.id, &result);
                    println!("[{}] 完成任务 #{}", name, task.id);
                }
                // 没有任务时静默等待下一轮
            }
            _ = shutdown.recv() => {
                println!("[{}] 收到关闭信号，退出。", name);
                break;
            }
        }
    }
}

// ── 主 agent 工具：往任务板添加任务 ──────────────────────────────────────────

struct AddTaskTool {
    board: TaskBoard,
}
#[async_trait]
impl Tool for AddTaskTool {
    fn name(&self) -> &str { "add_task" }
    fn definition(&self) -> Value {
        json!({
            "name": "add_task",
            "description": "往共享任务板添加一个新任务。teammate 会自动认领匹配的任务。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "任务描述" },
                    "role_hint": {
                        "type": "string",
                        "description": "适合处理该任务的角色: coder / reviewer / any",
                        "enum": ["coder", "reviewer", "any"]
                    }
                },
                "required": ["title", "role_hint"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let title = input["title"].as_str().unwrap_or("untitled");
        let role_hint = input["role_hint"].as_str().unwrap_or("any");
        let id = board_add(&self.board, title, role_hint);
        format!("任务 #{} 已添加到任务板，等待 teammate 认领。", id)
    }
}

struct ShowBoardTool {
    board: TaskBoard,
}
#[async_trait]
impl Tool for ShowBoardTool {
    fn name(&self) -> &str { "show_board" }
    fn definition(&self) -> Value {
        json!({
            "name": "show_board",
            "description": "显示当前任务板上所有任务的状态。",
            "input_schema": { "type": "object", "properties": {} }
        })
    }
    async fn execute(&self, _input: Value) -> String {
        let tasks = self.board.lock().unwrap().clone();
        if tasks.is_empty() {
            return "任务板为空。".to_string();
        }
        tasks.iter().map(|t| format!("#{} [{}] ({}) {}", t.id, t.status, t.role_hint, t.title))
            .collect::<Vec<_>>().join("\n")
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY"));
    let client = Arc::new(reqwest::Client::new());
    let board = make_board();

    // 启动 teammate workers，每个有独立的 shutdown channel
    let (shutdown_coder_tx, shutdown_coder_rx) = mpsc::channel::<()>(1);
    let (shutdown_reviewer_tx, shutdown_reviewer_rx) = mpsc::channel::<()>(1);

    tokio::spawn(teammate_worker(
        "coder".to_string(),
        "coder".to_string(),
        "你是 Rust 代码生成专家。收到任务后直接给出简洁、正确的实现，并简要说明思路。".to_string(),
        board.clone(),
        client.clone(),
        api_key.clone(),
        shutdown_coder_rx,
    ));

    tokio::spawn(teammate_worker(
        "reviewer".to_string(),
        "reviewer".to_string(),
        "你是严格的代码审查专家。找出潜在 bug、性能问题和不符合 Rust 惯用法的写法。".to_string(),
        board.clone(),
        client.clone(),
        api_key.clone(),
        shutdown_reviewer_rx,
    ));

    // 主 agent 工具
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("add_task".to_string(), Box::new(AddTaskTool { board: board.clone() }));
    tools.insert("show_board".to_string(), Box::new(ShowBoardTool { board: board.clone() }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是任务协调者。\
        - 用 add_task 把用户需求拆解成小任务，指定合适的 role_hint (coder/reviewer/any)\
        - 用 show_board 查看任务进度\
        - teammate 会自动认领并完成任务，你不需要手动分配\
        把用户需求拆分得越细越好，每个任务一个职责。";

    println!("=== S11 Autonomous (输入 '退出' 结束，'状态' 查看任务板) ===");

    let mut messages: Vec<Message> = vec![];

    loop {
        print!("\n用户> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        match input {
            "退出" | "quit" | "exit" => {
                println!("正在关闭所有 teammate...");
                let _ = shutdown_coder_tx.send(()).await;
                let _ = shutdown_reviewer_tx.send(()).await;
                // 给 worker 一点时间退出
                sleep(Duration::from_millis(500)).await;
                println!("已关闭。");
                break;
            }
            "状态" | "board" => {
                board_print(&board);
                continue;
            }
            "" => continue,
            _ => {}
        }

        messages.push(Message { role: "user".to_string(), content: json!(input) });

        // 主 agent 单轮推理
        loop {
            let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;

            if response.stop_reason.as_deref() == Some("tool_use") {
                messages.push(Message { role: "assistant".to_string(), content: json!(response.content) });
                let mut results = vec![];
                for block in &response.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        println!("[主agent 调用] {}", name);
                        let result = tools[name].execute(input.clone()).await;
                        results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                    }
                }
                messages.push(Message { role: "user".to_string(), content: json!(results) });
            } else {
                for block in &response.content {
                    if let ContentBlock::Text { text } = block {
                        println!("\nAgent: {}", text);
                    }
                }
                break;
            }
        }
    }
}

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[Message],
    tools: &[Value],
    system: &str,
) -> learn_claude_code_rust::ApiResponse {
    let request = learn_claude_code_rust::ApiRequest {
        model: "claude-haiku-4-5-20251001".to_string(),
        max_tokens: 8096,
        system: system.to_string(),
        messages: messages.to_vec(),
        tools: Some(tools.to_vec()),
    };
    client.post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request).send().await.unwrap()
        .json::<learn_claude_code_rust::ApiResponse>().await.unwrap()
}
