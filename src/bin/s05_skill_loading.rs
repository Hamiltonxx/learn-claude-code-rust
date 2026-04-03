use async_trait::async_trait;
use learn_claude_code_rust::{ApiRequest, ApiResponse, ContentBlock, Message, Tool};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};

// SkillLoaderTool: 按需加载 skills/ 下的 .md 文件

struct SkillLoaderTool;

#[async_trait]
impl Tool for SkillLoaderTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn definition(&self) -> Value {
        json!({
            "name": "load_skill",
            "description": "加载指定技能的知识文档。当你需要某个领域的专业知识时调用此工具。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "技能名称，对应 skills/ 目录下的子目录名，例如 'agent-builder'、'code-review'"
                    }
                },
                "required": ["skill_name"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let skill_name = input["skill_name"].as_str().unwrap_or("");
        let path = format!("skills/{}/SKILL.md", skill_name);

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                println!(" [加载技能] {} ({} 字节)", skill_name, content.len());
                content
            }
            Err(_) => {
                // 列出可用 skill 供模型参考
                let available = list_skills();
                format!("技能 '{}' 不存在。可用技能: {}", skill_name, available.join(", "))
            }
        }
    }  
}


// 列出 skills/ 下的所有子目录名
fn list_skills() -> Vec<String> {
    std::fs::read_dir("skills").into_iter().flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

// BashTool (复用，方便 agent 做实际操作)

struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

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
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(input["command"].as_str().unwrap_or(""))
            .output()
            .expect("failed");
        String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr)
    }
}

// agent loop

async fn agent_loop(
    client: &reqwest::Client,
    api_key: &str,
    system: &str,
    tools: &HashMap<String, Box<dyn Tool>>,
    tool_defs: &[Value],
    initial_message: &str,
) {
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: json!(initial_message),
    }];

    loop {
        let response = call_api(client, api_key, &messages, tool_defs, system).await;

        match response.stop_reason.as_deref() {
            Some("tool_use") => {
                // 1. 把 assistant 的 tool_use 加入历史
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: json!(response.content),
                });

                // 2. 执行每个工具，收集结果
                let mut results = vec![];
                for block in &response.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        println!("[工具调用] {}", name);
                        let result = tools[name].execute(input.clone()).await;
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": result
                        }));
                    }
                }

                // 3. 把 tool_result 加入历史，继续循环
                messages.push(Message {
                    role: "user".to_string(),
                    content: json!(results),
                });
            }
            _ => {
                // end_turn: 输出最终文本
                for block in &response.content {
                    if let ContentBlock::Text { text } = block {
                        println!("\nClaude: {}", text);
                    }
                }
                break;
            }
        }
    }
}

// --- main ---

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("load_skill".to_string(), Box::new(SkillLoaderTool));
    tools.insert("bash".to_string(), Box::new(BashTool));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    // system prompt 告知模型有哪些可用技能
    let available = list_skills().join(", ");
    let system = format!("你是一个知识丰富的助手。当你需要特定领域的专业知识时，用 load_skill 工具加载对应技能文档。\n可用技能: {}", available);

    loop {
        print!("\n> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        let input = input.trim();
        if input == "exit" { break; }

        agent_loop(&client, &api_key, &system, &tools, &tool_defs, input).await;
    }
}

// call_api (同前几个 session)

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[Message],
    tools: &[Value],
    system: &str,
) -> ApiResponse {
    let request = ApiRequest {
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
        .json(&request)
        .send()
        .await
        .unwrap()
        .json::<ApiResponse>()
        .await
        .unwrap()
}
