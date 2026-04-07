// Subagent Pattern — Context isolation via independent message history.
//
// Key insight: spawn child agents with ISOLATED messages[] to prevent
// "context pollution" where exploration details fill up the main conversation.
//
// Usage: add DispatchAgentTool to your main agent's tool map.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

// ── Subagent loop ─────────────────────────────────────────────────────────────

/// Run a subagent with completely isolated message history.
/// The parent only sees the final text output — not all the intermediate tool calls.
pub async fn run_subagent(
    client: &reqwest::Client,
    api_key: &str,
    task: &str,
    system: &str,
    cwd: &PathBuf,
) -> String {
    // KEY: fresh, isolated message history — parent context not visible
    let mut messages = vec![
        crate::Message { role: "user".into(), content: json!(task) }
    ];

    let tools = build_tools(cwd.clone());
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    loop {
        let resp = call_api(client, api_key, &messages, &tool_defs, system).await;

        if resp.stop_reason.as_deref() != Some("tool_use") {
            // Return ONLY the final summary text to parent
            return resp.content.iter()
                .filter_map(|b| if let crate::ContentBlock::Text { text } = b
                    { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("");
        }

        messages.push(crate::Message { role: "assistant".into(), content: json!(resp.content) });
        let mut results = vec![];
        for block in &resp.content {
            if let crate::ContentBlock::ToolUse { id, name, input } = block {
                let result = tools[name].execute(input.clone()).await;
                results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
            }
        }
        messages.push(crate::Message { role: "user".into(), content: json!(results) });
    }
}

// ── Agent type registry ───────────────────────────────────────────────────────

pub struct AgentType {
    pub description: &'static str,
    pub system: &'static str,
}

pub fn agent_types() -> HashMap<&'static str, AgentType> {
    let mut m = HashMap::new();
    m.insert("explore", AgentType {
        description: "Read-only agent for searching and analyzing code",
        system: "You are an exploration agent. Search and analyze, but NEVER modify files. Return a concise summary of what you found.",
    });
    m.insert("code", AgentType {
        description: "Full agent for implementing features and fixing bugs",
        system: "You are a coding agent. Implement the requested changes efficiently. Return a summary of what you changed.",
    });
    m.insert("plan", AgentType {
        description: "Planning agent for designing implementation strategies",
        system: "You are a planning agent. Analyze the codebase and output a numbered implementation plan. Do NOT make any changes.",
    });
    m
}

// ── DispatchAgentTool ─────────────────────────────────────────────────────────

pub struct DispatchAgentTool {
    pub client: std::sync::Arc<reqwest::Client>,
    pub api_key: std::sync::Arc<String>,
    pub cwd: PathBuf,
}

#[async_trait]
impl crate::Tool for DispatchAgentTool {
    fn name(&self) -> &str { "dispatch_agent" }

    fn definition(&self) -> Value {
        let types = agent_types();
        let descs = types.iter()
            .map(|(k, v)| format!("- {}: {}", k, v.description))
            .collect::<Vec<_>>().join("\n");
        json!({
            "name": "dispatch_agent",
            "description": format!("Spawn a subagent for a focused subtask.\n\nAgent types:\n{}", descs),
            "input_schema": {
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "Short task name (3-5 words)" },
                    "prompt": { "type": "string", "description": "Detailed instructions for the subagent" },
                    "agent_type": { "type": "string", "enum": ["explore", "code", "plan"] }
                },
                "required": ["description", "prompt", "agent_type"]
            }
        })
    }

    async fn execute(&self, input: Value) -> String {
        let description = input["description"].as_str().unwrap_or("task");
        let prompt = input["prompt"].as_str().unwrap_or("").to_string();
        let agent_type = input["agent_type"].as_str().unwrap_or("code");

        let types = agent_types();
        let system = types.get(agent_type)
            .map(|t| t.system)
            .unwrap_or("You are a helpful agent. Complete the task and summarize what you did.");

        println!("[subagent:{}] {}", agent_type, description);
        run_subagent(&self.client, &self.api_key, &prompt, system, &self.cwd).await
    }
}

// ── helpers (stubs — replace with your actual implementations) ────────────────

fn build_tools(_cwd: PathBuf) -> HashMap<String, Box<dyn crate::Tool>> {
    HashMap::new() // Replace with BashTool, ReadFileTool, etc. from tool-templates.rs
}

async fn call_api(
    _client: &reqwest::Client,
    _api_key: &str,
    _messages: &[crate::Message],
    _tools: &[Value],
    _system: &str,
) -> crate::ApiResponse {
    unimplemented!("replace with your call_api implementation")
}
