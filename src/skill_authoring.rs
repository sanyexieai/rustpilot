use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillAuthoringConfig {
    dir_candidates: Vec<PathBuf>,
    skill_file_name: String,
    tests_dir_name: String,
    smoke_test_file_name: String,
    integration_test_file_name: String,
    smoke_description_len_cap: usize,
    smoke_body_min_floor: usize,
    smoke_response_min_length: usize,
    smoke_keyword_min_len: usize,
}

impl Default for SkillAuthoringConfig {
    fn default() -> Self {
        Self {
            dir_candidates: default_dir_candidates(),
            skill_file_name: "SKILL.md".to_string(),
            tests_dir_name: "tests".to_string(),
            smoke_test_file_name: "smoke.json".to_string(),
            integration_test_file_name: "integration.py".to_string(),
            smoke_description_len_cap: env_usize("RUSTPILOT_SKILL_DESCRIPTION_MIN_CAP", 5),
            smoke_body_min_floor: env_usize("RUSTPILOT_SKILL_BODY_MIN_FLOOR", 20),
            smoke_response_min_length: env_usize("RUSTPILOT_SKILL_RESPONSE_MIN_LENGTH", 100),
            smoke_keyword_min_len: env_usize("RUSTPILOT_SKILL_KEYWORD_MIN_LEN", 2),
        }
    }
}

impl SkillAuthoringConfig {
    pub fn load() -> Self {
        Self::default()
    }

    pub fn resolve_existing_skills_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(explicit) = env_skills_dir() {
            return Ok(explicit);
        }

        let cwd = std::env::current_dir()?;
        for candidate in &self.dir_candidates {
            let path = cwd.join(candidate);
            if path.is_dir() {
                return Ok(path);
            }
        }

        anyhow::bail!(
            "skills directory not found (checked: {})",
            self.dir_candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    pub fn resolve_or_create_skills_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(explicit) = env_skills_dir() {
            std::fs::create_dir_all(&explicit)?;
            return Ok(explicit);
        }

        let cwd = std::env::current_dir()?;
        let relative = self
            .dir_candidates
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("skills"));
        let path = cwd.join(relative);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    pub fn skill_file_name(&self) -> &str {
        &self.skill_file_name
    }

    pub fn tests_dir_name(&self) -> &str {
        &self.tests_dir_name
    }

    pub fn smoke_test_file_name(&self) -> &str {
        &self.smoke_test_file_name
    }

    pub fn integration_test_file_name(&self) -> &str {
        &self.integration_test_file_name
    }

    pub fn skill_file_path(&self, skill_dir: &Path) -> PathBuf {
        skill_dir.join(self.skill_file_name())
    }

    pub fn tests_dir_path(&self, skill_dir: &Path) -> PathBuf {
        skill_dir.join(self.tests_dir_name())
    }

    pub fn smoke_test_path(&self, skill_dir: &Path) -> PathBuf {
        self.tests_dir_path(skill_dir)
            .join(self.smoke_test_file_name())
    }

    pub fn integration_test_path(&self, skill_dir: &Path) -> PathBuf {
        self.tests_dir_path(skill_dir)
            .join(self.integration_test_file_name())
    }

    pub fn render_skill_markdown(&self, name: &str, description: &str, body: &str) -> String {
        let title = name
            .split('-')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => {
                        let mut output = first.to_uppercase().collect::<String>();
                        output.push_str(chars.as_str());
                        output
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        format!(
            "---\nname: {name}\ndescription: {description}\n---\n\n# {title}\n\n{}\n",
            body.trim()
        )
    }

    pub fn render_smoke_test(
        &self,
        description: &str,
        body: &str,
        test_prompt: Option<&str>,
        expect_response_contains: &[String],
    ) -> String {
        let first_keyword = body
            .split_whitespace()
            .find(|word| word.len() >= self.smoke_keyword_min_len)
            .unwrap_or("##");
        let desc_min = description.len().min(self.smoke_description_len_cap);
        let body_min = (body.trim().len() / 2).max(self.smoke_body_min_floor);

        let mut obj = serde_json::json!({
            "name": "smoke",
            "expect_description_min_length": desc_min,
            "expect_body_min_length": body_min,
            "expect_body_contains": first_keyword,
        });

        if let Some(prompt) = test_prompt {
            obj["prompt"] = serde_json::Value::String(prompt.to_string());
            if !expect_response_contains.is_empty() {
                obj["expect_response_contains"] = serde_json::json!(expect_response_contains);
            }
            obj["expect_response_min_length"] = serde_json::json!(self.smoke_response_min_length);
        }

        serde_json::to_string_pretty(&obj).unwrap_or_default() + "\n"
    }

    pub fn skill_create_summary(&self) -> String {
        format!(
            "writes {}, generates {}/{} and {}/{}, and runs LLM tests",
            self.skill_file_name(),
            self.tests_dir_name(),
            self.smoke_test_file_name(),
            self.tests_dir_name(),
            self.integration_test_file_name()
        )
    }

    pub fn skill_authoring_protocol_text(&self) -> String {
        format!(
            "Skill Authoring Protocol:\n\
Creating or updating a skill MUST be done exclusively via the `skill_create` tool. Never write {} or any files under skills/ directly with file-writing tools. \
The `skill_create` tool requires: `name`, `description`, `body`, `test_prompt` (a representative user request that exercises the skill's core capability), and `expect_response_contains` (keywords that must appear in the LLM response). \
If the skill already exists, `skill_create` updates {} and {}/{} in-place; any custom {} is preserved. \
After creation or update the tool automatically runs a live LLM test using the skill as system prompt; the skill is only considered complete when all tests pass. \
If the LLM test fails, revise the skill body or the test expectations and call `skill_create` again with a corrected version. \
For skills involving real execution (browser automation, API calls, etc.), write an integration test by editing {}/{} - a template is generated automatically. \
The integration test can pause for human interaction by calling wait_for_human('message'), which sends a mail notification; resume by calling `skill_test_signal` after completing the action. \
To validate an existing skill (all test tiers), use `skill_validate` with the skill name.",
            self.skill_file_name(),
            self.skill_file_name(),
            self.tests_dir_name(),
            self.smoke_test_file_name(),
            self.integration_test_file_name(),
            self.tests_dir_name(),
            self.integration_test_file_name()
        )
    }
}

fn default_dir_candidates() -> Vec<PathBuf> {
    if let Ok(raw) = std::env::var("RUSTPILOT_SKILL_DIR_CANDIDATES") {
        let parsed = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            return parsed;
        }
    }

    // 默认只支持仓库根目录下的 `skills/`，不再自动探测 `s05/skills` 等历史路径。
    vec![PathBuf::from("skills")]
}

fn env_skills_dir() -> Option<PathBuf> {
    std::env::var("SKILLS_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::SkillAuthoringConfig;

    #[test]
    fn protocol_mentions_configured_paths() {
        let config = SkillAuthoringConfig::load();
        let text = config.skill_authoring_protocol_text();
        assert!(text.contains(config.skill_file_name()));
        assert!(text.contains(config.smoke_test_file_name()));
        assert!(text.contains(config.integration_test_file_name()));
    }

    #[test]
    fn smoke_test_uses_configured_response_length() {
        let config = SkillAuthoringConfig::load();
        let rendered =
            config.render_smoke_test("desc", "body text", Some("prompt"), &["ok".to_string()]);
        assert!(rendered.contains("\"expect_response_min_length\""));
    }
}
