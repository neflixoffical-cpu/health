//! `todo_write`: the model keeps a task list; the UI shows it.

use crate::util::*;
use async_trait::async_trait;
use health_core::{t, TodoItem, TodoStatus, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::sync::Arc;

/// Shared todo list (read by the UI).
#[derive(Default)]
pub struct TodoStore {
    items: RwLock<Vec<TodoItem>>,
}

impl TodoStore {
    pub fn get(&self) -> Vec<TodoItem> {
        self.items.read().clone()
    }
    pub fn clear(&self) {
        self.items.write().clear();
    }
}

pub struct TodoWrite(pub Arc<TodoStore>);

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &str {
        "todo_write"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo_write".into(),
            description: "Replace the task list shown to the user. Each item has content and a status (pending, in_progress, completed).".into(),
            input_schema: schema(
                json!({
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["content", "status"],
                            "additionalProperties": false
                        }
                    }
                }),
                &["todos"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, _: &Value) -> String {
        t!("Список задач", "Todo list").to_string()
    }
    async fn run(&self, input: Value, _ctx: ToolCtx) -> ToolOutput {
        let Some(arr) = input.get("todos").and_then(|v| v.as_array()) else {
            return ToolOutput::error("`todos` must be an array.");
        };
        let mut items = Vec::new();
        for (i, v) in arr.iter().enumerate() {
            let content = v.get("content").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if content.is_empty() {
                return ToolOutput::error(format!("Item {i} has empty content."));
            }
            let status = match v.get("status").and_then(|x| x.as_str()).unwrap_or("pending") {
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => TodoStatus::Pending,
            };
            items.push(TodoItem { content, status });
        }
        let done = items.iter().filter(|i| i.status == TodoStatus::Completed).count();
        let active = items.iter().filter(|i| i.status == TodoStatus::InProgress).count();
        let total = items.len();
        *self.0.items.write() = items.clone();
        ToolOutput::ok(format!("Todo list updated: {total} items ({done} completed, {active} in progress)."))
            .with_display(ToolDisplay::Todos { items })
    }
}
