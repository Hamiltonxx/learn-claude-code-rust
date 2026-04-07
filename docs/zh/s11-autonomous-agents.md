# s11: Autonomous Agents (自治智能体)

`s01 > s02 > s03 > s04 > s05 > s06 | s07 > s08 > s09 > s10 > [ s11 ] s12`

> *"队友自己看看板, 有活就认领"* -- 不需要领导逐个分配, 自组织。
>
> **Harness 层**: 自治 -- 模型自己找活干, 无需指派。

## 问题

s09-s10 中, 队友只在被明确指派时才动。领导得给每个队友写 prompt, 任务看板上 10 个未认领的任务得手动分配。这扩展不了。

真正的自治: 队友自己扫描任务看板, 认领没人做的任务, 做完再找下一个。

一个细节: 上下文压缩 (s06) 后智能体可能忘了自己是谁。身份重注入解决这个问题。

## 解决方案

```
Teammate lifecycle with idle cycle:

+-------+
| spawn |
+---+---+
    |
    v
+-------+   tool_use     +-------+
| WORK  | <------------- |  LLM  |
+---+---+                +-------+
    |
    | stop_reason != tool_use (or idle tool called)
    v
+--------+
|  IDLE  |  poll every 5s for up to 60s
+---+----+
    |
    +---> check inbox --> message? ----------> WORK
    |
    +---> scan .tasks/ --> unclaimed? -------> claim -> WORK
    |
    +---> 60s timeout ----------------------> SHUTDOWN

Identity re-injection after compression:
  if len(messages) <= 3:
    messages.insert(0, identity_block)
```

## 工作原理

1. 队友循环分两个阶段: WORK 和 IDLE。LLM 停止调用工具 (或调用了 `idle`) 时, 进入 IDLE。

```rust
// teammate 自主循环：loop + select!，轮询任务板和 shutdown 信号
async fn teammate_worker(
    name: String, role: String,
    board: TaskBoard,
    client: Arc<Client>, api_key: Arc<String>,
    mut shutdown: mpsc::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = sleep(Duration::from_secs(2)) => {
                // 尝试从任务板认领一个匹配 role 的任务
                if let Some(task) = board_claim(&board, &role) {
                    println!("[{}] 认领 #{}: {}", name, task.id, task.title);
                    let result = run_task(&client, &api_key, &task.title).await;
                    board_complete(&board, task.id, &result);
                }
            }
            _ = shutdown.recv() => {
                println!("[{}] 收到关闭信号，退出。", name);
                break;
            }
        }
    }
}
```

2. 空闲阶段循环轮询收件箱和任务看板。

```rust
// board_claim：找到第一个 Open 且 role_hint 匹配的任务，原子地标为 InProgress
fn board_claim(board: &TaskBoard, role: &str) -> Option<BoardTask> {
    let mut tasks = board.lock().unwrap();
    for task in tasks.iter_mut() {
        if task.status == BoardStatus::Open
            && (task.role_hint == role || task.role_hint == "any")
        {
            task.status = BoardStatus::InProgress { claimed_by: role.to_string() };
            return Some(task.clone());
        }
    }
    None
}
```

3. 任务看板扫描: 找 pending 状态、无 owner、未被阻塞的任务。

```rust
// 共享任务板用 Arc<Mutex<Vec<BoardTask>>> 保护并发访问
type TaskBoard = Arc<Mutex<Vec<BoardTask>>>;

// 添加任务
fn board_add(board: &TaskBoard, title: &str, role_hint: &str) -> u32 {
    let mut tasks = board.lock().unwrap();
    let id = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    tasks.push(BoardTask { id, title: title.to_string(),
        role_hint: role_hint.to_string(), status: BoardStatus::Open });
    id
}
```

4. 身份重注入: 上下文过短 (说明发生了压缩) 时, 在开头插入身份块。

```rust
// shutdown 协议：主 agent 通过 mpsc channel 发信号
let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

// 退出时发送
let _ = shutdown_tx.send(()).await;
```

## 相对 s10 的变更

| 组件           | 之前 (s10)       | 之后 (s11)                       |
|----------------|------------------|----------------------------------|
| Tools          | 12               | 14 (+idle, +claim_task)          |
| 自治性         | 领导指派         | 自组织                           |
| 空闲阶段       | 无               | 轮询收件箱 + 任务看板            |
| 任务认领       | 仅手动           | 自动认领未分配任务               |
| 身份           | 系统提示         | + 压缩后重注入                   |
| 超时           | 无               | 60 秒空闲 -> 自动关机            |

## 试一试

```sh
cd learn-claude-code
cargo run --bin s11_autonomous
```

试试这些 prompt (英文 prompt 对 LLM 效果更好, 也可以用中文):

1. `Create 3 tasks on the board, then spawn alice and bob. Watch them auto-claim.`
2. `Spawn a coder teammate and let it find work from the task board itself`
3. `Create tasks with dependencies. Watch teammates respect the blocked order.`
4. 输入 `/tasks` 查看带 owner 的任务看板
5. 输入 `/team` 监控谁在工作、谁在空闲
