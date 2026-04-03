use async_trait::async_trait;
use learn_claude_code_rust::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{Write as IoWrite, stdin, stdout};
use std::sync::{Arc, Mutex};

// Task 数据结构

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Todo,
    InProgress,
    Done,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Todo => write!(f, "todo"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Done => write!(f, "done"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    id: String,
    title: String,
    status: TaskStatus,
    deps: Vec<String>, // 依赖的 task id 列表
}

// TaskManager: CRUD + 持久化 + 拓扑排序

struct TaskManager {
    tasks: Vec<Task>,
    path: String,
    next_id: u32,
}

impl TaskManager {
    /// 从 JSON 文件加载，文件不存在则创建空的 TaskManager
    fn load(path: &str) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(tasks) = serde_json::from_str::<Vec<Task>>(&content) {
                let next_id = tasks.iter().filter_map(|t| t.id.parse::<u32>().ok())
                    .max().unwrap_or(0) + 1;
                return TaskManager { tasks, path: path.to_string(), next_id };
            }
        }
        TaskManager { tasks: vec![], path: path.to_string(), next_id: 1 }
    }

    /// 持久化到 JSON 文件
    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.tasks).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }

    /// 新建任务，返回新任务的 id
    fn add(&mut self, title: &str, deps: Vec<String>) -> Result<String, String> {
        // 检查依赖的 id 是否存在
        for dep in &deps {
            if !self.tasks.iter().any(|t| &t.id == dep) {
                return Err(format!("依赖的任务 id {} 不存在", dep));
            }
        }
        let id = self.next_id.to_string();
        self.next_id += 1;
        self.tasks.push(Task {
            id: id.clone(),
            title: title.to_string(),
            status: TaskStatus::Todo,
            deps,
        });
        self.save()?;
        Ok(id)
    }

    /// 更新任务状态
    fn update_status(&mut self, id: &str, status: TaskStatus) -> Result<(), String> {
        let task = self.tasks.iter_mut().find(|t| t.id == id)
            .ok_or_else(|| format!("任务 id {} 不存在", id))?;
        task.status = status;
        self.save()
    }

    /// Kahn 算法拓扑排序，返回任务列表 (按依赖顺序)
    /// 有环时返回 Err
    fn topo_sort(&self) -> Result<Vec<Task>, String> {
        // 建立 id -> index 映射
        let index: HashMap<&str, usize> = self.tasks
            .iter()
            .enumerate()
            .map(|(i,t)| (t.id.as_str(), i))
            .collect();

        // task 的入度 =  它的 deps 数量
        let mut in_degree: Vec<usize> = self.tasks.iter().map(|t| t.deps.len()).collect();

        // 邻接表: dep 完成后, 能减少哪些任务的入度
        let mut dependents: Vec<Vec<usize>> = vec![vec![]; self.tasks.len()];
        for (i, task) in self.tasks.iter().enumerate() {
            for dep in &task.deps {
                if let Some(&j) = index.get(dep.as_str()) {
                    dependents[j].push(i);
                }
            }
        }

        let mut queue: VecDeque<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        let mut result = vec![];
        while let Some(i) = queue.pop_front() {
            result.push(self.tasks[i].clone());
            for &next in &dependents[i] {
                in_degree[next] -= 1;
                if in_degree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }

        if result.len() != self.tasks.len() {
            return Err("任务依赖中存在循环".to_string());
        }
        Ok(result)
    }

    // 返回依赖全部完成、可立即开始的 Todo 任务
    fn get_next_tasks(&self) -> Vec<&Task> {
        self.tasks.iter().filter(|t| {
            t.status == TaskStatus::Todo && t.deps.iter().all(|dep_id| {
                self.tasks.iter().any(|d| &d.id == dep_id && d.status == TaskStatus::Done)
            })
        }).collect()
    }
}

// 工具

type SharedManager = Arc<Mutex<TaskManager>>;

struct CreateTaskTool(SharedManager);

