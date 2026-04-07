// Tool Templates — Copy and customize these for your agent.
//
// Each tool needs:
//   1. A struct implementing the Tool trait
//   2. fn name() -> &str
//   3. fn definition() -> Value  (JSON schema for the model)
//   4. async fn execute(input: Value) -> String

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// ── Tool trait ────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> Value;
    async fn execute(&self, input: Value) -> String;
}

// ── BashTool ──────────────────────────────────────────────────────────────────

pub struct BashTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn definition(&self) -> Value {
        json!({
            "name": "bash",
            "description": "Run a shell command. Use for: ls, find, grep, git, cargo, etc.",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string", "description": "Shell command to execute" } },
                "required": ["command"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let cmd = input["command"].as_str().unwrap_or("");
        // Block dangerous commands
        for danger in &["rm -rf /", "sudo rm", "shutdown", "> /dev/"] {
            if cmd.contains(danger) { return "Error: Dangerous command blocked".into(); }
        }
        match std::process::Command::new("sh")
            .arg("-c").arg(cmd).current_dir(&self.cwd).output()
        {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).to_string()
                    + &String::from_utf8_lossy(&o.stderr);
                if out.is_empty() { "(no output)".into() } else { out[..out.len().min(50000)].into() }
            }
            Err(e) => format!("Error: {}", e),
        }
    }
}

// ── ReadFileTool ──────────────────────────────────────────────────────────────

pub struct ReadFileTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "read_file",
            "description": "Read file contents. Returns UTF-8 text.",
            "input_schema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Relative path to the file" } },
                "required": ["path"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let rel = input["path"].as_str().unwrap_or("");
        let path = self.safe_path(rel);
        match path {
            Ok(p) => std::fs::read_to_string(&p)
                .map(|s| s[..s.len().min(50000)].to_string())
                .unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => e,
        }
    }
}

impl ReadFileTool {
    fn safe_path(&self, rel: &str) -> Result<PathBuf, String> {
        let p = self.cwd.join(rel).canonicalize()
            .map_err(|e| format!("Error: {}", e))?;
        if !p.starts_with(&self.cwd) {
            return Err(format!("Error: Path escapes workspace: {}", rel));
        }
        Ok(p)
    }
}

// ── WriteFileTool ─────────────────────────────────────────────────────────────

pub struct WriteFileTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "write_file",
            "description": "Write content to a file. Creates parent directories if needed.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let rel = input["path"].as_str().unwrap_or("");
        let content = input["content"].as_str().unwrap_or("");
        let path = self.cwd.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, content)
            .map(|_| format!("Wrote {} bytes to {}", content.len(), rel))
            .unwrap_or_else(|e| format!("Error: {}", e))
    }
}

// ── EditFileTool ──────────────────────────────────────────────────────────────

pub struct EditFileTool {
    pub cwd: PathBuf,
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }
    fn definition(&self) -> Value {
        json!({
            "name": "edit_file",
            "description": "Replace exact text in a file. Use for surgical edits.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string", "description": "Exact text to find (must match precisely)" },
                    "new_text": { "type": "string", "description": "Replacement text" }
                },
                "required": ["path", "old_text", "new_text"]
            }
        })
    }
    async fn execute(&self, input: Value) -> String {
        let rel = input["path"].as_str().unwrap_or("");
        let old = input["old_text"].as_str().unwrap_or("");
        let new = input["new_text"].as_str().unwrap_or("");
        let path = self.cwd.join(rel);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                if !content.contains(old) {
                    return format!("Error: Text not found in {}", rel);
                }
                let updated = content.replacen(old, new, 1);
                std::fs::write(&path, updated)
                    .map(|_| format!("Edited {}", rel))
                    .unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        }
    }
}

// ── Tool dispatch map (usage example) ────────────────────────────────────────

// use std::collections::HashMap;
//
// fn build_tools(cwd: PathBuf) -> HashMap<String, Box<dyn Tool>> {
//     let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
//     tools.insert("bash".into(),       Box::new(BashTool      { cwd: cwd.clone() }));
//     tools.insert("read_file".into(),  Box::new(ReadFileTool  { cwd: cwd.clone() }));
//     tools.insert("write_file".into(), Box::new(WriteFileTool { cwd: cwd.clone() }));
//     tools.insert("edit_file".into(),  Box::new(EditFileTool  { cwd: cwd.clone() }));
//     tools
// }
