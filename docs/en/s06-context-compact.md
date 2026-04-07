# s06: Context Compact

`s01 > s02 > s03 > s04 > s05 > [ s06 ] | s07 > s08 > s09 > s10 > s11 > s12`

> *"Context will fill up; you need a way to make room"* -- three-layer compression strategy for infinite sessions.
>
> **Harness layer**: Compression -- clean memory for infinite sessions.

## Problem

The context window is finite. A single `read_file` on a 1000-line file costs ~4000 tokens. After reading 30 files and running 20 bash commands, you hit 100,000+ tokens. The agent cannot work on large codebases without compression.

## Solution

Three layers, increasing in aggressiveness:

```
Every turn:
+------------------+
| Tool call result |
+------------------+
        |
        v
[Layer 1: micro_compact]        (silent, every turn)
  Replace tool_result > 3 turns old
  with "[Previous: used {tool_name}]"
        |
        v
[Check: tokens > 50000?]
   |               |
   no              yes
   |               |
   v               v
continue    [Layer 2: auto_compact]
              Save transcript to .transcripts/
              LLM summarizes conversation.
              Replace all messages with [summary].
                    |
                    v
            [Layer 3: compact tool]
              Model calls compact explicitly.
              Same summarization as auto_compact.
```

## How It Works

1. **Layer 1 -- micro_compact**: Before each LLM call, replace old tool results with placeholders.

```rust
// When messages exceed threshold, drain the oldest half into a summary
async fn maybe_compact(client: &Client, api_key: &str, messages: &mut Vec<Message>) {
    if messages.len() < COMPACT_THRESHOLD { return; }
    let half = messages.len() / 2;
    let old: Vec<Message> = messages.drain(..half).collect();
    let summary = summarize(client, api_key, &old).await;
    messages.insert(0, Message { role: "user".into(),
        content: json!(format!("[Conversation summary] {}", summary)) });
    messages.insert(1, Message { role: "assistant".into(),
        content: json!("Understood. Continuing.") });
    println!("[compact] Compressed {} messages into summary", half);
}
```

2. **Layer 2 -- auto_compact**: When tokens exceed threshold, save full transcript to disk, then ask the LLM to summarize.

```rust
async fn summarize(client: &Client, api_key: &str, messages: &[Message]) -> String {
    let text = messages.iter()
        .filter_map(|m| m.content.as_str()
            .map(|s| format!("[{}] {}", m.role, s)))
        .collect::<Vec<_>>().join("\n");

    let req = vec![Message { role: "user".into(),
        content: json!(format!("Summarize this conversation concisely:\n{}", text)) }];
    let resp = call_api(client, api_key, &req, &[], "You are a summarization assistant").await;
    resp.content.iter()
        .filter_map(|b| if let ContentBlock::Text { text } = b
            { Some(text.clone()) } else { None })
        .collect::<Vec<_>>().join("")
}
```

3. **Layer 3 -- manual compact**: The `compact` tool triggers the same summarization on demand.

4. The loop integrates all three:

```rust
loop {
    // Compact if messages exceed threshold
    maybe_compact(&client, &api_key, &mut messages).await;

    let resp = call_api(&client, &api_key, &messages, &tool_defs, system).await;
    if resp.stop_reason.as_deref() == Some("tool_use") {
        // execute tools...
    } else {
        break;
    }
}
```

Transcripts preserve full history on disk. Nothing is truly lost -- just moved out of active context.

## What Changed From s05

| Component      | Before (s05)     | After (s06)                |
|----------------|------------------|----------------------------|
| Tools          | 5                | 5 (base + compact)         |
| Context mgmt   | None             | Three-layer compression    |
| Micro-compact  | None             | Old results -> placeholders|
| Auto-compact   | None             | Token threshold trigger    |
| Transcripts    | None             | Saved to .transcripts/     |

## Try It

```sh
cd learn-claude-code
cargo run --bin s06_context_compact
```

1. `Read every Python file in the agents/ directory one by one` (watch micro-compact replace old results)
2. `Keep reading files until compression triggers automatically`
3. `Use the compact tool to manually compress the conversation`
