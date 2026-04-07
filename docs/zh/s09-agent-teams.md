# s09: Agent Teams (智能体团队)

`s01 > s02 > s03 > s04 > s05 > s06 | s07 > s08 > [ s09 ] s10 > s11 > s12`

> *"任务太大一个人干不完, 要能分给队友"* -- 持久化队友 + JSONL 邮箱。
>
> **Harness 层**: 团队邮箱 -- 多个模型, 通过文件协调。

## 问题

子智能体 (s04) 是一次性的: 生成、干活、返回摘要、消亡。没有身份, 没有跨调用的记忆。后台任务 (s08) 能跑 shell 命令, 但做不了 LLM 引导的决策。

真正的团队协作需要三样东西: (1) 能跨多轮对话存活的持久智能体, (2) 身份和生命周期管理, (3) 智能体之间的通信通道。

## 解决方案

```
Teammate lifecycle:
  spawn -> WORKING -> IDLE -> WORKING -> ... -> SHUTDOWN

Communication:
  .team/
    config.json           <- team roster + statuses
    inbox/
      alice.jsonl         <- append-only, drain-on-read
      bob.jsonl
      lead.jsonl

              +--------+    send("alice","bob","...")    +--------+
              | alice  | -----------------------------> |  bob   |
              | loop   |    bob.jsonl << {json_line}    |  loop  |
              +--------+                                +--------+
                   ^                                         |
                   |        BUS.read_inbox("alice")          |
                   +---- alice.jsonl -> read + drain ---------+
```

## 工作原理

1. TeammateManager 通过 config.json 维护团队名册。

```rust
struct Teammate {
    name: String,
    role: String,
    system_prompt: String,
    // 收件箱：mpsc channel
    inbox_tx: mpsc::Sender<String>,
}

// 共享队伍注册表
type TeamRoster = Arc<Mutex<Vec<Teammate>>>;
```

2. `spawn()` 创建队友并在线程中启动 agent loop。

```rust
// spawn：为每个 teammate 创建独立的 tokio task + channel
fn spawn_teammate(name: String, role: String, system: String,
                  client: Arc<Client>, api_key: Arc<String>) -> mpsc::Sender<String> {
    let (inbox_tx, mut inbox_rx) = mpsc::channel::<String>(32);

    tokio::spawn(async move {
        let mut messages = vec![Message { role: "user".into(),
            content: json!(format!("你是 {}，角色：{}", name, role)) }];
        loop {
            // 检查收件箱
            while let Ok(msg) = inbox_rx.try_recv() {
                messages.push(Message { role: "user".into(),
                    content: json!(format!("<inbox>{}</inbox>", msg)) });
            }
            let resp = call_api(&client, &api_key, &messages, &[], &system).await;
            if resp.stop_reason.as_deref() != Some("tool_use") { break; }
            // 工具执行...
        }
    });

    inbox_tx
}
```

3. MessageBus: append-only 的 JSONL 收件箱。`send()` 追加一行; `read_inbox()` 读取全部并清空。

```rust
// 发消息给 teammate：直接发到对方的 inbox_tx channel
async fn send_message(roster: &TeamRoster, to: &str, content: &str) -> String {
    let tx = {
        let r = roster.lock().unwrap();
        r.iter().find(|t| t.name == to).map(|t| t.inbox_tx.clone())
    };
    match tx {
        Some(tx) => {
            let _ = tx.send(content.to_string()).await;
            format!("消息已发送给 {}", to)
        }
        None => format!("找不到队友 {}", to),
    }
}
```

4. 每个队友在每次 LLM 调用前检查收件箱, 将消息注入上下文。

```rust
// teammate 主循环：每轮检查收件箱，调用 API，执行工具
tokio::spawn(async move {
    let mut messages = vec![Message { role: "user".into(),
        content: json!(system_prompt) }];
    loop {
        // 非阻塞排空收件箱
        while let Ok(msg) = inbox_rx.try_recv() {
            messages.push(Message { role: "user".into(),
                content: json!(format!("<inbox>{}</inbox>", msg)) });
            messages.push(Message { role: "assistant".into(),
                content: json!("已记录消息。") });
        }
        let resp = call_api(&client, &api_key, &messages, &tool_defs, &system).await;
        if resp.stop_reason.as_deref() != Some("tool_use") { break; }
        // 执行工具，追加结果...
    }
});
```

## 相对 s08 的变更

| 组件           | 之前 (s08)       | 之后 (s09)                         |
|----------------|------------------|------------------------------------|
| Tools          | 6                | 9 (+spawn/send/read_inbox)         |
| 智能体数量     | 单一             | 领导 + N 个队友                    |
| 持久化         | 无               | config.json + JSONL 收件箱         |
| 线程           | 后台命令         | 每线程完整 agent loop              |
| 生命周期       | 一次性           | idle -> working -> idle            |
| 通信           | 无               | message + broadcast                |

## 试一试

```sh
cd learn-claude-code
cargo run --bin s09_agent_teams
```

试试这些 prompt (英文 prompt 对 LLM 效果更好, 也可以用中文):

1. `Spawn alice (coder) and bob (tester). Have alice send bob a message.`
2. `Broadcast "status update: phase 1 complete" to all teammates`
3. `Check the lead inbox for any messages`
4. 输入 `/team` 查看团队名册和状态
5. 输入 `/inbox` 手动检查领导的收件箱
