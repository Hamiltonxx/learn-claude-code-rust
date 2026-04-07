# s08: Background Tasks (后台任务)

`s01 > s02 > s03 > s04 > s05 > s06 | s07 > [ s08 ] s09 > s10 > s11 > s12`

> *"慢操作丢后台, agent 继续想下一步"* -- 后台线程跑命令, 完成后注入通知。
>
> **Harness 层**: 后台执行 -- 模型继续思考, harness 负责等待。

## 问题

有些命令要跑好几分钟: `npm install`、`pytest`、`docker build`。阻塞式循环下模型只能干等。用户说 "装依赖, 顺便建个配置文件", 智能体却只能一个一个来。

## 解决方案

```
Main thread                Background thread
+-----------------+        +-----------------+
| agent loop      |        | subprocess runs |
| ...             |        | ...             |
| [LLM call] <---+------- | enqueue(result) |
|  ^drain queue   |        +-----------------+
+-----------------+

Timeline:
Agent --[spawn A]--[spawn B]--[other work]----
             |          |
             v          v
          [A runs]   [B runs]      (parallel)
             |          |
             +-- results injected before next LLM call --+
```

## 工作原理

1. BackgroundManager 用线程安全的通知队列追踪任务。

```rust
// tokio::spawn 后台执行，mpsc channel 通知主循环
let (done_tx, mut done_rx) = mpsc::channel::<String>(16);
```

2. `run()` 启动守护线程, 立即返回。

```rust
// 后台任务工具：spawn 一个 tokio task，立即返回
async fn execute(&self, input: Value) -> String {
    let command = input["command"].as_str().unwrap_or("").to_string();
    let task_id = format!("{:x}", rand::random::<u32>());
    let tx = self.done_tx.clone();
    let id = task_id.clone();

    tokio::spawn(async move {
        let out = Command::new("sh").arg("-c").arg(&command)
            .output().await.unwrap();
        let result = String::from_utf8_lossy(&out.stdout).to_string();
        let _ = tx.send(format!("[bg:{}] {}", id, &result[..result.len().min(200)])).await;
    });

    format!("后台任务 {} 已启动", task_id)
}
```

3. 子进程完成后, 结果进入通知队列。

```rust
// 主循环每轮用 try_recv 非阻塞检查后台完成通知
while let Ok(msg) = done_rx.try_recv() {
    println!("[后台通知] {}", msg);
    messages.push(Message { role: "user".into(),
        content: json!(format!("<background-results>{}</background-results>", msg)) });
    messages.push(Message { role: "assistant".into(),
        content: json!("已记录后台结果。") });
}
```

4. 每次 LLM 调用前排空通知队列。

```rust
// agent loop：每轮先排空通知，再调用 API
loop {
    // 排空后台完成通知（非阻塞）
    while let Ok(msg) = done_rx.try_recv() {
        messages.push(Message { role: "user".into(),
            content: json!(format!("<background-results>{}</background-results>", msg)) });
        messages.push(Message { role: "assistant".into(),
            content: json!("已记录后台结果。") });
    }

    let resp = call_api(&client, &api_key, &messages, &tool_defs, system).await;
    // ... 工具执行 ...
}
```

循环保持单线程。只有子进程 I/O 被并行化。

## 相对 s07 的变更

| 组件           | 之前 (s07)       | 之后 (s08)                         |
|----------------|------------------|------------------------------------|
| Tools          | 8                | 6 (基础 + background_run + check)  |
| 执行方式       | 仅阻塞           | 阻塞 + 后台线程                    |
| 通知机制       | 无               | 每轮排空的队列                     |
| 并发           | 无               | 守护线程                           |

## 试一试

```sh
cd learn-claude-code
cargo run --bin s08_background_tasks
```

试试这些 prompt (英文 prompt 对 LLM 效果更好, 也可以用中文):

1. `Run "sleep 5 && echo done" in the background, then create a file while it runs`
2. `Start 3 background tasks: "sleep 2", "sleep 4", "sleep 6". Check their status.`
3. `Run pytest in the background and keep working on other things`
