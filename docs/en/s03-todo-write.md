# s03: TodoWrite

`s01 > s02 > [ s03 ] s04 > s05 > s06 | s07 > s08 > s09 > s10 > s11 > s12`

> *"An agent without a plan drifts"* -- list the steps first, then execute.
>
> **Harness layer**: Planning -- keeping the model on course without scripting the route.

## Problem

On multi-step tasks, the model loses track. It repeats work, skips steps, or wanders off. Long conversations make this worse -- the system prompt fades as tool results fill the context. A 10-step refactoring might complete steps 1-3, then the model starts improvising because it forgot steps 4-10.

## Solution

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

## How It Works

1. `TodoManager` stores items with statuses, shared across async boundaries via `Arc<Mutex<T>>`.

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
            Some(todo) => { todo.status = status.to_string(); format!("Task #{} -> {}", id, status) }
            None => format!("Task #{} not found", id),
        }
    }
}
```

2. `TodoWriteTool` implements the `Tool` trait and registers into the dispatch map like any other tool.

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
            "add"    => { let t = input["title"].as_str().unwrap_or("untitled");
                          format!("Added #{}: {}", mgr.add(t), t) }
            "list"   => mgr.list(),
            "update" => mgr.update(input["id"].as_u64().unwrap_or(0) as u32,
                                   input["status"].as_str().unwrap_or("pending")),
            "delete" => mgr.delete(input["id"].as_u64().unwrap_or(0) as u32),
            other    => format!("Unknown action: {}", other),
        }
    }
}

// Register into dispatch map
tools.insert("todo_write".to_string(), Box::new(TodoWriteTool { manager }));
```

3. Planning rules are enforced via the system prompt, requiring the model to list tasks before executing.

```rust
let system = "You are an organized assistant. For any multi-step task you must:
    1. First use todo_write(add) to list all subtasks
    2. Before starting each step, use todo_write(update, in_progress)
    3. After completing, use todo_write(update, done)
    Do not skip planning and jump straight to execution.
";
```

`Arc<Mutex<TodoManager>>` lets the tool trait object safely hold mutable state across async calls.

## What Changed From s02

| Component      | Before (s02)     | After (s03)                |
|----------------|------------------|----------------------------|
| Tools          | 4                | 5 (+todo)                  |
| Planning       | None             | TodoManager with statuses  |
| Planning rules | None             | system prompt enforcement  |
| State sharing  | Global variable  | `Arc<Mutex<TodoManager>>`  |

## Try It

```sh
cargo run --bin s03_todo
```

1. `Create a hello world Rust project and run it`
2. `Create three files: config.rs, utils.rs, main.rs, and have main.rs reference them`
3. `Read Cargo.toml, summarize the dependencies, then write a deps.md`
