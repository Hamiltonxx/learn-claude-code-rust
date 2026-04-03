use async_trait::async_trait;
use learn_claude_code_rust::{ApiRequest, ApiResponse, ContentBlock, Message, Tool};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{{Write as IoWrite, stdin, stdout}};

const COMPACT_THRESHOLD: usize = 20;  // 超过这个数触发压缩
const KEEP_RECENT: usize = 10;        // 保留最近 N 条原文

// SkillLoaderTool (同 S05)

struct SkillLoaderTool;

#[async_trait]
impl Tool for SkillLoaderTool {
    fn name(&self) -> &str { "load_skill" }
    
    fn definition(&self) -> Value {
        json!({
            "name": "load_skill",
            "description": "加载指定技能的知识文档。当你需要某个领域的专业知识时调用此工具。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "技能名称，对应 skills/ 目录下的子目录名"
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
                let available = list_skills();
                format!("技能 '{}' 不存在。可用技能: {}", skill_name, available.join(", "))
            }
        }
    }
}

fn list_skills() -> Vec<String> {
    std::fs::read_dir("skills").into_iter().flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

// BashTool (同 S05)

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
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(input["command"].as_str().unwrap_or(""))
            .output()
            .expect("failed");
        String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr)
    }
}

// S06 新增: 上下文压缩

// 把一批消息的内容拼成文本，发给模型生成摘要
async fn summarize(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[Message],
) -> String {
    let text = messages.iter().map(|m| {
        format!("{}: {}", m.role, m.content)
    }).collect::<Vec<_>>().join("\n");

    let req = ApiRequest {
        model: "claude-haiku-4-5".to_string(),
        max_tokens: 1024,
        system: "请将以下对话历史压缩成简洁摘要，保留关键信息、已完成的操作和重要结论。".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: json!(text),
        }],
        tools: None,
    };

    let resp = client.post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&req)
        .send().await.unwrap()
        .json::<ApiResponse>().await.unwrap();

    resp.content.iter().filter_map(|b| {
        if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None }
    }).collect::<Vec<_>>().join("\n")
}

// 每轮循环前检查，超过阈值则压缩
async fn maybe_compact(
    client: &reqwest::Client,
    api_key: &str,
    messages: &mut Vec<Message>,
) {
    if messages.len() < COMPACT_THRESHOLD {
        return;
    }

    // 取出前面要压缩的部分 (保留最近 KEEP_RECENT 条)
    let to_compress: Vec<Message> = messages.drain(..COMPACT_THRESHOLD - KEEP_RECENT).collect();
    println!("[压缩] 将 {} 条消息压缩为摘要...", to_compress.len());

    let summary = summarize(client, api_key, &to_compress).await;

    // 把摘要作为 user 信息插回最前面
    messages.insert(0, Message {
        role: "user".to_string(),
        content: json!(format!("[对话历史摘要]\n{}", summary)),
    });

    println!("[压缩完成] 剩余消息数: {}", messages.len());
}

// agent loop (在每轮开始调用 maybe_compact)

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
        // S06 新增: 每轮开始前检查是否需要压缩
        maybe_compact(client, api_key, &mut messages).await;

        let response = call_api(client, api_key, &messages, tool_defs, system).await;

        match response.stop_reason.as_deref() {
            Some("tool_use") => {
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: json!(response.content),
                });

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

                messages.push(Message {
                    role: "user".to_string(),
                    content: json!(results),
                });
            }
            _ => {
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

// main

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("load_skill".to_string(), Box::new(SkillLoaderTool));
    tools.insert("bash".to_string(), Box::new(BashTool));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

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

// call_api

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
        .send().await.unwrap()
        .json::<ApiResponse>().await.unwrap()
}
