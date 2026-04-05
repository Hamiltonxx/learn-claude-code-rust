// S10 Protocols — 统一消息格式 + 协议状态机
//
// 核心概念：
//   - AgentMessage enum：所有 agent 间通信的统一格式
//   - Protocol enum：agent 的当前协议状态（空闲/等待批准/关闭中）
//   - 主 agent 收到 shutdown 信号后走关闭流程
//   - 主 agent 生成 plan 后先请求用户批准，批准后才执行
//
// Rust 重点：enum 状态机、match 穷尽模式、Display trait

use async_trait::async_trait;
use learn_claude_code_rust::{ContentBlock, Message, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Write as IoWrite, stdin, stdout};
use std::sync::Arc;

// ── 统一消息格式 ──────────────────────────────────────────────────────────────

/// 所有在 agent 之间传递的消息都用这个 enum 表示
/// tag = "type" 让序列化后带上 type 字段，方便调试
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentMessage {
    /// 普通任务请求
    Task { from: String, to: String, content: String },
    /// 任务完成后的响应
    Result { from: String, to: String, content: String },
    /// 要求执行前先经过审批的计划
    PlanRequest { from: String, plan: String },
    /// 用户批准 / 拒绝计划
    PlanApproval { approved: bool, reason: Option<String> },
    /// 触发关闭流程
    Shutdown { reason: String },
}

impl std::fmt::Display for AgentMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMessage::Task { from, to, content } =>
                write!(f, "[Task] {} -> {}: {}", from, to, content),
            AgentMessage::Result { from, to, content } =>
                write!(f, "[Result] {} -> {}: {}", from, to, content),
            AgentMessage::PlanRequest { from, plan } =>
                write!(f, "[PlanRequest] from={} plan={}", from, plan),
            AgentMessage::PlanApproval { approved, reason } =>
                write!(f, "[PlanApproval] approved={} reason={:?}", approved, reason),
            AgentMessage::Shutdown { reason } =>
                write!(f, "[Shutdown] reason={}", reason),
        }
    }
}

// ── 协议状态机 ────────────────────────────────────────────────────────────────

/// 描述 agent 当前所处的协议阶段
#[derive(Debug, Clone, PartialEq)]
enum Protocol {
    /// 正常运行，等待用户输入
    Idle,
    /// 已发出 PlanRequest，等待用户审批
    AwaitingApproval { plan: String },
    /// 收到 Shutdown，正在清理
    ShuttingDown { reason: String },
    /// 完全终止
    Terminated,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Idle => write!(f, "Idle"),
            Protocol::AwaitingApproval { .. } => write!(f, "AwaitingApproval"),
            Protocol::ShuttingDown { reason } => write!(f, "ShuttingDown({})", reason),
            Protocol::Terminated => write!(f, "Terminated"),
        }
    }
}

impl Protocol {
    /// 状态转换：接收一条消息后返回新状态
    fn transition(self, msg: &AgentMessage) -> Self {
        match (self, msg) {
            // 空闲时收到计划请求 → 等待审批
            (Protocol::Idle, AgentMessage::PlanRequest { plan, .. }) =>
                Protocol::AwaitingApproval { plan: plan.clone() },

            // 等待审批时收到批准 → 回到空闲（执行计划由调用方负责）
            (Protocol::AwaitingApproval { .. }, AgentMessage::PlanApproval { approved: true, .. }) =>
                Protocol::Idle,

            // 等待审批时收到拒绝 → 回到空闲（放弃计划）
            (Protocol::AwaitingApproval { .. }, AgentMessage::PlanApproval { approved: false, .. }) =>
                Protocol::Idle,

            // 任意状态收到 Shutdown → 进入关闭流程
            (_, AgentMessage::Shutdown { reason }) =>
                Protocol::ShuttingDown { reason: reason.clone() },

            // ShuttingDown 完成清理后 → Terminated（由外部触发，这里用 Task 消息模拟）
            (Protocol::ShuttingDown { .. }, AgentMessage::Task { .. }) =>
                Protocol::Terminated,

            // 其他情况保持不变
            (state, _) => state,
        }
    }
}

// ── Teammate ──────────────────────────────────────────────────────────────────

struct Teammate {
    system_prompt: String,
}

// ── 工具 ──────────────────────────────────────────────────────────────────────

struct BashTool;
#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn definition(&self) -> Value {
        json!({
            "name": "bash",
            "description": "执行 bash 命令",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c").arg(input["command"].as_str().unwrap_or(""))
            .output().expect("failed");
        String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr)
    }
}

