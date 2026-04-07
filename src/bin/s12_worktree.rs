// S12 Worktree Isolation — 每个 teammate 在独立 CWD 下操作，防止互相干扰
//
// 核心概念：
//   - 在 s11 基础上，给每个 teammate_worker 分配独立的工作目录（cwd）
//   - BashTool 通过 .current_dir(&cwd) 把命令限定在各自目录
//   - 主 agent 也有自己的 cwd（项目根目录）
//   - 退出时清理临时目录
//
// Rust 重点：PathBuf 传参、Command::current_dir、临时目录管理

use async_trait::async_trait;
use learn_claude_code_rust::{ContentBlock, Message, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

// ── 任务板（与 s11 相同）────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Open,
    InProgress { claimed_by: String },
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
    role_hint: String,
    status: TaskStatus,
}

type TaskBoard = Arc<Mutex<Vec<Task>>>;

fn make_board() -> TaskBoard {
    Arc::new(Mutex::new(vec![]))
}

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

fn board_complete(board: &TaskBoard, id: u32, result: &str) {
    let mut tasks = board.lock().unwrap();
    if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
        task.status = TaskStatus::Done { result: result.to_string() };
    }
}

fn board_print(board: &TaskBoard) {
    let tasks = board.lock().unwrap();
    println!("\n── 任务板 ──────────────────────────────────");
    for task in tasks.iter() {
        println!("  #{} [{}] ({}) {}", task.id, task.status, task.role_hint, task.title);
    }
    println!("────────────────────────────────────────────\n");
}

// ── 带 cwd 的工具 ─────────────────────────────────────────────────────────────

/// BashTool：在指定目录下执行命令
struct BashTool {
    cwd: PathBuf,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn definition(&self) -> Value {
        json!({
            "name": "bash",
            "description": "在 teammate 的隔离工作目录下执行 bash 命令",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let cmd = input["command"].as_str().unwrap_or("");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.cwd)  // 关键：限定在各自的工作目录
            .output();
        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                if stderr.is_empty() { stdout } else { format!("{}{}", stdout, stderr) }
            }
            Err(e) => format!("执行失败: {}", e),
        }
    }
}

/// WriteFileTool：在 cwd 下写文件
struct WriteFileTool {
    cwd: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "write_file",
            "description": "在 teammate 的隔离目录下写入文件",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对于工作目录的文件路径" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let rel_path = input["path"].as_str().unwrap_or("output.txt");
        let content = input["content"].as_str().unwrap_or("");
        let full_path = self.cwd.join(rel_path);
        match std::fs::write(&full_path, content) {
            Ok(_) => format!("已写入 {}", full_path.display()),
            Err(e) => format!("写入失败: {}", e),
        }
    }
}

// ── teammate agent loop ───────────────────────────────────────────────────────

