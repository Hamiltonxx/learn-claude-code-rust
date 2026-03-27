use async_trait::async_trait;
use learn_claude_code_rust::Tool;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::process::Command;
use std::sync::Arc;

// --- 基础工具 (照抄 s03) ---

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
                "properties": {
                    "command": { "type": "string" }
                },
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

struct ReadFileTool;
#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "read_file",
            "description": "读取文件",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        std::fs::read_to_string(input["path"].as_str().unwrap_or("")).unwrap_or_else(|e| e.to_string())
    }
}

// --- agent_loop: 子 agent 的独立循环 ---
//
// 关键: messages 是局部变量，和调用方完全隔离
// 返回子 agent 的最终文本输出

async fn agent_loop(
    client: &reqwest::Client,
    api_key: &str,
    task: &str,
    tools: &HashMap<String, Box<dyn Tool>>,
    tool_defs: &[Value],
) -> String {
    let mut messages = vec![learn_claude_code_rust::Message {
        role: "user".to_string(),
        content: json!(task),
    }];

    loop {
        let response = call_api(client, api_key, &messages, tool_defs, "你是一个能干的助手。").await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(learn_claude_code_rust::Message {
                role: "assistant".to_string(),
                content: json!(response.content),
            });

            let mut results = vec![];
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::ToolUse{ id, name, input } = block {
                    println!(" [子agent工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                }
            }

            messages.push(learn_claude_code_rust::Message {
                role: "user".to_string(),
                content: json!(results),
            });
        } else {
            // 收集子 agent 的文本输出并返回
            return response.content.iter().filter_map(|b| if let learn_claude_code_rust::ContentBlock::Text {text} = b {
                Some(text.clone())
            } else { None })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
}

// --- DispatchAgentTool ---

struct DispatchAgentTool {
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
}

#[async_trait]
impl Tool for DispatchAgentTool {
    fn name(&self) -> &str { "dispatch_agent" }

    fn definition(&self) -> Value {
        json!({
            "name": "dispatch_agent",
            "description": "派生一个子agent来完成独立的子任务。子agent有自己的对话历史，完成后返回结果。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "交给子agent的任务描述" }
                },
                "required": ["task"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let task = input["task"].as_str().unwrap_or("").to_string();
        println!("[派生子agent]任务: {}", task);

        // 子agent有自己独立的工具集
        let mut sub_tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
        sub_tools.insert("bash".to_string(),        Box::new(BashTool));
        sub_tools.insert("write_file".to_string(),  Box::new(WriteFileTool));
        sub_tools.insert("read_file".to_string(),   Box::new(ReadFileTool));

        let sub_tool_defs: Vec<Value> = sub_tools.values().map(|t| t.definition()).collect();

        // 调用独立的 agent_loop, messages 完全隔离
        let result = agent_loop(&self.client, &self.api_key, &task, &sub_tools, &sub_tool_defs).await;
        println!("[子agent完成]");
        result
    }
}

// --- main ---

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set"));
    let client = Arc::new(reqwest::Client::new());

    // 主 agent 只有 dispatch_agent 工具
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("dispatch_agent".to_string(), Box::new(DispatchAgentTool {
        client: client.clone(),
        api_key: api_key.clone(),
    }));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是一个任务协调者。收到复杂任务时，把它拆成若干独立子任务，用 dispatch_agent 工具逐一派发执行。";

    print!("> ");
    IoWrite::flush(&mut stdout()).unwrap();
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
                    println!("[主agent工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
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
