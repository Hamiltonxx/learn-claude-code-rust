#!/bin/bash
# init_agent.sh — Scaffold a new Rust agent project
#
# Usage:
#   ./init_agent.sh <agent-name>
#
# Example:
#   ./init_agent.sh my-agent

set -e

NAME=${1:?"Usage: $0 <agent-name>"}
DIR="./$NAME"

echo "Creating Rust agent project: $NAME"

mkdir -p "$DIR/src"

# Cargo.toml
cat > "$DIR/Cargo.toml" <<EOF
[package]
name = "$NAME"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
async-trait = "0.1"
EOF

# .env.example
cat > "$DIR/.env.example" <<EOF
ANTHROPIC_API_KEY=sk-xxx
EOF

# .gitignore
cat > "$DIR/.gitignore" <<EOF
/target
.env
EOF

# src/main.rs — minimal agent template
cat > "$DIR/src/main.rs" <<'RUST'
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Write as IoWrite, stdin, stdout};

const API_URL: &str = "https://api.ofox.ai/anthropic/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message { role: String, content: Value }

#[derive(Debug, Deserialize)]
struct ApiResponse { content: Vec<ContentBlock>, stop_reason: Option<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

fn tools() -> Value {
    json!([
        { "name": "bash", "description": "Run shell command",
          "input_schema": { "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] } }
    ])
}

fn execute_tool(name: &str, input: &Value) -> String {
    match name {
        "bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            let out = std::process::Command::new("sh").arg("-c").arg(cmd).output();
            match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout).to_string()
                    + &String::from_utf8_lossy(&o.stderr),
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => format!("Unknown tool: {}", name),
    }
}

async fn call_api(client: &Client, api_key: &str, messages: &[Message]) -> ApiResponse {
    client.post(API_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&json!({
            "model": MODEL, "max_tokens": 8096,
            "system": "You are a helpful coding agent.",
            "messages": messages, "tools": tools(),
        }))
        .send().await.unwrap()
        .json::<ApiResponse>().await.unwrap()
}

async fn agent(prompt: &str, history: &mut Vec<Message>, client: &Client, api_key: &str) -> String {
    history.push(Message { role: "user".into(), content: json!(prompt) });
    loop {
        let resp = call_api(client, api_key, history).await;
        history.push(Message { role: "assistant".into(), content: json!(resp.content) });
        if resp.stop_reason.as_deref() != Some("tool_use") {
            return resp.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("");
        }
        let mut results = vec![];
        for block in &resp.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                let output = execute_tool(name, input);
                results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": output }));
            }
        }
        history.push(Message { role: "user".into(), content: json!(results) });
    }
}

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY");
    let client = Client::new();
    let mut history = vec![];
    println!("Agent ready. Type 'q' to quit.\n");
    loop {
        print!(">> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut line = String::new();
        stdin().read_line(&mut line).unwrap();
        let line = line.trim();
        if line.is_empty() || line == "q" { break; }
        println!("{}\n", agent(line, &mut history, &client, &api_key).await);
    }
}
RUST

echo ""
echo "Project '$NAME' created at $DIR"
echo ""
echo "Next steps:"
echo "  1. cd $DIR"
echo "  2. cp .env.example .env && edit .env"
echo "  3. cargo run"
