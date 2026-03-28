# s04: Subagents (子智能体)

`s01 > s02 > s03 > [ s04 ] s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"大任务拆小, 每个小任务干净的上下文"* -- 子智能体用独立 messages[], 不污染主对话。
>
> **Harness 层**: 上下文隔离 -- 守护模型的思维清晰度。

## 问题

智能体工作越久, messages 数组越胖。每次读文件、跑命令的输出都永久留在上下文里。"这个项目用什么测试框架?" 可能要读 5 个文件, 但父智能体只需要一个词: "pytest。"

## 解决方案

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

## 工作原理

1. 父 agent 只有 `dispatch_agent` 工具；子 agent 拥有 bash、write_file、read_file 等基础工具（禁止递归派生）。

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

        // 子 agent 有自己独立的工具集，不含 dispatch_agent（防递归）
        let mut sub_tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
        sub_tools.insert("bash".to_string(),       Box::new(BashTool));
        sub_tools.insert("write_file".to_string(), Box::new(WriteFileTool));
        sub_tools.insert("read_file".to_string(),  Box::new(ReadFileTool));
        let sub_tool_defs: Vec<Value> = sub_tools.values().map(|t| t.definition()).collect();

        // 调用独立的 agent_loop，messages 完全隔离
        agent_loop(&self.client, &self.api_key, &task, &sub_tools, &sub_tool_defs).await
    }
}
```

2. `agent_loop` 以空 messages 启动，运行自己的工具循环，只返回最终文本。

```rust
async fn agent_loop(
    client: &reqwest::Client, api_key: &str, task: &str,
    tools: &HashMap<String, Box<dyn Tool>>, tool_defs: &[Value],
) -> String {
    // 关键: messages 是局部变量，和调用方完全隔离
    let mut messages = vec![Message { role: "user".to_string(), content: json!(task) }];

    loop {
        let response = call_api(client, api_key, &messages, tool_defs, "你是一个能干的助手。").await;

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
            // 子 agent 的全部 messages 在此丢弃，只返回文本摘要
            return response.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("\n");
        }
    }
}
```

子 agent 可能跑了几十次工具调用，但整个消息历史在函数返回后直接丢弃。父 agent 收到的只是一段文本，作为普通 `tool_result`。

## 相对 s03 的变更

| 组件           | 之前 (s03)       | 之后 (s04)                    |
|----------------|------------------|-------------------------------|
| Tools          | 5                | 5 (基础) + task (仅父端)      |
| 上下文         | 单一共享         | 父 + 子隔离                   |
| Subagent       | 无               | `agent_loop()` 独立函数       |
| 返回值         | 不适用           | 仅摘要文本                    |
| 所有权         | 不适用           | `Arc<Client>` + `Arc<String>` |

## 试一试

```sh
cargo run --bin s04_subagent
```

试试这些 prompt:

1. `用子agent读取 Cargo.toml，总结项目依赖`
2. `派发两个子任务：一个写 hello.rs，一个写 world.rs`
3. `用子agent检查当前目录结构，回来告诉我有哪些 .rs 文件`
