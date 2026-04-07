# s10: Team Protocols (团队协议)

`s01 > s02 > s03 > s04 > s05 > s06 | s07 > s08 > s09 > [ s10 ] s11 > s12`

> *"队友之间要有统一的沟通规矩"* -- 一个 request-response 模式驱动所有协商。
>
> **Harness 层**: 协议 -- 模型之间的结构化握手。

## 问题

s09 中队友能干活能通信, 但缺少结构化协调:

**关机**: 直接杀线程会留下写了一半的文件和过期的 config.json。需要握手 -- 领导请求, 队友批准 (收尾退出) 或拒绝 (继续干)。

**计划审批**: 领导说 "重构认证模块", 队友立刻开干。高风险变更应该先过审。

两者结构一样: 一方发带唯一 ID 的请求, 另一方引用同一 ID 响应。

## 解决方案

```
Shutdown Protocol            Plan Approval Protocol
==================           ======================

Lead             Teammate    Teammate           Lead
  |                 |           |                 |
  |--shutdown_req-->|           |--plan_req------>|
  | {req_id:"abc"}  |           | {req_id:"xyz"}  |
  |                 |           |                 |
  |<--shutdown_resp-|           |<--plan_resp-----|
  | {req_id:"abc",  |           | {req_id:"xyz",  |
  |  approve:true}  |           |  approve:true}  |

Shared FSM:
  [pending] --approve--> [approved]
  [pending] --reject---> [rejected]

Trackers:
  shutdown_requests = {req_id: {target, status}}
  plan_requests     = {req_id: {from, plan, status}}
```

## 工作原理

1. 领导生成 request_id, 通过收件箱发起关机请求。

```rust
// 统一消息格式（enum 状态机）
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TeamMessage {
    ShutdownRequest  { request_id: String },
    ShutdownResponse { request_id: String, approve: bool },
    PlanRequest      { request_id: String, plan: String },
    PlanResponse     { request_id: String, approve: bool, feedback: String },
}

// 发起关机请求
async fn request_shutdown(to: &str, roster: &TeamRoster) -> String {
    let req_id = format!("{:x}", rand::random::<u32>());
    let msg = serde_json::to_string(&TeamMessage::ShutdownRequest {
        request_id: req_id.clone() }).unwrap();
    send_message(roster, to, &msg).await;
    format!("关机请求 {} 已发送 (pending)", req_id)
}
```

2. 队友收到请求后, 用 approve/reject 响应。

```rust
// teammate 处理关机请求：解析 enum，决定 approve/reject
if let Ok(TeamMessage::ShutdownRequest { request_id }) =
    serde_json::from_str::<TeamMessage>(&inbox_msg)
{
    let approve = true; // 收尾后同意
    let resp = TeamMessage::ShutdownResponse { request_id, approve };
    send_message(roster, "lead", &serde_json::to_string(&resp).unwrap()).await;
    if approve { break; } // 退出循环
}
```

3. 计划审批遵循完全相同的模式。队友提交计划 (生成 request_id), 领导审查 (引用同一个 request_id)。

```rust
// 同一个 FSM，两种用途：计划审批与关机协议结构完全相同
// pending -> approved | rejected
#[derive(Debug, Clone, PartialEq)]
enum RequestStatus { Pending, Approved, Rejected }

struct PendingRequest {
    from: String,
    plan: String,
    status: RequestStatus,
}

// 领导审批计划
fn review_plan(requests: &mut HashMap<String, PendingRequest>,
               req_id: &str, approve: bool, feedback: &str) {
    if let Some(req) = requests.get_mut(req_id) {
        req.status = if approve { RequestStatus::Approved }
                     else       { RequestStatus::Rejected };
        // 发送审批结果...
    }
}
```

一个 FSM, 两种用途。同样的 `pending -> approved | rejected` 状态机可以套用到任何请求-响应协议上。

## 相对 s09 的变更

| 组件           | 之前 (s09)       | 之后 (s10)                           |
|----------------|------------------|--------------------------------------|
| Tools          | 9                | 12 (+shutdown_req/resp +plan)        |
| 关机           | 仅自然退出       | 请求-响应握手                        |
| 计划门控       | 无               | 提交/审查与审批                      |
| 关联           | 无               | 每个请求一个 request_id              |
| FSM            | 无               | pending -> approved/rejected         |

## 试一试

```sh
cd learn-claude-code
cargo run --bin s10_protocols
```

试试这些 prompt (英文 prompt 对 LLM 效果更好, 也可以用中文):

1. `Spawn alice as a coder. Then request her shutdown.`
2. `List teammates to see alice's status after shutdown approval`
3. `Spawn bob with a risky refactoring task. Review and reject his plan.`
4. `Spawn charlie, have him submit a plan, then approve it.`
5. 输入 `/team` 监控状态
