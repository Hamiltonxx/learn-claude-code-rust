# s01: The Agent Loop

`[ s01 ] s02 > s03 > s04 > s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"One loop & Bash is all you need"* -- one tool + one loop = an agent.
>
> **Harness layer**: The loop -- the model's first connection to the real world.

## Problem

A language model can reason about code, but it can't *touch* the real world -- can't read files, run tests, or check errors. Without a loop, every tool call requires you to manually copy-paste results back. You become the loop.

## Solution

```
+--------+      +-------+      +---------+
|  User  | ---> |  LLM  | ---> |  Tool   |
| prompt |      |       |      | execute |
+--------+      +---+---+      +----+----+
                    ^                |
                    |   tool_result  |
                    +----------------+
                    (loop until stop_reason != "tool_use")
```

One exit condition controls the entire flow. The loop runs until the model stops calling tools.

## How It Works

1. User prompt becomes the first message.

```rust
messages.push(Message {
    role: "user".to_string(),
    content: json!(input.trim()),
});
```

2. Send messages + tool definitions to the LLM.

```rust
let response = call_api(&client, &api_key, &messages).await;
```

3. Append the assistant response. Check `stop_reason` -- if the model didn't call a tool, we're done.

```rust
messages.push(Message {
    role: "assistant".to_string(),
    content: json!(reply),
});
if response.stop_reason.as_deref() != Some("tool_use") {
    break;
}
```

4. Execute each tool call, collect results, append as a user message. Loop back to step 2.

```rust
for block in &response.content {
    if let ContentBlock::ToolUse { id, name, input } = block {
        let result = execute_tool(name, input).await;
        tool_results.push(json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": result,
        }));
    }
}
messages.push(Message {
    role: "user".to_string(),
    content: json!(tool_results),
});
```

Assembled into one function:

```rust
async fn agent_loop(client: &Client, api_key: &str) {
    let mut messages: Vec<Message> = vec![];
    loop {
        let response = call_api(client, api_key, &messages).await;
        messages.push(Message {
            role: "assistant".to_string(),
            content: json!(response.content),
        });

        if response.stop_reason.as_deref() != Some("tool_use") {
            break;
        }

        let mut tool_results = vec![];
        for block in &response.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                let result = execute_tool(name, input).await;
                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": result,
                }));
            }
        }
        messages.push(Message {
            role: "user".to_string(),
            content: json!(tool_results),
        });
    }
}
```

That's the entire agent in under 30 lines. Everything else in this course layers on top -- without changing the loop.

## What Changed

| Component     | Before     | After                              |
|---------------|------------|------------------------------------|
| Agent loop    | (none)     | `loop` + stop_reason check         |
| Tools         | (none)     | `bash` (one tool)                  |
| Messages      | (none)     | Accumulating `Vec<Message>`        |
| Control flow  | (none)     | `stop_reason != "tool_use"`        |

## Try It

```sh
cd learn-claude-code-rust
cargo run --bin s01_agent_loop
```

1. `Create a file called hello.rs that prints "Hello, World!"`
2. `List all Rust files in this directory`
3. `What is the current git branch?`
4. `Create a directory called test_output and write 3 files in it`
