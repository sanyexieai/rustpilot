use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRecord {
    pub tenant_id: String,
    pub display_name: String,
    pub created_at: u64,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub user_id: String,
    pub display_name: String,
    pub created_at: u64,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipRecord {
    pub tenant_id: String,
    pub user_id: String,
    pub role: String,
    pub created_at: u64,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTokenRecord {
    pub token_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub label: String,
    pub secret: String,
    pub created_at: u64,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub tenant_id: String,
    pub user_id: String,
    pub role: String,
    pub auth_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapIdentity {
    pub tenant: TenantRecord,
    pub user: UserRecord,
    pub membership: MembershipRecord,
    pub token: ApiTokenRecord,
}

#[derive(Debug, Clone)]
pub struct IdentityManager {
    dir: PathBuf,
}

impl IdentityManager {
    pub fn new(team_dir: PathBuf) -> anyhow::Result<Self> {
        let dir = team_dir.join("identity");
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn bootstrap_local_admin(&self) -> anyhow::Result<BootstrapIdentity> {
        let tenant = self.ensure_tenant("default", "Default")?;
        let user = self.ensure_user("local-admin", "Local Admin")?;
        let membership = self.ensure_membership(&tenant.tenant_id, &user.user_id, "owner")?;
        let token = self.ensure_token(
            &tenant.tenant_id,
            &user.user_id,
            "local-dev",
            Some("rp_local_default_token"),
        )?;
        Ok(BootstrapIdentity {
            tenant,
            user,
            membership,
            token,
        })
    }

    pub fn resolve_token(&self, secret: &str) -> anyhow::Result<Option<AuthContext>> {
        let tokens = self.load_tokens()?;
        let memberships = self.load_memberships()?;
        let tenants = self.load_tenants()?;
        let users = self.load_users()?;
        let Some(token) = tokens
            .into_iter()
            .find(|item| !item.disabled && item.secret == secret)
        else {
            return Ok(None);
        };
        self.build_auth_context(
            &token.tenant_id,
            &token.user_id,
            "token",
            &memberships,
            &tenants,
            &users,
        )
        .map(Some)
    }

    pub fn resolve_membership(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> anyhow::Result<Option<AuthContext>> {
        let memberships = self.load_memberships()?;
        let tenants = self.load_tenants()?;
        let users = self.load_users()?;
        self.build_auth_context(tenant_id, user_id, "header", &memberships, &tenants, &users)
            .map(Some)
            .or_else(|err| {
                if err.to_string().contains("membership not found")
                    || err.to_string().contains("tenant not found")
                    || err.to_string().contains("user not found")
                {
                    Ok(None)
                } else {
                    Err(err)
                }
            })
    }

    fn build_auth_context(
        &self,
        tenant_id: &str,
        user_id: &str,
        auth_mode: &str,
        memberships: &[MembershipRecord],
        tenants: &[TenantRecord],
        users: &[UserRecord],
    ) -> anyhow::Result<AuthContext> {
        let tenant = tenants
            .iter()
            .find(|item| item.tenant_id == tenant_id && !item.disabled)
            .ok_or_else(|| anyhow::anyhow!("tenant not found: {}", tenant_id))?;
        let user = users
            .iter()
            .find(|item| item.user_id == user_id && !item.disabled)
            .ok_or_else(|| anyhow::anyhow!("user not found: {}", user_id))?;
        let membership = memberships
            .iter()
            .find(|item| {
                item.tenant_id == tenant.tenant_id && item.user_id == user.user_id && !item.disabled
            })
            .ok_or_else(|| anyhow::anyhow!("membership not found: {}/{}", tenant_id, user_id))?;
        Ok(AuthContext {
            tenant_id: tenant.tenant_id.clone(),
            user_id: user.user_id.clone(),
            role: membership.role.clone(),
            auth_mode: auth_mode.to_string(),
        })
    }

    fn ensure_tenant(&self, tenant_id: &str, display_name: &str) -> anyhow::Result<TenantRecord> {
        let mut items = self.load_tenants()?;
        if let Some(existing) = items.iter().find(|item| item.tenant_id == tenant_id) {
            return Ok(existing.clone());
        }
        let record = TenantRecord {
            tenant_id: tenant_id.to_string(),
            display_name: display_name.to_string(),
            created_at: now_secs(),
            disabled: false,
        };
        items.push(record.clone());
        self.save_tenants(&items)?;
        Ok(record)
    }

    fn ensure_user(&self, user_id: &str, display_name: &str) -> anyhow::Result<UserRecord> {
        let mut items = self.load_users()?;
        if let Some(existing) = items.iter().find(|item| item.user_id == user_id) {
            return Ok(existing.clone());
        }
        let record = UserRecord {
            user_id: user_id.to_string(),
            display_name: display_name.to_string(),
            created_at: now_secs(),
            disabled: false,
        };
        items.push(record.clone());
        self.save_users(&items)?;
        Ok(record)
    }

    fn ensure_membership(
        &self,
        tenant_id: &str,
        user_id: &str,
        role: &str,
    ) -> anyhow::Result<MembershipRecord> {
        let mut items = self.load_memberships()?;
        if let Some(existing) = items
            .iter()
            .find(|item| item.tenant_id == tenant_id && item.user_id == user_id)
        {
            return Ok(existing.clone());
        }
        let record = MembershipRecord {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            role: role.to_string(),
            created_at: now_secs(),
            disabled: false,
        };
        items.push(record.clone());
        self.save_memberships(&items)?;
        Ok(record)
    }

    fn ensure_token(
        &self,
        tenant_id: &str,
        user_id: &str,
        label: &str,
        fixed_secret: Option<&str>,
    ) -> anyhow::Result<ApiTokenRecord> {
        let mut items = self.load_tokens()?;
        if let Some(existing) = items.iter().find(|item| {
            item.tenant_id == tenant_id && item.user_id == user_id && item.label == label
        }) {
            return Ok(existing.clone());
        }
        let now = now_secs();
        let record = ApiTokenRecord {
            token_id: format!("token-{}", now),
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            label: label.to_string(),
            secret: fixed_secret
                .map(ToString::to_string)
                .unwrap_or_else(generate_token_secret),
            created_at: now,
            disabled: false,
        };
        items.push(record.clone());
        self.save_tokens(&items)?;
        Ok(record)
    }

    fn tenants_path(&self) -> PathBuf {
        self.dir.join("tenants.json")
    }

    fn users_path(&self) -> PathBuf {
        self.dir.join("users.json")
    }

    fn memberships_path(&self) -> PathBuf {
        self.dir.join("memberships.json")
    }

    fn tokens_path(&self) -> PathBuf {
        self.dir.join("tokens.json")
    }

    fn load_tenants(&self) -> anyhow::Result<Vec<TenantRecord>> {
        load_vec(self.tenants_path())
    }

    fn save_tenants(&self, items: &[TenantRecord]) -> anyhow::Result<()> {
        save_vec(self.tenants_path(), items)
    }

    fn load_users(&self) -> anyhow::Result<Vec<UserRecord>> {
        load_vec(self.users_path())
    }

    fn save_users(&self, items: &[UserRecord]) -> anyhow::Result<()> {
        save_vec(self.users_path(), items)
    }

    fn load_memberships(&self) -> anyhow::Result<Vec<MembershipRecord>> {
        load_vec(self.memberships_path())
    }

    fn save_memberships(&self, items: &[MembershipRecord]) -> anyhow::Result<()> {
        save_vec(self.memberships_path(), items)
    }

    fn load_tokens(&self) -> anyhow::Result<Vec<ApiTokenRecord>> {
        load_vec(self.tokens_path())
    }

    fn save_tokens(&self, items: &[ApiTokenRecord]) -> anyhow::Result<()> {
        save_vec(self.tokens_path(), items)
    }
}

fn load_vec<T>(path: PathBuf) -> anyhow::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&content)?)
}

fn save_vec<T>(path: PathBuf, items: &[T]) -> anyhow::Result<()>
where
    T: Serialize,
{
    fs::write(path, serde_json::to_string_pretty(items)?)?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn generate_token_secret() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("rp_{:x}_{:x}", std::process::id(), now)
}