async fn run_task(
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: &str,
    task_title: &str,
    cwd: &PathBuf,
) -> String {
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("bash".to_string(), Box::new(BashTool { cwd: cwd.clone() }));
    tools.insert("write_file".to_string(), Box::new(WriteFileTool { cwd: cwd.clone() }));
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

// ── teammate 自主轮询（带 cwd 隔离）─────────────────────────────────────────

async fn teammate_worker(
    name: String,
    role: String,
    system_prompt: String,
    cwd: PathBuf,  // 独立工作目录
    board: TaskBoard,
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    mut shutdown: mpsc::Receiver<()>,
) {
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    println!("[{}] 上线，工作目录: {}", name, cwd.display());

    loop {
        tokio::select! {
            _ = sleep(POLL_INTERVAL) => {
                if let Some(task) = board_claim(&board, &role) {
                    println!("[{}] 认领任务 #{}: {}", name, task.id, task.title);
                    let result = run_task(&client, &api_key, &system_prompt, &task.title, &cwd).await;
                    board_complete(&board, task.id, &result);
                    println!("[{}] 完成任务 #{}\n结果: {}", name, task.id, &result[..result.len().min(100)]);
                }
            }
            _ = shutdown.recv() => {
                println!("[{}] 收到关闭信号，退出。", name);
                break;
            }
        }
    }
}

// ── 主 agent 工具 ─────────────────────────────────────────────────────────────

struct AddTaskTool { board: TaskBoard }
#[async_trait]
impl Tool for AddTaskTool {
    fn name(&self) -> &str { "add_task" }
    fn definition(&self) -> Value {
        json!({
            "name": "add_task",
            "description": "往共享任务板添加一个新任务",
            "input_schema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "role_hint": { "type": "string", "enum": ["coder", "reviewer", "any"] }
                },
                "required": ["title", "role_hint"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let id = board_add(
            &self.board,
            input["title"].as_str().unwrap_or("untitled"),
            input["role_hint"].as_str().unwrap_or("any"),
        );
        format!("任务 #{} 已添加，等待 teammate 认领。", id)
    }
}

struct ShowBoardTool { board: TaskBoard }
#[async_trait]
impl Tool for ShowBoardTool {
    fn name(&self) -> &str { "show_board" }
    fn definition(&self) -> Value {
        json!({ "name": "show_board", "description": "显示任务板", "input_schema": { "type": "object", "properties": {} } })
    }
    async fn execute(&self, _: Value) -> String {
        let tasks = self.board.lock().unwrap().clone();
        if tasks.is_empty() { return "任务板为空。".to_string(); }
        tasks.iter()
            .map(|t| format!("#{} [{}] ({}) {}", t.id, t.status, t.role_hint, t.title))
            .collect::<Vec<_>>().join("\n")
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY"));
    let client = Arc::new(reqwest::Client::new());
    let board = make_board();

    // 为每个 teammate 创建隔离的临时工作目录
    let base_dir = std::env::temp_dir().join("s12_worktree");
    let coder_dir = base_dir.join("coder");
    let reviewer_dir = base_dir.join("reviewer");
    std::fs::create_dir_all(&coder_dir).unwrap();
    std::fs::create_dir_all(&reviewer_dir).unwrap();
    println!("[初始化] coder  工作目录: {}", coder_dir.display());
    println!("[初始化] reviewer 工作目录: {}", reviewer_dir.display());

    let (shutdown_coder_tx, shutdown_coder_rx) = mpsc::channel::<()>(1);
    let (shutdown_reviewer_tx, shutdown_reviewer_rx) = mpsc::channel::<()>(1);

    tokio::spawn(teammate_worker(
        "coder".to_string(),
        "coder".to_string(),
        "你是 Rust 代码生成专家。把生成的代码用 write_file 工具写到当前目录，文件名用 .rs 结尾。".to_string(),
        coder_dir.clone(),
        board.clone(),
        client.clone(),
        api_key.clone(),
        shutdown_coder_rx,
    ));

    tokio::spawn(teammate_worker(
        "reviewer".to_string(),
        "reviewer".to_string(),
        "你是代码审查专家。把审查意见用 write_file 工具写到 review.md，指出潜在问题。".to_string(),
        reviewer_dir.clone(),
        board.clone(),
        client.clone(),
        api_key.clone(),
        shutdown_reviewer_rx,
    ));

    // 主 agent
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("add_task".to_string(), Box::new(AddTaskTool { board: board.clone() }));
    tools.insert("show_board".to_string(), Box::new(ShowBoardTool { board: board.clone() }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是任务协调者。用 add_task 把用户需求拆成小任务，用 show_board 查看进度。\
        coder 负责写代码，reviewer 负责审查，any 两者都可以认领。";

    println!("\n=== S12 Worktree Isolation ===");
    println!("命令: '退出' 结束 | '状态' 查看任务板 | '目录' 查看 teammate 工作目录\n");

    let mut messages: Vec<Message> = vec![];

    loop {
        print!("用户> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        match input {
            "退出" | "quit" | "exit" => {
                println!("正在关闭 teammate...");
                let _ = shutdown_coder_tx.send(()).await;
                let _ = shutdown_reviewer_tx.send(()).await;
                sleep(Duration::from_millis(500)).await;
                // 打印各 teammate 目录内容
                println!("\n── coder 工作目录内容 ──");
                let _ = std::process::Command::new("ls").arg("-la").arg(&coder_dir).status();
                println!("\n── reviewer 工作目录内容 ──");
                let _ = std::process::Command::new("ls").arg("-la").arg(&reviewer_dir).status();
                println!("\n已关闭。工作目录保留在 {}", base_dir.display());
                break;
            }
            "状态" | "board" => { board_print(&board); continue; }
            "目录" | "dirs" => {
                println!("coder    → {}", coder_dir.display());
                println!("reviewer → {}", reviewer_dir.display());
                continue;
            }
            "" => continue,
            _ => {}
        }

        messages.push(Message { role: "user".to_string(), content: json!(input) });

        loop {
            let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;
            if response.stop_reason.as_deref() == Some("tool_use") {
                messages.push(Message { role: "assistant".to_string(), content: json!(response.content) });
                let mut results = vec![];
                for block in &response.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        println!("[主agent] 调用 {}", name);
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
