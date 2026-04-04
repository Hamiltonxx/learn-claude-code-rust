use async_trait::async_trait;
use learn_claude_code_rust::{ContentBlock, Message, Tool};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::sync::Arc;

// --- Teammate 定义 ---

struct Teammate {
    name: String,
    role: String,
    system_prompt: String,
}

// --- 基础工具 (供 teammate 使用) ---

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

// --- teammate 的 agent_loop ---
// 与 s04 的 agent_loop 一致，区别是接收 system_prompt 感受
// messages 是局部变量，完全隔离

async fn teammate_loop(
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: &str,
    task: &str,
    tools: &HashMap<String, Box<dyn Tool>>,
    tool_defs: &[Value],
) -> String {
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: json!(task),
    }];

    loop {
        let response = call_api(client, api_key, &messages, tool_defs, system_prompt).await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(Message {
                role: "assistant".to_string(),
                content: json!(response.content),
            });

            let mut results = vec![];
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    println!("  [teammate 工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": result
                    }));
                }
            }

            messages.push(Message {
                role: "user".to_string(),
                content: json!(results),
            });
        } else {
            return response.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
}


// --- SendMessageTool ---
// 主 agent 用这个 tool 把任务发给指定的 teammate
// teammate 有自己的 role (system_prompt), 完全独立运行

struct SendMessageTool {
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    // 用 Arc 共享 teammate 注册表
    teammates: Arc<HashMap<String, Teammate>>,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str { "send_message" }

    fn definition(&self) -> Value {
        json!({
            "name": "send_message",
            "description": "把任务发给指定的队友(teammate)。队友有自己的专长，会独立完成任务并返回结果。可用队友: coder(代码生成), reviewer(代码审查)",
            "input_schema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "队友名字: coder 或 reviewer"
                    },
                    "message": {
                        "type": "string",
                        "description": "交给队友的任务描述"
                    }
                },
                "required": ["to", "message"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let to = input["to"].as_str().unwrap_or("");
        let message = input["message"].as_str().unwrap_or("");

        let Some(teammate) = self.teammates.get(to) else {
            return format!("未找到队友: {}", to);
        };

        println!("[主agent -> {}({})] {}", teammate.name, teammate.role, message);

        // teammate 的工具集 (根据角色可以不同，这里都给 bash)
        let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
        tools.insert("bash".to_string(), Box::new(BashTool));
        let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

        let result = teammate_loop(
            &self.client,
            &self.api_key,
            &teammate.system_prompt,
            message,
            &tools,
            &tool_defs,
        ).await;

        println!("[{} -> 主agent] 完成", teammate.name);
        result
    }
}

// --- main ---

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY"));
    let client = Arc::new(reqwest::Client::new());

    // 注册 teammates
    let mut teammate_map = HashMap::new();
    teammate_map.insert("coder".to_string(), Teammate {
        name: "coder".to_string(),
        role: "代码生成专家".to_string(),
        system_prompt: "你是一个 Rust 代码生成专家。收到需求后，直接给出简洁、正确的 Rust代码实现，并简要说明思路。".to_string(),
    });
    teammate_map.insert("reviewer".to_string(), Teammate {
        name: "reviewer".to_string(),
        role: "代码审查专家".to_string(),
        system_prompt: "你是一个严格的代码审查专家。收到代码后，找出潜在的 bug、性能问题、不符合 Rust 惯用法的写法，给出具体的改进建议。".to_string(),
    });

    let teammates = Arc::new(teammate_map);

    // 主 agent 只有 send_message 工具
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("send_message".to_string(), Box::new(SendMessageTool {
        client: client.clone(),
        api_key: api_key.clone(),
        teammates: teammates.clone(),
    }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是一个技术团队的协调者。你有两个队友：\
        - coder: 负责写代码\
        - reviewer: 负责审核代码\
        收到任务后，先让 coder 实现，再把代码发给 reviewer 审查，最后综合两人的结果给出完整回答。";

    print!("任务> ");
    IoWrite::flush(&mut stdout()).unwrap();
    let mut input = String::new();
    stdin().read_line(&mut input).unwrap();

    let mut messages = vec![Message {
        role: "user".to_string(),
        content: json!(input.trim()),
    }];

    loop {
        let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(Message {
                role: "assistant".to_string(),
                content: json!(response.content),
            });

            let mut results = vec![];
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    println!("[主agent 调用工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": result
                    }));
                }
            }

            messages.push(Message {
                role: "user".to_string(),
                content: json!(results),
            });
        } else {
            for block in &response.content {
                if let ContentBlock::Text { text } = block {
                    println!("\nClaude: {}", text);
                }
            }
            break;
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
