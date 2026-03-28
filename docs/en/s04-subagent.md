# s04: Subagents

`s01 > s02 > s03 > [ s04 ] s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"Break big tasks down; each subtask gets a clean context"* -- subagents use independent messages[], keeping the main conversation clean.
>
> **Harness layer**: Context isolation -- protecting the model's clarity of thought.

## Problem

As the agent works, its messages array grows. Every file read, every bash output stays in context permanently. "What testing framework does this project use?" might require reading 5 files, but the parent only needs the answer: "pytest."

## Solution

```
Parent agent                     Subagent
+------------------+             +------------------+
| messages=[...]   |             | messages=[]      | <-- fresh
|                  |  dispatch   |                  |
| tool: task       | ----------> | while tool_use:  |
|   prompt="..."   |             |   call tools     |
|                  |  summary    |   append results |
|   result = "..." | <---------- | return last text |
+------------------+             +------------------+

Parent context stays clean. Subagent context is discarded.
```

## How It Works

1. The parent only has `dispatch_agent`. The child gets bash, write_file, read_file — but not `dispatch_agent` (no recursive spawning).

```rust
struct DispatchAgentTool {
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
}

#[async_trait]
impl Tool for DispatchAgentTool {
    fn name(&self) -> &str { "dispatch_agent" }
    async fn execute(&self, input: Value) -> String {
        let task = input["task"].as_str().unwrap_or("").to_string();

        // Child gets its own isolated tool set — no dispatch_agent (prevents recursion)
        let mut sub_tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
        sub_tools.insert("bash".to_string(),       Box::new(BashTool));
        sub_tools.insert("write_file".to_string(), Box::new(WriteFileTool));
        sub_tools.insert("read_file".to_string(),  Box::new(ReadFileTool));
        let sub_tool_defs: Vec<Value> = sub_tools.values().map(|t| t.definition()).collect();

        // Run independent agent_loop — messages are fully isolated
        agent_loop(&self.client, &self.api_key, &task, &sub_tools, &sub_tool_defs).await
    }
}
```

2. `agent_loop` starts with a fresh local `messages` vec and runs its own tool loop. Only the final text is returned.

```rust
async fn agent_loop(
    client: &reqwest::Client, api_key: &str, task: &str,
    tools: &HashMap<String, Box<dyn Tool>>, tool_defs: &[Value],
) -> String {
    // Key: messages is a local variable, fully isolated from the caller
    let mut messages = vec![Message { role: "user".to_string(), content: json!(task) }];

    loop {
        let response = call_api(client, api_key, &messages, tool_defs, "You are a capable assistant.").await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(Message { role: "assistant".to_string(), content: json!(response.content) });
            let mut results = vec![];
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let result = tools[name].execute(input.clone()).await;
                    results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                }
            }
            messages.push(Message { role: "user".to_string(), content: json!(results) });
        } else {
            // The entire messages vec is dropped here — only text summary is returned
            return response.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("\n");
        }
    }
}
```

The child's entire message history is dropped when the function returns. The parent receives only a text summary as a normal `tool_result`.

## What Changed From s03

| Component      | Before (s03)     | After (s04)               |
|----------------|------------------|---------------------------|
| Tools          | 5                | 5 (base) + task (parent)  |
| Context        | Single shared    | Parent + child isolation  |
| Subagent       | None             | `agent_loop()` async fn   |
| Return value   | N/A              | Summary text only         |
| Ownership      | N/A              | `Arc<Client>` + `Arc<String>` |

## Try It

```sh
cargo run --bin s04_subagent
```

1. `Use a subagent to read Cargo.toml and summarize the dependencies`
2. `Dispatch two subtasks: one writes hello.rs, another writes world.rs`
3. `Use a subagent to check the directory structure and report back which .rs files exist`
