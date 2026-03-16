mod agent;
mod approval;
mod budget;
mod context;
mod decision;
mod event;
mod launch_registry;
mod mailbox;
mod prompt_history;
mod proposal;
mod reflection;
mod resident_config;
mod resident_runtime;
mod session;
mod system_model;
mod task;
mod tools;
mod ui_page;
mod ui_schema;
mod ui_surface;
mod util;
mod worktree;

pub use agent::{AgentManager, AgentProfile, AgentState};
pub use approval::{ApprovalBlockRecord, ApprovalManager, ApprovalMode, ApprovalPolicy};
pub use budget::{BudgetLedger, BudgetManager, EnergyMode, classify_energy};
pub use context::ProjectContext;
pub use decision::{DecisionManager, DecisionRecord};
pub use event::{EventBus, EventRecord};
pub use launch_registry::{LaunchRecord, LaunchRegistryManager, LaunchRequest};
pub use mailbox::{MailRecord, Mailbox};
pub use prompt_history::{PromptChangeRecord, PromptHistoryManager};
pub use proposal::{ProposalManager, ProposalRecord};
pub use reflection::{ReflectionManager, ReflectionRecord};
pub use resident_config::{ResidentAgentConfig, ResidentConfigManager};
pub use resident_runtime::{ResidentRuntimeManager, ResidentRuntimeState};
pub use session::{SessionManager, SessionRecord};
pub use system_model::{
    SystemAlert, SystemDecision, SystemLaunch, SystemModel, SystemModelManager, SystemPromptChange,
    SystemProposal, SystemProtocol, SystemResident, SystemSummary, SystemTask,
};
pub use task::{TaskCreateOptions, TaskManager, TaskRecord, task_priority_rank};
pub use tools::{handle_project_tool_call, project_tool_definitions};
pub use ui_page::{
    UiDesignRules, UiPage as GeneratedUiPage, UiPageContext, UiPageManager, UiUserIntentMemory,
};
pub use ui_schema::{UiSchema, UiSchemaManager, UiSection};
pub use ui_surface::{UiAction, UiPage as UiSurfacePage, UiSurface, UiSurfaceManager};
pub use worktree::{WorktreeManager, WorktreeRecord};
