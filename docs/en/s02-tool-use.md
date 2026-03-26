# s02: Tool Use

`s01 > [ s02 ] s03 > s04 > s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"Adding a tool means adding one handler"* -- the loop stays the same; new tools register into the dispatch map.
>
> **Harness layer**: Tool dispatch -- expanding what the model can reach.

## Problem

With only `bash`, the agent shells out for everything. `cat` truncates unpredictably, `sed` fails on special characters, and every bash call is an unconstrained security surface. Dedicated tools like `read_file` and `write_file` let you enforce path sandboxing at the tool level.

The key insight: adding tools does not require changing the loop.

## Solution

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

The dispatch map is a HashMap<String, Box<dyn Tool>>.
One lookup replaces any if/else chain.
```

## How It Works

1. Define a `Tool` trait. Each tool implements it.

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

2. The dispatch map links tool names to handlers.

```rust
let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
tools.insert("bash".to_string(),       Box::new(BashTool));
tools.insert("read_file".to_string(),  Box::new(ReadFileTool));
tools.insert("write_file".to_string(), Box::new(WriteFileTool));
tools.insert("edit_file".to_string(),  Box::new(EditFileTool));
```

3. In the loop, look up the handler by name. The loop body itself is unchanged from s01.

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

Add a tool = implement `Tool` trait + insert into the `HashMap`. The loop never changes.

## What Changed From s01

| Component      | Before (s01)        | After (s02)                     |
|----------------|---------------------|---------------------------------|
| Tools          | 1 (bash only)       | 4 (bash, read, write, edit)     |
| Dispatch       | Hardcoded bash call | `HashMap<String, Box<dyn Tool>>`|
| Abstraction    | None                | `Tool` trait object             |
| Agent loop     | Unchanged           | Unchanged                       |

## Try It

```sh
cd learn-claude-code-rust
cargo run --bin s02_tool_use
```

1. `Read the file Cargo.toml and summarize it`
2. `Create a file called greet.rs with a greet function`
3. `Edit greet.rs to add a comment to the function`
4. `Read greet.rs to verify the edit worked`
