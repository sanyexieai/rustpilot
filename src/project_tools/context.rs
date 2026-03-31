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
        let state_root = Self::scoped_state_root_for(&repo_root);
        Self::with_state_root(repo_root, state_root)
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

    pub fn scoped_state_root_for(repo_root: &Path) -> PathBuf {
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
                return repo_root
                    .join(".tenants")
                    .join(sanitize_scope_segment(&tenant_id))
                    .join("users")
                    .join(sanitize_scope_segment(&user_id));
            }
            return repo_root
                .join(".tenants")
                .join(sanitize_scope_segment(&tenant_id));
        }
        repo_root.to_path_buf()
    }

    pub fn scoped_team_dir_for(repo_root: &Path) -> PathBuf {
        Self::scoped_state_root_for(repo_root).join(".team")
    }

    fn with_state_root(repo_root: PathBuf, state_root: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&state_root)?;
        let team_dir = state_root.join(".team");
        let global_team_dir = repo_root.join(".team");
        migrate_legacy_scoped_identity_dir(&team_dir, &global_team_dir)?;
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
        let identity = Arc::new(IdentityManager::new(global_team_dir.clone())?);
        let tenant_runtime_registry =
            Arc::new(TenantRuntimeRegistryManager::new(global_team_dir)?);
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

fn migrate_legacy_scoped_identity_dir(team_dir: &Path, global_team_dir: &Path) -> anyhow::Result<()> {
    let legacy_identity_dir = team_dir.join("identity");
    if !legacy_identity_dir.exists() {
        return Ok(());
    }

    let global_identity_dir = global_team_dir.join("identity");
    fs::create_dir_all(&global_identity_dir)?;
    for file_name in ["tenants.json", "users.json", "memberships.json", "tokens.json"] {
        let source = legacy_identity_dir.join(file_name);
        if !source.exists() {
            continue;
        }
        let target = global_identity_dir.join(file_name);
        if !target.exists() {
            fs::rename(&source, &target).or_else(|_| {
                fs::copy(&source, &target)?;
                fs::remove_file(&source)
            })?;
        } else {
            fs::remove_file(&source)?;
        }
    }

    let is_empty = fs::read_dir(&legacy_identity_dir)?.next().is_none();
    if is_empty {
        fs::remove_dir(&legacy_identity_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ProjectContext;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn scoped_projects_share_global_identity_store() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let repo_root = std::env::temp_dir().join(format!("rustpilot-context-test-{unique}"));
        fs::create_dir_all(&repo_root).expect("create repo root");

        let root = ProjectContext::new(repo_root.clone()).expect("root context");
        let scoped =
            ProjectContext::for_user(repo_root.clone(), "tenant-a", "user-a").expect("scoped");

        let bootstrap = root
            .identity()
            .bootstrap_local_admin()
            .expect("bootstrap identity");

        let resolved = scoped
            .identity()
            .resolve_membership(&bootstrap.tenant.tenant_id, &bootstrap.user.user_id)
            .expect("resolve membership");

        assert!(resolved.is_some(), "scoped context should read global identity");
        assert!(repo_root.join(".team").join("identity").exists());
        assert!(
            !repo_root
                .join(".tenants")
                .join("tenant-a")
                .join("users")
                .join("user-a")
                .join(".team")
                .join("identity")
                .exists(),
            "scoped state root should not create a second identity store"
        );

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn scoped_projects_migrate_legacy_identity_dir_to_global_store() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let repo_root = std::env::temp_dir().join(format!("rustpilot-context-test-{unique}"));
        let legacy_identity_dir = repo_root
            .join(".tenants")
            .join("tenant-a")
            .join("users")
            .join("user-a")
            .join(".team")
            .join("identity");
        fs::create_dir_all(&legacy_identity_dir).expect("create legacy identity dir");
        fs::write(
            legacy_identity_dir.join("users.json"),
            "[{\"user_id\":\"legacy-user\",\"display_name\":\"Legacy User\",\"created_at\":1,\"disabled\":false}]",
        )
        .expect("write legacy users");

        let scoped =
            ProjectContext::for_user(repo_root.clone(), "tenant-a", "user-a").expect("scoped");

        let resolved = scoped
            .identity()
            .resolve_membership("default", "local-admin")
            .expect("resolve membership");
        assert!(resolved.is_none(), "migration should not invent memberships");
        assert!(
            repo_root.join(".team").join("identity").join("users.json").exists(),
            "global identity store should receive migrated files"
        );
        assert!(
            !legacy_identity_dir.exists(),
            "legacy scoped identity dir should be removed after migration"
        );

        let _ = fs::remove_dir_all(&repo_root);
    }
}
