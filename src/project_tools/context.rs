use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    AgentManager, ApprovalManager, BudgetManager, DecisionManager, EventBus, IdentityManager,
    LaunchRegistryManager, LaunchSettingsManager, Mailbox, PromptHistoryManager, ProposalManager,
    ReflectionManager, ResidentConfigManager, ResidentRuntimeManager, SessionManager,
    SystemModelManager, TaskManager, TenantRuntimeRegistryManager, UiPageManager, UiSchemaManager,
    UiSurfaceManager, WorktreeManager,
};

#[derive(Debug, Clone)]
pub struct ProjectContext {
    repo_root: PathBuf,
    state_root: PathBuf,
    tasks: Arc<TaskManager>,
    approval: Arc<ApprovalManager>,
    events: Arc<EventBus>,
    launches: Arc<LaunchRegistryManager>,
    launch_settings: Arc<LaunchSettingsManager>,
    mailbox: Arc<Mailbox>,
    agents: Arc<AgentManager>,
    budgets: Arc<BudgetManager>,
    decisions: Arc<DecisionManager>,
    reflections: Arc<ReflectionManager>,
    sessions: Arc<SessionManager>,
    residents: Arc<ResidentConfigManager>,
    resident_runtime: Arc<ResidentRuntimeManager>,
    proposals: Arc<ProposalManager>,
    identity: Arc<IdentityManager>,
    tenant_runtime_registry: Arc<TenantRuntimeRegistryManager>,
    prompt_history: Arc<PromptHistoryManager>,
    system_model: Arc<SystemModelManager>,
    ui_surface: Arc<UiSurfaceManager>,
    ui_schema: Arc<UiSchemaManager>,
    ui_page: Arc<UiPageManager>,
    worktrees: Arc<WorktreeManager>,
}

impl ProjectContext {
    pub fn new(repo_root: PathBuf) -> anyhow::Result<Self> {
        if let Some(tenant_id) = std::env::var("RUSTPILOT_TENANT_ID")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            if let Some(user_id) = std::env::var("RUSTPILOT_USER_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                return Self::for_user(repo_root, &tenant_id, &user_id);
            }
            return Self::for_tenant(repo_root, &tenant_id);
        }
        Self::with_state_root(repo_root.clone(), repo_root)
    }

    pub fn for_tenant(repo_root: PathBuf, tenant_id: &str) -> anyhow::Result<Self> {
        let state_root = repo_root
            .join(".tenants")
            .join(sanitize_scope_segment(tenant_id));
        Self::with_state_root(repo_root, state_root)
    }

    pub fn for_user(repo_root: PathBuf, tenant_id: &str, user_id: &str) -> anyhow::Result<Self> {
        let state_root = repo_root
            .join(".tenants")
            .join(sanitize_scope_segment(tenant_id))
            .join("users")
            .join(sanitize_scope_segment(user_id));
        Self::with_state_root(repo_root, state_root)
    }

    fn with_state_root(repo_root: PathBuf, state_root: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&state_root)?;
        let team_dir = state_root.join(".team");
        let tasks_dir = state_root.join(".tasks");
        let worktrees_dir = state_root.join(".worktrees");
        let tasks = Arc::new(TaskManager::new(tasks_dir)?);
        let approval = Arc::new(ApprovalManager::new(team_dir.clone())?);
        let events = Arc::new(EventBus::new(worktrees_dir.join("events.jsonl"))?);
        let launches = Arc::new(LaunchRegistryManager::new(team_dir.clone())?);
        let launch_settings = Arc::new(LaunchSettingsManager::new(team_dir.clone())?);
        let mailbox = Arc::new(Mailbox::new(team_dir.join("mailbox"))?);
        let agents = Arc::new(AgentManager::new(team_dir.clone())?);
        let budgets = Arc::new(BudgetManager::new(team_dir.clone())?);
        let decisions = Arc::new(DecisionManager::new(team_dir.clone())?);
        let reflections = Arc::new(ReflectionManager::new(team_dir.clone())?);
        let sessions = Arc::new(SessionManager::new(team_dir.clone())?);
        let residents = Arc::new(ResidentConfigManager::new(team_dir.clone())?);
        let resident_runtime = Arc::new(ResidentRuntimeManager::new(team_dir.clone())?);
        let proposals = Arc::new(ProposalManager::new(team_dir.clone())?);
        let identity = Arc::new(IdentityManager::new(team_dir.clone())?);
        let tenant_runtime_registry =
            Arc::new(TenantRuntimeRegistryManager::new(repo_root.join(".team"))?);
        let prompt_history = Arc::new(PromptHistoryManager::new(team_dir.clone())?);
        let system_model = Arc::new(SystemModelManager::new(team_dir.clone())?);
        let ui_surface = Arc::new(UiSurfaceManager::new(team_dir.clone())?);
        let ui_schema = Arc::new(UiSchemaManager::new(team_dir.clone())?);
        let ui_page = Arc::new(UiPageManager::new(team_dir.clone())?);
        let worktrees = Arc::new(WorktreeManager::new(
            repo_root.clone(),
            worktrees_dir,
            (*tasks).clone(),
            (*events).clone(),
        )?);
        Ok(Self {
            repo_root,
            state_root,
            tasks,
            approval,
            events,
            launches,
            launch_settings,
            mailbox,
            agents,
            budgets,
            decisions,
            reflections,
            sessions,
            residents,
            resident_runtime,
            proposals,
            identity,
            tenant_runtime_registry,
            prompt_history,
            system_model,
            ui_surface,
            ui_schema,
            ui_page,
            worktrees,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn tasks(&self) -> &TaskManager {
        self.tasks.as_ref()
    }

    pub fn approval(&self) -> &ApprovalManager {
        self.approval.as_ref()
    }

    pub fn events(&self) -> &EventBus {
        self.events.as_ref()
    }

    pub fn worktrees(&self) -> &WorktreeManager {
        self.worktrees.as_ref()
    }

    pub fn launches(&self) -> &LaunchRegistryManager {
        self.launches.as_ref()
    }

    pub fn launch_settings(&self) -> &LaunchSettingsManager {
        self.launch_settings.as_ref()
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

    pub fn sessions(&self) -> &SessionManager {
        self.sessions.as_ref()
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

    pub fn identity(&self) -> &IdentityManager {
        self.identity.as_ref()
    }

    pub fn tenant_runtime_registry(&self) -> &TenantRuntimeRegistryManager {
        self.tenant_runtime_registry.as_ref()
    }

    pub fn system_model(&self) -> &SystemModelManager {
        self.system_model.as_ref()
    }

    pub fn prompt_history(&self) -> &PromptHistoryManager {
        self.prompt_history.as_ref()
    }

    pub fn ui_schema(&self) -> &UiSchemaManager {
        self.ui_schema.as_ref()
    }

    pub fn ui_surface(&self) -> &UiSurfaceManager {
        self.ui_surface.as_ref()
    }

    pub fn ui_page(&self) -> &UiPageManager {
        self.ui_page.as_ref()
    }
}

fn sanitize_scope_segment(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
