# s03: TodoWrite (待办写入)

`s01 > s02 > [ s03 ] s04 > s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"没有计划的 agent 走哪算哪"* -- 先列步骤再动手, 完成率翻倍。
>
> **Harness 层**: 规划 -- 让模型不偏航, 但不替它画航线。

## 问题

多步任务中, 模型会丢失进度 -- 重复做过的事、跳步、跑偏。对话越长越严重: 工具结果不断填满上下文, 系统提示的影响力逐渐被稀释。一个 10 步重构可能做完 1-3 步就开始即兴发挥, 因为 4-10 步已经被挤出注意力了。

## 解决方案

```
+--------+      +-------+      +---------+
|  User  | ---> |  LLM  | ---> | Tools   |
| prompt |      |       |      | + todo  |
+--------+      +---+---+      +----+----+
                    ^                |
                    |   tool_result  |
                    +----------------+
                          |
              +-----------+-----------+
              | TodoManager state     |
              | [ ] task A            |
              | [>] task B  <- doing  |
              | [x] task C            |
              +-----------------------+
                          |
              if rounds_since_todo >= 3:
                inject <reminder> into tool_result
```

## 工作原理

1. TodoManager 存储带状态的项目。同一时间只允许一个 `in_progress`。

```rust
#[derive(Debug, Clone)]
struct TodoItem {
    id: u32,
    title: String,
    status: String,  // "pending" | "in_progress" | "done"
}

struct TodoManager {
    todos: Vec<TodoItem>,
    next_id: u32,
}

impl TodoManager {
    fn update(&mut self, id: u32, status: &str) -> String {
        match self.todos.iter_mut().find(|t| t.id == id) {
            Some(todo) => { todo.status = status.to_string(); format!("任务 #{} -> {}", id, status) }
            None => format!("找不到任务 #{}", id),
        }
    }
}
```

2. `TodoWriteTool` 实现 `Tool` trait，持有 `Arc<Mutex<TodoManager>>`，和其他工具一样注册进 dispatch map。

```rust
struct TodoWriteTool {
    manager: Arc<Mutex<TodoManager>>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str { "todo_write" }
    async fn execute(&self, input: Value) -> String {
        let mut mgr = self.manager.lock().unwrap();
        match input["action"].as_str().unwrap_or("") {
            "add"    => { let t = input["title"].as_str().unwrap_or("未命名");
                          format!("已添加 #{}: {}", mgr.add(t), t) }
            "list"   => mgr.list(),
            "update" => mgr.update(input["id"].as_u64().unwrap_or(0) as u32,
                                   input["status"].as_str().unwrap_or("pending")),
            "delete" => mgr.delete(input["id"].as_u64().unwrap_or(0) as u32),
            other    => format!("未知操作: {}", other),
        }
    }
}

// 注册进 dispatch map
tools.insert("todo_write".to_string(), Box::new(TodoWriteTool { manager }));
```

3. 规划规则写进 system prompt，强制模型先列任务再执行。

```rust
let system = "你是一个有条理的助手。执行任何多步骤任务时必须：
    1. 先用 todo_write(add) 把所有子任务列出
    2. 开始某步前用 todo_write(update, in_progress) 标记
    3. 完成后用 todo_write(update, done) 标记
    不允许跳过规划直接执行。
";
```

`Arc<Mutex<TodoManager>>` 让 tool trait object 安全持有可变状态；system prompt 的约束替代了 nag reminder 机制。

## 相对 s02 的变更

| 组件           | 之前 (s02)       | 之后 (s03)                     |
|----------------|------------------|--------------------------------|
| Tools          | 4                | 5 (+todo)                      |
| 规划           | 无               | 带状态的 TodoManager           |
| 规划约束       | 无               | system prompt 强制先列任务     |
| 状态共享       | 全局变量         | `Arc<Mutex<TodoManager>>`      |

## 试一试

```sh
cargo run --bin s03_todo
```

试试这些 prompt:

1. `帮我创建一个 hello world Rust 项目并运行`
2. `创建三个文件：config.rs、utils.rs、main.rs，并在 main.rs 中引用它们`
3. `读取 Cargo.toml，总结依赖，然后写一份 deps.md`
