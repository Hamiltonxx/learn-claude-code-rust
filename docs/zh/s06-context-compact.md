# s06: Context Compact (上下文压缩)

`s01 > s02 > s03 > s04 > s05 > [ s06 ] | s07 > s08 > s09 > s10 > s11 > s12`

> *"上下文总会满, 要有办法腾地方"* -- 三层压缩策略, 换来无限会话。
>
> **Harness 层**: 压缩 -- 干净的记忆, 无限的会话。

## 问题

上下文窗口是有限的。读一个 1000 行的文件就吃掉 ~4000 token; 读 30 个文件、跑 20 条命令, 轻松突破 100k token。不压缩, 智能体根本没法在大项目里干活。

## 解决方案

三层压缩, 激进程度递增:

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

## 工作原理

1. **第一层 -- micro_compact**: 每次 LLM 调用前, 将旧的 tool result 替换为占位符。

```rust
// 超过阈值时，把前半段压缩成摘要，保留后半段原文
async fn maybe_compact(
    client: &Client,
    api_key: &str,
    messages: &mut Vec<Message>,
) {
    if messages.len() < COMPACT_THRESHOLD { return; }
    let half = messages.len() / 2;
    let old: Vec<Message> = messages.drain(..half).collect();
    // 用 LLM 生成摘要
    let summary = summarize(client, api_key, &old).await;
    // 把摘要插回消息列表头部
    messages.insert(0, Message { role: "user".into(),
        content: json!(format!("[对话摘要] {}", summary)) });
    messages.insert(1, Message { role: "assistant".into(),
        content: json!("已了解历史摘要，继续处理。") });
    println!("[压缩] 已将 {} 条消息压缩为摘要", half);
}
```

2. **第二层 -- auto_compact**: token 超过阈值时, 保存完整对话到磁盘, 让 LLM 做摘要。

```rust
async fn summarize(
    client: &Client,
    api_key: &str,
    messages: &[Message],
) -> String {
    let text = messages.iter()
        .filter_map(|m| m.content.as_str()
            .map(|s| format!("[{}] {}", m.role, s)))
        .collect::<Vec<_>>().join("\n");

    let req = vec![Message { role: "user".into(),
        content: json!(format!("请把以下对话压缩成简洁摘要：\n{}", text)) }];
    let resp = call_api(client, api_key, &req, &[], "你是摘要助手").await;
    resp.content.iter()
        .filter_map(|b| if let ContentBlock::Text { text } = b
            { Some(text.clone()) } else { None })
        .collect::<Vec<_>>().join("")
}
```

3. **第三层 -- manual compact**: `compact` 工具按需触发同样的摘要机制。

4. 循环整合三层:

```rust
loop {
    // S06：消息过多时自动压缩
    maybe_compact(&client, &api_key, &mut messages).await;

    let resp = call_api(&client, &api_key, &messages, &tool_defs, system).await;

    if resp.stop_reason.as_deref() == Some("tool_use") {
        // 执行工具...
    } else {
        // 输出最终回答
        break;
    }
}
```

完整历史通过 transcript 保存在磁盘上。信息没有真正丢失, 只是移出了活跃上下文。

## 相对 s05 的变更

| 组件           | 之前 (s05)       | 之后 (s06)                     |
|----------------|------------------|--------------------------------|
| Tools          | 5                | 5 (基础 + compact)             |
| 上下文管理     | 无               | 三层压缩                       |
| Micro-compact  | 无               | 旧结果 -> 占位符               |
| Auto-compact   | 无               | token 阈值触发                 |
| Transcripts    | 无               | 保存到 .transcripts/           |

## 试一试

```sh
cd learn-claude-code
cargo run --bin s06_context_compact
```

试试这些 prompt (英文 prompt 对 LLM 效果更好, 也可以用中文):

1. `Read every Python file in the agents/ directory one by one` (观察 micro-compact 替换旧结果)
2. `Keep reading files until compression triggers automatically`
3. `Use the compact tool to manually compress the conversation`
