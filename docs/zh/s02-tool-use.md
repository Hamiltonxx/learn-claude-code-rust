# s02: Tool Use (工具使用)

`s01 > [ s02 ] s03 > s04 > s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"加一个工具, 只加一个 handler"* -- 循环不用动, 新工具注册进 dispatch map 就行。
>
> **Harness 层**: 工具分发 -- 扩展模型能触达的边界。

## 问题

只有 `bash` 时, 所有操作都走 shell。`cat` 截断不可预测, `sed` 遇到特殊字符就崩, 每次 bash 调用都是不受约束的安全面。专用工具 (`read_file`, `write_file`) 可以在工具层面做路径沙箱。

关键洞察: 加工具不需要改循环。

## 解决方案

```
+--------+      +-------+      +----------------------+
|  User  | ---> |  LLM  | ---> | Tool Dispatch        |
| prompt |      |       |      | HashMap {            |
+--------+      +---+---+      |   "bash": BashTool   |
                    ^           |   "read": ReadTool   |
                    |           |   "write": WriteTool |
                    +-----------+   "edit": EditTool   |
                    tool_result | }                    |
                                +----------------------+

dispatch map 是 HashMap<String, Box<dyn Tool>>。
一次查找替代所有 if/else 分支。
```

## 工作原理

1. 定义 `Tool` trait，每个工具实现它。

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> Value;
    async fn execute(&self, input: Value) -> String;
}

struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }

    async fn execute(&self, input: Value) -> String {
        let path = input["path"].as_str().unwrap_or("");
        std::fs::read_to_string(path).unwrap_or_else(|e| e.to_string())
    }
    // ...
}
```

2. dispatch map 将工具名映射到处理对象。

```rust
let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
tools.insert("bash".to_string(),       Box::new(BashTool));
tools.insert("read_file".to_string(),  Box::new(ReadFileTool));
tools.insert("write_file".to_string(), Box::new(WriteFileTool));
tools.insert("edit_file".to_string(),  Box::new(EditFileTool));
```

3. 循环中按名称查找处理对象。循环体本身与 s01 完全一致。

```rust
for block in &response.content {
    if let ContentBlock::ToolUse { id, name, input } = block {
        let result = tools[name].execute(input.clone()).await;
        tool_results.push(json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": result,
        }));
    }
}
```

加工具 = 实现 `Tool` trait + 插入 `HashMap`。循环永远不变。

## 相对 s01 的变更

| 组件           | 之前 (s01)          | 之后 (s02)                       |
|----------------|---------------------|----------------------------------|
| Tools          | 1 (仅 bash)         | 4 (bash, read, write, edit)      |
| Dispatch       | 硬编码 bash 调用    | `HashMap<String, Box<dyn Tool>>` |
| 抽象           | 无                  | `Tool` trait object              |
| Agent loop     | 不变                | 不变                             |

## 试一试

```sh
cd learn-claude-code-rust
cargo run --bin s02_tool_use
```

试试这些 prompt:

1. `Read the file Cargo.toml and summarize it`
2. `Create a file called greet.rs with a greet function`
3. `Edit greet.rs to add a comment to the function`
4. `Read greet.rs to verify the edit worked`
