use async_trait::async_trait;
use learn_claude_code_rust::Tool;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ── 后台任务状态 ────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum BgStatus {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone)]
struct BgTask {
    id: String,
    command: String,
    status: BgStatus,
}

type BgStore = Arc<Mutex<HashMap<String, BgTask>>>;
// 完成通知: (task_id, output, is_error)
type CompletionTx = mpsc::UnboundedSender<(String, String, bool)>;

// ── run_background 工具 ─────────────────────────────────────────

struct RunBackgroundTool {
    store: BgStore,
    tx: CompletionTx,
    next_id: Arc<Mutex<u32>>,
}

#[async_trait]
impl Tool for RunBackgroundTool {
    fn name(&self) -> &str { "run_background" }
    fn definition(&self) -> Value {
        json!({
            "name": "run_background",
            "description": "在后台异步执行 shell 命令，立即返回任务 id，不阻塞 agent",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let command = input["command"].as_str().unwrap_or("").to_string();

        let id = {
            let mut n = self.next_id.lock().unwrap();
            let id = format!("bg-{}", n);
            *n += 1;
            id
        };

        // 注册为 Running
        self.store.lock().unwrap().insert(id.clone(), BgTask {
            id: id.clone(),
            command: command.clone(),
            status: BgStatus::Running,
        });

        // tokio::spawn：后台跑，主线程立即返回
        let store = self.store.clone();
        let tx = self.tx.clone();
        let id2 = id.clone();
        tokio::spawn(async move {
            let out = tokio::process::Command::new("sh")
                .arg("-c").arg(&command)
                .output().await;

            let (result, is_error) = match out {
                Ok(o) if o.status.success() =>
                    (String::from_utf8_lossy(&o.stdout).into_owned(), false),
                Ok(o) =>
                    (String::from_utf8_lossy(&o.stderr).into_owned(), true),
                Err(e) => (e.to_string(), true),
            };

            let status = if is_error {
                BgStatus::Failed(result.clone())
            } else {
                BgStatus::Done(result.clone())
            };
            store.lock().unwrap().entry(id2.clone()).and_modify(|t| t.status = status);
            let _ = tx.send((id2, result, is_error));
        });

        format!("后台任务已启动，id={}", id)
    }
}

// ── check_background 工具 ───────────────────────────────────────

struct CheckBackgroundTool(BgStore);

#[async_trait]
impl Tool for CheckBackgroundTool {
    fn name(&self) -> &str { "check_background" }
    fn definition(&self) -> Value {
        json!({
            "name": "check_background",
            "description": "查看某个后台任务的状态和输出",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let id = input["id"].as_str().unwrap_or("");
        let store = self.0.lock().unwrap();
        match store.get(id) {
            None => format!("找不到任务 {}", id),
            Some(t) => match &t.status {
                BgStatus::Running =>
                    format!("[{}] {} | 运行中...", id, t.command),
                BgStatus::Done(out) =>
                    format!("[{}] {} | 完成\n{}", id, t.command, out),
                BgStatus::Failed(err) =>
                    format!("[{}] {} | 失败\n{}", id, t.command, err),
            }
        }
    }
}

// ── list_background 工具 ────────────────────────────────────────

struct ListBackgroundTool(BgStore);

#[async_trait]
impl Tool for ListBackgroundTool {
    fn name(&self) -> &str { "list_background" }
    fn definition(&self) -> Value {
        json!({
            "name": "list_background",
            "description": "列出所有后台任务及当前状态",
            "input_schema": { "type": "object", "properties": {} }
        })
    }
    async fn execute(&self, _input: Value) -> String {
        let store = self.0.lock().unwrap();
        if store.is_empty() { return "暂无后台任务".to_string(); }
        let mut lines: Vec<String> = store.values().map(|t| {
            let s = match &t.status {
                BgStatus::Running   => "运行中",
                BgStatus::Done(_)   => "完成",
                BgStatus::Failed(_) => "失败",
            };
            format!("[{}] {} | {}", t.id, t.command, s)
        }).collect();
        lines.sort();
        lines.join("\n")
    }
}

// ── API ─────────────────────────────────────────────────────────

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[learn_claude_code_rust::Message],
    tools: &[Value],
    system: &str,
) -> learn_claude_code_rust::ApiResponse {
    let req = learn_claude_code_rust::ApiRequest {
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
        .json(&req)
        .send().await.unwrap()
        .json::<learn_claude_code_rust::ApiResponse>().await.unwrap()
}

// ── main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, String, bool)>();
    let next_id = Arc::new(Mutex::new(1u32));

    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("run_background".to_string(), Box::new(RunBackgroundTool {
        store: store.clone(), tx, next_id,
    }));
    tools.insert("check_background".to_string(), Box::new(CheckBackgroundTool(store.clone())));
    tools.insert("list_background".to_string(),  Box::new(ListBackgroundTool(store.clone())));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();
    let system = "你是支持后台任务的 agent。用 run_background 启动耗时命令并告知用户任务 id；可继续对话；用 check_background 或 list_background 查询进度。";

    println!("=== S08 Background Tasks ===");
    println!("输入 exit 退出\n");

    let mut messages: Vec<learn_claude_code_rust::Message> = vec![];

    loop {
        // 非阻塞：打印所有已完成的后台通知
        while let Ok((id, out, err)) = rx.try_recv() {
            let label = if err { "失败" } else { "完成" };
            println!("\n[后台通知] {} {} | {}",
                id, label, out.lines().next().unwrap_or(""));
        }

        print!("> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut line = String::new();
        stdin().read_line(&mut line).unwrap();
        let line = line.trim().to_string();
        if line.is_empty() { continue; }
        if line == "exit" || line == "quit" { break; }

        messages.push(learn_claude_code_rust::Message {
            role: "user".to_string(),
            content: json!(line),
        });

        // agent loop（工具调用直到 end_turn）
        loop {
            let resp = call_api(&client, &api_key, &messages, &tool_defs, system).await;

            if resp.stop_reason.as_deref() == Some("tool_use") {
                messages.push(learn_claude_code_rust::Message {
                    role: "assistant".to_string(),
                    content: json!(resp.content),
                });
                let mut results = vec![];
                for block in &resp.content {
                    if let learn_claude_code_rust::ContentBlock::ToolUse { id, name, input } = block {
                        println!("[工具] {}", name);
                        let result = match tools.get(name.as_str()) {
                            Some(t) => t.execute(input.clone()).await,
                            None => format!("未知工具: {}", name),
                        };
                        println!("    -> {}", result.lines().next().unwrap_or(""));
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": result
                        }));
                    }
                }
                messages.push(learn_claude_code_rust::Message {
                    role: "user".to_string(),
                    content: json!(results),
                });
            } else {
                println!();
                for block in &resp.content {
                    if let learn_claude_code_rust::ContentBlock::Text { text } = block {
                        println!("Claude: {}", text);
                    }
                }
                break;
            }
        }
    }
}
