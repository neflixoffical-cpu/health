//! health-tools — built-in tools: read/write/edit files, list, glob, grep, shell, web fetch, todos.
//!
//! [`BuiltinTools`] is a [`ToolProvider`]; the shell tool follows the configured shell
//! (`Config.shell`), so its description always names the shell actually used.

mod fs_tools;
mod search;
mod shell;
mod util;
mod web;
mod todo;

pub use shell::{find_shell, ShellProgram};
pub use todo::TodoStore;

use health_core::{SharedConfig, ShellKind, Tool, ToolProvider};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// All built-in tools.
pub struct BuiltinTools {
    config: SharedConfig,
    fixed: Vec<Arc<dyn Tool>>,
    todo: Arc<TodoStore>,
    shells: Mutex<HashMap<ShellKind, Arc<dyn Tool>>>,
}

impl BuiltinTools {
    pub fn new(config: SharedConfig) -> Arc<BuiltinTools> {
        let todo = Arc::new(TodoStore::default());
        let fixed: Vec<Arc<dyn Tool>> = vec![
            Arc::new(fs_tools::ReadFile),
            Arc::new(fs_tools::WriteFile),
            Arc::new(fs_tools::EditFile),
            Arc::new(fs_tools::ListDir),
            Arc::new(search::Glob),
            Arc::new(search::Grep),
            Arc::new(web::WebFetch::new()),
            Arc::new(todo::TodoWrite(todo.clone())),
        ];
        Arc::new(BuiltinTools { config, fixed, todo, shells: Mutex::new(HashMap::new()) })
    }

    /// Current todo list (for the UI task panel).
    pub fn todos(&self) -> Arc<TodoStore> {
        self.todo.clone()
    }
}

impl ToolProvider for BuiltinTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let kind = self.config.read().shell;
        let shell = self.shells.lock().entry(kind).or_insert_with(|| Arc::new(shell::Shell::new(kind)) as Arc<dyn Tool>).clone();
        let mut v = self.fixed.clone();
        v.insert(6, shell);
        v
    }
}

/// Convenience for tests and simple embedding: the default tool list (PowerShell shell).
pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    let cfg: SharedConfig = Arc::new(parking_lot::RwLock::new(health_core::Config::default()));
    BuiltinTools::new(cfg).tools()
}
