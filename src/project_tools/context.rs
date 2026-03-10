use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    AgentManager, BudgetManager, DecisionManager, EventBus, Mailbox, ProposalManager,
    ReflectionManager, ResidentConfigManager, ResidentRuntimeManager, SystemModelManager,
    TaskManager, UiSchemaManager, UiSurfaceManager, WorktreeManager,
};

#[derive(Debug, Clone)]
pub struct ProjectContext {
    repo_root: PathBuf,
    tasks: Arc<TaskManager>,
    events: Arc<EventBus>,
    mailbox: Arc<Mailbox>,
    agents: Arc<AgentManager>,
    budgets: Arc<BudgetManager>,
    decisions: Arc<DecisionManager>,
    reflections: Arc<ReflectionManager>,
    residents: Arc<ResidentConfigManager>,
    resident_runtime: Arc<ResidentRuntimeManager>,
    proposals: Arc<ProposalManager>,
    system_model: Arc<SystemModelManager>,
    ui_surface: Arc<UiSurfaceManager>,
    ui_schema: Arc<UiSchemaManager>,
    worktrees: Arc<WorktreeManager>,
}

impl ProjectContext {
    pub fn new(repo_root: PathBuf) -> anyhow::Result<Self> {
        let tasks = Arc::new(TaskManager::new(repo_root.join(".tasks"))?);
        let events = Arc::new(EventBus::new(
            repo_root.join(".worktrees").join("events.jsonl"),
        )?);
        let mailbox = Arc::new(Mailbox::new(repo_root.join(".team").join("mailbox"))?);
        let agents = Arc::new(AgentManager::new(repo_root.join(".team"))?);
        let budgets = Arc::new(BudgetManager::new(repo_root.join(".team"))?);
        let decisions = Arc::new(DecisionManager::new(repo_root.join(".team"))?);
        let reflections = Arc::new(ReflectionManager::new(repo_root.join(".team"))?);
        let residents = Arc::new(ResidentConfigManager::new(repo_root.join(".team"))?);
        let resident_runtime = Arc::new(ResidentRuntimeManager::new(repo_root.join(".team"))?);
        let proposals = Arc::new(ProposalManager::new(repo_root.join(".team"))?);
        let system_model = Arc::new(SystemModelManager::new(repo_root.join(".team"))?);
        let ui_surface = Arc::new(UiSurfaceManager::new(repo_root.join(".team"))?);
        let ui_schema = Arc::new(UiSchemaManager::new(repo_root.join(".team"))?);
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
            agents,
            budgets,
            decisions,
            reflections,
            residents,
            resident_runtime,
            proposals,
            system_model,
            ui_surface,
            ui_schema,
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

    pub fn agents(&self) -> &AgentManager {
        self.agents.as_ref()
    }

    pub fn budgets(&self) -> &BudgetManager {
        self.budgets.as_ref()
    }

    pub fn reflections(&self) -> &ReflectionManager {
        self.reflections.as_ref()
    }

    pub fn decisions(&self) -> &DecisionManager {
        self.decisions.as_ref()
    }

    pub fn residents(&self) -> &ResidentConfigManager {
        self.residents.as_ref()
    }

    pub fn resident_runtime(&self) -> &ResidentRuntimeManager {
        self.resident_runtime.as_ref()
    }

    pub fn proposals(&self) -> &ProposalManager {
        self.proposals.as_ref()
    }

    pub fn system_model(&self) -> &SystemModelManager {
        self.system_model.as_ref()
    }

    pub fn ui_schema(&self) -> &UiSchemaManager {
        self.ui_schema.as_ref()
    }

    pub fn ui_surface(&self) -> &UiSurfaceManager {
        self.ui_surface.as_ref()
    }
}