#[async_trait]
impl Tool for CreateTaskTool {
    fn name(&self) -> &str { "create_task" }
    fn definition(&self) -> Value {
        json!({
            "name": "create_task",
            "description": "创建新任务, 可指定依赖的任务 id 列表",
            "input_schema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "deps": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["title", "deps"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let title = input["title"].as_str().unwrap_or("").to_string();
        let deps: Vec<String> = input["deps"].as_array().unwrap_or(&vec![])
            .iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
        let mut mgr = self.0.lock().unwrap();
        match mgr.add(&title, deps) {
            Ok(id) => format!("已创建任务 id={} title={}", id, title),
            Err(e) => format!("创建失败: {}", e),
        }
    }
}

struct UpdateTaskTool(SharedManager);

#[async_trait]
impl Tool for UpdateTaskTool {
    fn name(&self) -> &str { "udpate_task" }
    fn definition(&self) -> Value {
        json!({
            "name": "update_task",
            "description": "更新任务状态",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "status": { "type": "string", "enum": ["todo", "in_progress", "done"] }
                },
                "required": ["id", "status"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let id = input["id"].as_str().unwrap_or("");
        let status = match input["status"].as_str().unwrap_or("") {
            "todo" => TaskStatus::Todo,
            "in_progress" => TaskStatus::InProgress,
            "done" => TaskStatus::Done,
            s => return format!("未知状态: {}", s),
        };
        let mut mgr = self.0.lock().unwrap();
        match mgr.update_status(id, status) {
            Ok(()) => format!("任务 {} 状态已更新", id),
            Err(e) => format!("更新失败: {}", e),
        }
    }
}

struct ListTasksTool(SharedManager);

#[async_trait]
impl Tool for ListTasksTool {
    fn name(&self) -> &str { "list_tasks" }
    fn definition(&self) -> Value {
        json!({
            "name": "list_tasks",
            "description": "列出所有任务 (按拓扑顺序)",
            "input_schema": { "type": "object", "properties": {} }
        })
    }
    async fn execute(&self, _input: Value) -> String {
        let mgr = self.0.lock().unwrap();
        match mgr.topo_sort() {
            Ok(tasks) => {
                if tasks.is_empty() { return "暂无任务".to_string(); }
                tasks.iter().map(|t| {
                    let deps = if t.deps.is_empty() { "无".to_string() } else { t.deps.join(", ") };
                    format!("[{}] {} | 状态: {} | 依赖: {}", t.id, t.title, t.status, deps)
                }).collect::<Vec<_>>().join("\n")
            }
            Err(e) => format!("错误: {}", e),
        }
    }
}

struct GetNextTasksTool(SharedManager);

#[async_trait]
impl Tool for GetNextTasksTool {
    fn name(&self) -> &str { "get_next_tasks" }
    fn definition(&self) -> Value {
        json!({
            "name": "get_next_tasks",
            "description": "返回所有依赖已完成、可立即开始的 todo 任务",
            "input_schema": { "type": "object", "properties": {} }
        })
    }
    async fn execute(&self, _input: Value) -> String {
        let mgr = self.0.lock().unwrap();
        let next = mgr.get_next_tasks();
        if next.is_empty() {
            "没有可立即执行的任务".to_string()
        } else {
            next.iter().map(|t| format!("[{}] {}", t.id, t.title)).collect::<Vec<_>>().join("\n")
        }
    }
}

// API 调用

async fn call_api(
    client: &reqwest::Client,
    api_key: &str,
    messages: &[learn_claude_code_rust::Message],
    tools: &[Value],
    system: &str,
) -> learn_claude_code_rust::ApiResponse {
    let request = learn_claude_code_rust::ApiRequest {
        model: "claude-haiku-4-5".to_string(),
        max_tokens: 8096,
        system: system.to_string(),
        messages: messages.to_vec(),
        tools: Some(tools.to_vec()),
    };
    client.post("https://api.ofox.ai/anthropic/v1/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send().await.unwrap()
        .json::<learn_claude_code_rust::ApiResponse>().await.unwrap()
}


// main

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let client = reqwest::Client::new();

    let manager = Arc::new(Mutex::new(TaskManager::load("tasks.json")));

    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert("create_task".to_string(),     Box::new(CreateTaskTool(manager.clone())));
    tools.insert("update_task".to_string(),     Box::new(UpdateTaskTool(manager.clone())));
    tools.insert("list_tasks".to_string(),      Box::new(ListTasksTool(manager.clone())));
    tools.insert("get_next_tasks".to_string(),  Box::new(GetNextTasksTool(manager.clone())));

    let tool_defs: Vec<Value> = tools.values().map(|t| t.definition()).collect();

    let system = "你是一个任务管理助手。用户描述项目后，用 create_task 创建任务并合并设置依赖，然后用 list_tasks 展示完整列表，最后用 get_next_tasks 告诉用户可以立即开始的任务。";

    println!("=== S07 Task System ===");
    println!("任务持久化到 tasks.json\n");

    print!("> ");
    IoWrite::flush(&mut stdout()).unwrap();
    let mut input = String::new();
    stdin().read_line(&mut input).unwrap();

    let mut messages = vec![learn_claude_code_rust::Message {
        role: "user".to_string(),
        content: json!(input.trim()),
    }];

    loop {
        let response = call_api(&client, &api_key, &messages, &tool_defs, system).await;

        if response.stop_reason.as_deref() == Some("tool_use") {
            messages.push(learn_claude_code_rust::Message {
                role: "assistant".to_string(),
                content: json!(response.content),
            });

            let mut results = vec![];
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::ToolUse { id, name, input } = block {
                    println!("[工具] {}", name);
                    let result = tools[name].execute(input.clone()).await;
                    println!("    -> {}", result.lines().next().unwrap_or(""));
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": result
                    }));
                }
            }

            messages.push(learn_claude_code_rust::Message {
                role: "user".to_string(),
                content: json!(results),
            });
        } else {
            println!();
            for block in &response.content {
                if let learn_claude_code_rust::ContentBlock::Text { text } = block {
                    println!("Claude: {}", text);
                }
            }
            break;
        }
    }
}
