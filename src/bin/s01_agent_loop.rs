use learn_claude_code_rust::{ApiRequest, ApiResponse, ContentBlock, Message};
use reqwest::Client;
use serde_json::{json, Value};
use std::io::{self, Write};

const MODEL: &str = "claude-haiku-4-5-20251001"; // 比 "claude-opus-4-6" 省钱,够用了
const MAX_TOKENS: u32 = 8096;
const SYSTEM: &str = "You are a helpful assistant.";

#[tokio::main]
async fn main () {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = Client::new();
    let mut messages: Vec<Message> = vec![];

    println!("Agent ready. Type your message (Ctrl+C to exit):\n");

    loop {
        // 读取用户输入
        print!("> ");
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // 把用户消息加入历史
        messages.push(Message {
            role: "user".to_string(),
            content: json!(input),
        });

        // 调用 API
        let response = call_api(&client, &api_key, &messages).await;

        // 提取文本回复
        let reply = response.content.iter().filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

        println!("\nClaude: {}\n", reply);

        // 把 Claude 的回复加入历史
        messages.push(Message {
            role: "assistant".to_string(),
            content: json!(reply),
        });
    }
}

async fn call_api(client: &Client, api_key: &str, messages: &[Message]) -> ApiResponse {
    let request = ApiRequest {
        model: MODEL.to_string(),
        max_tokens: MAX_TOKENS,
        system: SYSTEM.to_string(),
        messages: messages.to_vec(),
        tools: None,
    };

    client
        /* .post("https://api.anthropic.com/v1/messages") */
        /* .header("x-api-key", api_key) */
        .post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .body(serde_json::to_string(&request).unwrap())
        .send()
        .await
        .unwrap()
        .json::<ApiResponse>()
        .await
        .unwrap()
}
