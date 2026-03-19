#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifiedRootRequest {
    Direct,
    DelegateArtifactBuild,
    DelegateSkillAuthoring,
}

#[derive(Debug, Clone)]
pub struct RootRequestConfig {
    build_verbs: Vec<String>,
    skill_tokens: Vec<String>,
    artifact_tokens: Vec<String>,
}

impl Default for RootRequestConfig {
    fn default() -> Self {
        Self {
            build_verbs: env_csv(
                "RUSTPILOT_ROOT_BUILD_VERBS",
                &[
                    "write ",
                    "create ",
                    "generate ",
                    "implement ",
                    "build ",
                    "make ",
                    "scaffold ",
                    "写",
                    "写一个",
                    "做一个",
                    "做个",
                    "创建",
                    "生成",
                    "实现",
                ],
            ),
            skill_tokens: env_csv("RUSTPILOT_ROOT_SKILL_TOKENS", &["skill", "skills", "技能"]),
            artifact_tokens: env_csv(
                "RUSTPILOT_ROOT_ARTIFACT_TOKENS",
                &[
                    "html",
                    "css",
                    "javascript",
                    "typescript",
                    "react",
                    "vue",
                    "component",
                    "page",
                    "login page",
                    "single file",
                    "file",
                    "页面",
                    "登录页",
                    "单文件",
                    "组件",
                    "网页",
                    "脚本",
                    "文件",
                ],
            ),
        }
    }
}

impl RootRequestConfig {
    pub fn load() -> Self {
        Self::default()
    }

    pub fn classify_lowered(&self, lowered_input: &str) -> ClassifiedRootRequest {
        if !contains_any_token(lowered_input, &self.build_verbs) {
            return ClassifiedRootRequest::Direct;
        }
        if contains_any_token(lowered_input, &self.skill_tokens) {
            return ClassifiedRootRequest::DelegateSkillAuthoring;
        }
        if contains_any_token(lowered_input, &self.artifact_tokens) {
            return ClassifiedRootRequest::DelegateArtifactBuild;
        }
        ClassifiedRootRequest::Direct
    }
}

fn contains_any_token(haystack: &str, tokens: &[String]) -> bool {
    tokens.iter().any(|token| haystack.contains(token))
}

fn env_csv(key: &str, defaults: &[&str]) -> Vec<String> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !parsed.is_empty() {
        return parsed;
    }
    defaults.iter().map(|value| value.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::{ClassifiedRootRequest, RootRequestConfig};

    #[test]
    fn classifies_skill_requests() {
        let config = RootRequestConfig::load();
        assert_eq!(
            config.classify_lowered("create a skill for browser automation"),
            ClassifiedRootRequest::DelegateSkillAuthoring
        );
    }

    #[test]
    fn classifies_artifact_requests() {
        let config = RootRequestConfig::load();
        assert_eq!(
            config.classify_lowered("create a react login page"),
            ClassifiedRootRequest::DelegateArtifactBuild
        );
    }
}
