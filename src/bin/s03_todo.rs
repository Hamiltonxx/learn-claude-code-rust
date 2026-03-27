use async_trait::async_trait;
use learn_claude_code_rust::Tool;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write, stdin, stdout};
use std::process::Command;
use std::sync::{Arc, Mutex};

// --- 数据结构 ---

#[derive(Debug, Clone)]
struct TodoItem {
    id: u32,
    title: String,
    status: String,
}

struct TodoManager {
    todos: Vec<TodoItem>,
    next_id: u32,
}

impl TodoManager {
    fn new() -> Self {
        TodoManager { todos: vec![], next_id: 1 }
    }

    fn add(&mut self, title: &str) -> u32 {
        let id = self.next_id;
        self.todos.push(TodoItem { id, title: title.to_string(), status: "pending".to_string() });
        self.next_id += 1;
        id
    }

    fn list(&self) -> String {
        if self.todos.is_empty() { return " (暂无任务) ".to_string(); }
        self.todos.iter().map(|t| {
            let icon = match t.status.as_str() {
                "done" => "✅",
                "in_progress" => "🔄",
                _ => "⬜"
            };
            format!("{} [{}] {}", icon, t.id, t.title)
        })
        .collect::<Vec<_>>().join("\n")
    }

    fn update(&mut self, id: u32, status: &str) -> String {
        match self.todos.iter_mut().find(|t| t.id == id) {
            Some(todo) => { todo.status = status.to_string(); format!("任务 #{} -> {}", id, status) }
            None => format!("找不到任务 #{}", id),
        }
    }

    fn delete(&mut self, id: u32) -> String {
        let before = self.todos.len();
        self.todos.retain(|t| t.id != id);
        if self.todos.len() < before {
            format!("已删除 #{}", id)
        } else {
            format!("找不到 #{}", id)
        }
    }
}

// --- TodoWriteTool ---

struct TodoWriteTool {
    manager: Arc<Mutex<TodoManager>>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str { "todo_write" }

    fn definition(&self) -> Value {
        json!({
            "name": "todo_write",
            "description": "管理任务列表。执行多步骤任务时，必须先add所有子任务，再逐步执行并更新状态。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "list", "update", "delete"] },
                    "title":  { "type": "string" },
                    "id":     { "type": "integer" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "done"] }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let mut mgr = self.manager.lock().unwrap();
        match input["action"].as_str().unwrap_or("") {
            "add" => { let t = input["title"].as_str().unwrap_or("未命名"); format!("已添加 #{}: {}", mgr.add(t), t)}
            "list" => mgr.list(),
            "update" => mgr.update(input["id"].as_u64().unwrap_or(0) as u32, input["status"].as_str().unwrap_or("pending")),
            "delete" => mgr.delete(input["id"].as_u64().unwrap_or(0) as u32),
            other => format!("未知操作: {}", other),
        }
    }
}

// --- BashTool / WriteFileTool (直接从 s02 照抄) ---

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
        let out = Command::new("sh").arg("-c").arg(input["command"].as_str().unwrap_or("")).output().expect("failed");
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
    }
}

struct WriteFileTool;
#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "write_file",
            "description": "写入文件",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        std::fs::write(input["path"].as_str().unwrap_or(""), input["content"].as_str().unwrap_or(""))
            .map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string())
    }
}

// --- main ---

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    let manager = Arc::new(Mutex::new(TodoManager::new()));

    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("todo_write".to_string(), Box::new(TodoWriteTool { manager }));
    tools.insert("bash".to_string(),       Box::new(BashTool));
    tools.insert("write_file".to_string(), Box::new(WriteFileTool));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是一个有条理的助手。执行任何多步骤任务时必须：
        1. 先用 todo_write(add) 把所有子任务列出
        2. 开始某步前用 todo_write(update, in_progress) 标记
        3. 完成后用 todo_write(update, done) 标记
        不允许跳过规划直接执行。
    ";

    print!("> ");
    Write::flush(&mut stdout()).unwrap();
    let mut input = String::new();
    stdin().read_line(&mut input).unwrap();

    let mut messages = vec![learn_claude_code_rust::Message {
        role: "user".to_string(),
        content: json!(input.trim()),
    }];

    loop {
        let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(learn_claude_code_rust::Message {
                role: "assistant".to_string(),
                content: json!(response.content),
            });

            let mut results = vec![];
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::ToolUse { id, name, input } = block {
                    println!("[工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    println!("[结果] {}", result);
                    results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                }
            }

            messages.push(learn_claude_code_rust::Message {
                role: "user".to_string(),
                content: json!(results),
            });
        } else {
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::Text { text } = block {
                    println!("\nClaude: {}", text);
                }
            }
            break;
        }
    }
}

async fn call_api(
    client: &reqwest::Client, api_key: &str,
    messages: &[learn_claude_code_rust::Message], tools: &[Value], system: &str,
) -> learn_claude_code_rust::ApiResponse {
    let request = learn_claude_code_rust::ApiRequest {
        model: "claude-haiku-4-5".to_string(),
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
