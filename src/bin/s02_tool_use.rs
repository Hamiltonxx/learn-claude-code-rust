use async_trait::async_trait;
use learn_claude_code_rust::Tool;
use serde_json::{json, Value};
use std::process::Command;

struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn definition(&self) -> Value {
        json!({
            "name": "bash",
            "description": "Run a bash command",
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
        let command = input["command"].as_str().unwrap_or("");
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("failed to execute");
        String::from_utf8_lossy(&output.stdout).to_string()
            + &String::from_utf8_lossy(&output.stderr)
    }
}

struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn definition(&self) -> Value {
        json!({
            "name": "read_file",
            "description": "Read the contents of a file",
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
        let path = input["path"].as_str().unwrap_or("");
        std::fs::read_to_string(path).unwrap_or_else(|e| e.to_string())
    }
}

struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }

    fn definition(&self) -> Value {
        json!({
            "name": "write_file",
            "description": "Write content to a file",
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
        let path = input["path"].as_str().unwrap_or("");
        let content = input["content"].as_str().unwrap_or("");
        std::fs::write(path, content).map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string())
    }
}

struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }

    fn definition(&self) -> Value {
        json!({
            "name": "edit_file",
            "description": "Replace a string in a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" }
                },
                "required": ["path", "old_str", "new_str"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let path = input["path"].as_str().unwrap_or("");
        let old = input["old_str"].as_str().unwrap_or("");
        let new = input["new_str"].as_str().unwrap_or("");
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let updated = text.replace(old, new);
                std::fs::write(path, updated).map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string())
            }
            Err(e) => e.to_string(),
        }
    }
}

use std::collections::HashMap;
use std::io::{Write, stdin, stdout};

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    // 构建 dispatcher
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("bash".to_string(), Box::new(BashTool));
    tools.insert("read_file".to_string(), Box::new(ReadFileTool));
    tools.insert("write_file".to_string(), Box::new(WriteFileTool));
    tools.insert("edit_file".to_string(), Box::new(EditFileTool));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    // 读用户输入
    print!("> ");
    Write::flush(&mut stdout()).unwrap();
    let mut input = String::new();
    stdin().read_line(&mut input).unwrap();

    let mut messages = vec![learn_claude_code_rust::Message {
        role: "user".to_string(),
        content: serde_json::json!(input.trim()),
    }];

    // agent loop
    loop {
        let response = call_api(&client, &api_key, &messages, &tool_defs).await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            // 1. 把 assistant 回复加入历史
            messages.push(learn_claude_code_rust::Message {
                role: "assistant".to_string(),
                content: serde_json::json!(response.content),
            });

            // 2. 执行所有 tool_use, 收集结果
            let mut tool_results = vec![];
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::ToolUse { id, name, input } = block {
                    println!("[调用工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    tool_results.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": result
                    }));
                }
            }

            // 3. 把工具结果作为 user 消息加入历史
            messages.push(learn_claude_code_rust::Message {
                role: "user".to_string(),
                content: serde_json::json!(tool_results),
            });
        } else {
            // end_turn, 打印文本回复
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::Text { text } = block {
                    println!("\nClaude: {}", text);
                }
            }
            break;
        }
    }
}

async fn call_api(client: &reqwest::Client, api_key: &str, messages: &[learn_claude_code_rust::Message], tools: &[Value],) -> learn_claude_code_rust::ApiResponse {
    let request = learn_claude_code_rust::ApiRequest {
        model: "claude-haiku-4-5".to_string(),
        max_tokens: 8096,
        system: "You are a helpful assistant.".to_string(),
        messages: messages.to_vec(),
        tools: Some(tools.to_vec()),
    };

    client
        .post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .unwrap()
        .json::<learn_claude_code_rust::ApiResponse>()
        .await
        .unwrap()
}
