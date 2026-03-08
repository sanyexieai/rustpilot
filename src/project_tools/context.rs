use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{EventBus, Mailbox, TaskManager, WorktreeManager};

#[derive(Debug, Clone)]
pub struct ProjectContext {
    repo_root: PathBuf,
    tasks: Arc<TaskManager>,
    events: Arc<EventBus>,
    mailbox: Arc<Mailbox>,
    worktrees: Arc<WorktreeManager>,
}

impl ProjectContext {
    pub fn new(repo_root: PathBuf) -> anyhow::Result<Self> {
        let tasks = Arc::new(TaskManager::new(repo_root.join(".tasks"))?);
        let events = Arc::new(EventBus::new(
            repo_root.join(".worktrees").join("events.jsonl"),
        )?);
        let mailbox = Arc::new(Mailbox::new(repo_root.join(".team").join("mailbox"))?);
        let worktrees = Arc::new(WorktreeManager::new(
            repo_root.clone(),
            (*tasks).clone(),
            (*events).clone(),
        )?);
        Ok(Self {
            repo_root,
            tasks,
            events,
            mailbox,
            worktrees,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn tasks(&self) -> &TaskManager {
        self.tasks.as_ref()
    }

    pub fn events(&self) -> &EventBus {
        self.events.as_ref()
    }

    pub fn worktrees(&self) -> &WorktreeManager {
        self.worktrees.as_ref()
    }

    pub fn mailbox(&self) -> &Mailbox {
        self.mailbox.as_ref()
    }
}