/// 主 agent 用来触发关闭流程的工具
/// Claude 调用 shutdown 时，外部循环检测到协议状态变为 ShuttingDown
struct ShutdownTool {
    // 用 Arc<Mutex> 把状态写回主循环
    protocol: Arc<std::sync::Mutex<Protocol>>,
}
#[async_trait]
impl Tool for ShutdownTool {
    fn name(&self) -> &str { "shutdown" }
    fn definition(&self) -> Value {
        json!({
            "name": "shutdown",
            "description": "发起关闭流程。当用户说'退出'、'关闭'或任务全部完成时调用。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "reason": { "type": "string", "description": "关闭原因" }
                },
                "required": ["reason"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let reason = input["reason"].as_str().unwrap_or("用户请求").to_string();
        let msg = AgentMessage::Shutdown { reason: reason.clone() };
        let mut p = self.protocol.lock().unwrap();
        *p = p.clone().transition(&msg);
        format!("已发起关闭流程: {}", reason)
    }
}

/// 主 agent 用来提交计划、等待用户审批的工具
struct PlanRequestTool {
    protocol: Arc<std::sync::Mutex<Protocol>>,
}
#[async_trait]
impl Tool for PlanRequestTool {
    fn name(&self) -> &str { "request_plan_approval" }
    fn definition(&self) -> Value {
        json!({
            "name": "request_plan_approval",
            "description": "提交执行计划，等待用户审批后再继续。对于有副作用的操作（写文件、执行命令）必须先调用此工具。",
            "input_schema": {
                "type": "object",
                "properties": {
                    "plan": { "type": "string", "description": "详细的执行步骤描述" }
                },
                "required": ["plan"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let plan = input["plan"].as_str().unwrap_or("").to_string();
        let msg = AgentMessage::PlanRequest { from: "agent".to_string(), plan: plan.clone() };
        {
            let mut p = self.protocol.lock().unwrap();
            *p = p.clone().transition(&msg);
        }
        // 打印计划，让外部主循环读取用户输入并决定是否批准
        println!("\n[计划审批] Agent 提交了以下计划:\n{}\n", plan);
        print!("是否批准？(y/n): ");
        IoWrite::flush(&mut stdout()).unwrap();

        let mut answer = String::new();
        stdin().read_line(&mut answer).unwrap();
        let approved = answer.trim().to_lowercase().starts_with('y');

        let approval = AgentMessage::PlanApproval {
            approved,
            reason: if approved { None } else { Some("用户拒绝".to_string()) },
        };
        {
            let mut p = self.protocol.lock().unwrap();
            *p = p.clone().transition(&approval);
        }

        if approved {
            "计划已批准，继续执行。".to_string()
        } else {
            "计划被拒绝，请重新规划。".to_string()
        }
    }
}

/// 派发给 teammate 的工具（复用 s09 模式）
struct SendMessageTool {
    client: Arc<reqwest::Client>,
    api_key: Arc<String>,
    teammates: Arc<HashMap<String, Teammate>>,
}
#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str { "send_message" }
    fn definition(&self) -> Value {
        json!({
            "name": "send_message",
            "description": "把任务发给指定队友。可用: coder(写代码), reviewer(审查代码)",
            "input_schema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["to", "message"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let to = input["to"].as_str().unwrap_or("");
        let message = input["message"].as_str().unwrap_or("");

        let Some(teammate) = self.teammates.get(to) else {
            return format!("未找到队友: {}", to);
        };

        // 构造 AgentMessage::Task 并打印（体现统一协议）
        let msg = AgentMessage::Task {
            from: "coordinator".to_string(),
            to: to.to_string(),
            content: message.to_string(),
        };
        println!("  {}", msg);

        let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
        tools.insert("bash".to_string(), Box::new(BashTool));
        let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

        let result = teammate_loop(
            &self.client, &self.api_key,
            &teammate.system_prompt, message,
            &tools, &tool_defs,
        ).await;

        // 构造 AgentMessage::Result 并打印
        let reply = AgentMessage::Result {
            from: to.to_string(),
            to: "coordinator".to_string(),
            content: result.clone(),
        };
        println!("  {}", reply);
        result
    }
}

// ── teammate agent loop ───────────────────────────────────────────────────────

async fn teammate_loop(
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: &str,
    task: &str,
    tools: &HashMap<String, Box<dyn Tool>>,
    tool_defs: &[Value],
) -> String {
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: json!(task),
    }];
    loop {
        let response = call_api(client, api_key, &messages, tool_defs, system_prompt).await;
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
            return response.content.iter()
                .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None })
                .collect::<Vec<_>>().join("\n");
        }
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let api_key = Arc::new(std::env::var("ANTHROPIC_API_KEY").expect("需要 ANTHROPIC_API_KEY"));
    let client = Arc::new(reqwest::Client::new());

    // 协议状态机（共享给工具）
    let protocol = Arc::new(std::sync::Mutex::new(Protocol::Idle));

    // 注册 teammates
    let mut teammate_map = HashMap::new();
    teammate_map.insert("coder".to_string(), Teammate {
        system_prompt: "你是 Rust 代码生成专家。收到需求后直接给出简洁正确的实现。".to_string(),
    });
    teammate_map.insert("reviewer".to_string(), Teammate {
        system_prompt: "你是严格的代码审查专家。找出 bug、性能问题和不符合 Rust 惯用法的写法。".to_string(),
    });
    let teammates = Arc::new(teammate_map);

    // 注册工具
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("send_message".to_string(), Box::new(SendMessageTool {
        client: client.clone(), api_key: api_key.clone(), teammates: teammates.clone(),
    }));
    tools.insert("shutdown".to_string(), Box::new(ShutdownTool {
        protocol: protocol.clone(),
    }));
    tools.insert("request_plan_approval".to_string(), Box::new(PlanRequestTool {
        protocol: protocol.clone(),
    }));
    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是一个协调型 AI 技术团队。\
        - 对有副作用的操作，先用 request_plan_approval 列出执行计划并等待批准\
        - 批准后才派发给 coder / reviewer\
        - 用户说退出或任务全部完成时，调用 shutdown\
        始终用统一消息格式与队友沟通。";

    println!("=== S10 Protocols (输入 '退出' 触发关闭) ===");

    let mut messages: Vec<Message> = vec![];

    loop {
        // 检查协议状态
        let current_state = protocol.lock().unwrap().clone();
        match current_state {
            Protocol::Terminated => {
                println!("[协议] Agent 已终止。再见！");
                break;
            }
            Protocol::ShuttingDown { ref reason } => {
                println!("[协议] 正在关闭: {}... 再见！", reason);
                // 转为 Terminated
                let mut p = protocol.lock().unwrap();
                *p = Protocol::Terminated;
                break;
            }
            _ => {}
        }

        // 读取用户输入
        print!("\n用户> ");
        IoWrite::flush(&mut stdout()).unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        let input = input.trim();
        if input.is_empty() { continue; }

        // 将用户输入包装成 AgentMessage::Task 并打印（体现统一协议）
        let user_msg = AgentMessage::Task {
            from: "user".to_string(),
            to: "coordinator".to_string(),
            content: input.to_string(),
        };
        println!("  {}", user_msg);

        messages.push(Message { role: "user".to_string(), content: json!(input) });

        // agent loop（单轮完整推理直到 end_turn）
        loop {
            let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;

            if response.stop_reason.as_deref() == Some("tool_use") {
                messages.push(Message { role: "assistant".to_string(), content: json!(response.content) });
                let mut results = vec![];
                for block in &response.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        println!("[工具调用] {}", name);
                        let result = tools[name].execute(input.clone()).await;
                        results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": result }));
                    }
                }
                messages.push(Message { role: "user".to_string(), content: json!(results) });
            } else {
                for block in &response.content {
                    if let ContentBlock::Text { text } = block {
                        println!("\nAgent: {}", text);
                    }
                }
                break;
            }

            // 每次工具执行后检查协议状态
            let state = protocol.lock().unwrap().clone();
            if matches!(state, Protocol::ShuttingDown { .. } | Protocol::Terminated) {
                break;
            }
        }
    }
}

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[Message],
    tools: &[Value],
    system: &str,
) -> learn_claude_code_rust::ApiResponse {
    let request = learn_claude_code_rust::ApiRequest {
        model: "claude-haiku-4-5-20251001".to_string(),
        max_tokens: 8096,
        system: system.to_string(),
        messages: messages.to_vec(),
        tools: Some(tools.to_vec()),
    };
    client.post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request).send().await.unwrap()
        .json::<learn_claude_code_rust::ApiResponse>().await.unwrap()
}
