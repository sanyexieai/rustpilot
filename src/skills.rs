use crate::skill_authoring::SkillAuthoringConfig;
use crate::tool_capability::ToolCapabilityLevel;
use crate::tool_manifest::resolve_or_create_tools_dir;
use anyhow::Context as _;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub struct SkillRegistry {
    base_dir: PathBuf,
    skills: Vec<SkillInfo>,
}

impl SkillRegistry {
    pub fn load() -> anyhow::Result<Self> {
        let base_dir = resolve_skills_dir()?;
        let skills = scan_skills(&base_dir)?;
        Ok(Self { base_dir, skills })
    }

    pub fn empty() -> Self {
        Self {
            base_dir: PathBuf::new(),
            skills: Vec::new(),
        }
    }

    pub fn list(&self) -> &[SkillInfo] {
        &self.skills
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn get(&self, name: &str) -> anyhow::Result<String> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow::anyhow!("unknown skill: {}", name))?;
        let content = fs::read_to_string(&skill.path)?;
        Ok(content)
    }
}

pub fn skill_exists(raw_name: &str) -> bool {
    let name = normalize_skill_name(raw_name);
    resolve_skills_dir()
        .map(|base| base.join(&name).is_dir())
        .unwrap_or(false)
}

pub fn create_prompt_skill(
    raw_name: &str,
    description: &str,
    body: &str,
    test_prompt: Option<&str>,
    expect_response_contains: &[String],
) -> anyhow::Result<PathBuf> {
    let name = normalize_skill_name(raw_name);
    if name.is_empty() {
        anyhow::bail!("invalid skill name: {}", raw_name);
    }
    let config = SkillAuthoringConfig::load();
    let base_dir = resolve_or_create_skills_dir()?;
    let skill_dir = base_dir.join(&name);
    fs::create_dir_all(config.tests_dir_path(&skill_dir))?;
    // When no test_prompt is provided, fall back to the description so that
    // the smoke test always has a prompt and LLM tests always run.
    let effective_test_prompt = test_prompt.unwrap_or(description);
    // Always overwrite the core skill content and smoke test.
    fs::write(
        config.skill_file_path(&skill_dir),
        config.render_skill_markdown(&name, description, body),
    )?;
    fs::write(
        config.smoke_test_path(&skill_dir),
        config.render_smoke_test(
            description,
            body,
            Some(effective_test_prompt),
            expect_response_contains,
        ),
    )?;
    // Only write the integration test template if it doesn't already exist,
    // so that any custom integration.py written by the user is preserved.
    let integration_path = config.integration_test_path(&skill_dir);
    if !integration_path.exists() {
        fs::write(
            &integration_path,
            crate::skill_integration::integration_test_template(),
        )?;
    }
    Ok(skill_dir)
}

/// Run LLM functional tests for a skill: sends each test case's `prompt` to the LLM
/// using the skill as system prompt and validates the response.
/// Returns the number of LLM test cases that passed.
pub async fn run_skill_llm_tests(
    skill_dir: &Path,
    client: &reqwest::Client,
    config: &crate::config::LlmConfig,
) -> anyhow::Result<usize> {
    let authoring = SkillAuthoringConfig::load();
    let skill_md = authoring.skill_file_path(skill_dir);
    let skill_content = fs::read_to_string(&skill_md)
        .with_context(|| format!("SKILL.md not found in {}", skill_dir.display()))?;
    let tests_dir = authoring.tests_dir_path(skill_dir);
    if !tests_dir.is_dir() {
        return Ok(0);
    }
    let cases = load_skill_test_cases(&tests_dir)?;
    let llm_cases: Vec<_> = cases.into_iter().filter(|c| c.prompt.is_some()).collect();
    if llm_cases.is_empty() {
        return Ok(0);
    }
    let mut passed = 0;
    for case in &llm_cases {
        let prompt = case.prompt.as_deref().unwrap();
        let response = crate::agent::one_shot_completion(client, config, &skill_content, prompt)
            .await
            .with_context(|| format!("LLM call for test '{}' failed", case.name))?;
        if let Some(min_len) = case.expect_response_min_length {
            if response.len() < min_len {
                anyhow::bail!(
                    "llm test '{}': response length {} < required {}",
                    case.name,
                    response.len(),
                    min_len
                );
            }
        }
        if let Some(fragments) = &case.expect_response_contains {
            let response_lower = response.to_lowercase();
            for fragment in fragments {
                if !response_lower.contains(&fragment.to_lowercase()) {
                    anyhow::bail!(
                        "llm test '{}': response missing expected fragment '{}'",
                        case.name,
                        fragment
                    );
                }
            }
        }
        passed += 1;
    }
    Ok(passed)
}

/// 对已存在的 skill 目录运行验证和测试，返回通过的测试数量。
/// 如果验证失败则返回 Err，错误信息说明失败原因。
pub fn validate_skill(skill_dir: &Path) -> anyhow::Result<usize> {
    let authoring = SkillAuthoringConfig::load();
    let skill_md = authoring.skill_file_path(skill_dir);
    let content = fs::read_to_string(&skill_md)
        .map_err(|_| anyhow::anyhow!("SKILL.md not found in {}", skill_dir.display()))?;
    let info = parse_frontmatter(&content, &skill_md).ok_or_else(|| {
        anyhow::anyhow!("SKILL.md frontmatter is invalid or missing name/description")
    })?;
    let body = extract_body(&content);
    validate_and_test_skill(&info, body)?;
    // 计算通过的测试数量
    let tests_dir = authoring.tests_dir_path(skill_dir);
    let count = if tests_dir.is_dir() {
        load_skill_test_cases(&tests_dir)
            .map(|c| c.len())
            .unwrap_or(0)
    } else {
        0
    };
    Ok(count)
}

fn resolve_skills_dir() -> anyhow::Result<PathBuf> {
    SkillAuthoringConfig::load().resolve_existing_skills_dir()
}

fn resolve_or_create_skills_dir() -> anyhow::Result<PathBuf> {
    SkillAuthoringConfig::load().resolve_or_create_skills_dir()
}

pub fn init_tool_skill(raw_name: &str, level: ToolCapabilityLevel) -> anyhow::Result<PathBuf> {
    let name = normalize_skill_name(raw_name);
    if name.is_empty() {
        anyhow::bail!("invalid skill name: {}", raw_name);
    }

    let base_dir = resolve_or_create_tools_dir()?;
    let skill_dir = base_dir.join(level.directory_name()).join(&name);
    if skill_dir.exists() {
        anyhow::bail!("skill already exists: {}", skill_dir.display());
    }

    fs::create_dir_all(skill_dir.join("tests"))?;
    fs::write(
        skill_dir.join("tool.toml"),
        tool_manifest_template(&name, level),
    )?;
    fs::write(
        skill_dir.join("README.md"),
        tool_readme_template(&name, level),
    )?;
    fs::write(skill_dir.join("tool.sh"), tool_script_template())?;
    fs::write(
        skill_dir.join("tests").join("smoke.json"),
        test_case_template(),
    )?;
    Ok(skill_dir)
}

fn normalize_skill_name(input: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in input.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            Some('-')
        } else {
            None
        };

        if let Some(c) = normalized {
            if c == '-' {
                if prev_hyphen || out.is_empty() {
                    continue;
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
            out.push(c);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn tool_manifest_template(name: &str, level: ToolCapabilityLevel) -> String {
    format!(
        "schema_version = 1\n\n[tool]\nname = \"{name}\"\ndescription = \"external tool skill\"\nlevel = \"{}\"\nruntime_kind = \"script\"\nlanguage = \"python\"\nruntime = \"python 3\"\ncommand = \"python\"\nargs = [\"./tool.py\"]\n",
        level.as_str()
    )
}

fn tool_readme_template(name: &str, level: ToolCapabilityLevel) -> String {
    format!(
        "# {name}\n\nThis tool must stay in `tools/{}/{name}/`.\n\nRequired files:\n- `tool.toml`\n- tool entrypoint such as `tool.py`\n- `tests/` with at least one smoke test\n",
        level.directory_name()
    )
}

fn tool_script_template() -> &'static str {
    "#!/usr/bin/env python3\nimport sys\n\n\ndef main() -> int:\n    data = sys.stdin.read()\n    sys.stdout.write(data)\n    return 0\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n"
}

fn test_case_template() -> &'static str {
    "{\n  \"name\": \"smoke\",\n  \"arguments\": { \"input\": \"hello\" },\n  \"expect_status\": 0,\n  \"expect_stdout_contains\": \"hello\"\n}\n"
}

#[derive(Debug, Deserialize)]
struct SkillTestCase {
    name: String,
    // Static checks (run without LLM)
    #[serde(default)]
    expect_body_contains: Option<String>,
    #[serde(default)]
    expect_description_min_length: Option<usize>,
    #[serde(default)]
    expect_body_min_length: Option<usize>,
    // LLM functional test: send `prompt` to the LLM using the skill as system prompt
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    expect_response_contains: Option<Vec<String>>,
    #[serde(default)]
    expect_response_min_length: Option<usize>,
}

fn validate_and_test_skill(info: &SkillInfo, body: &str) -> anyhow::Result<()> {
    if info.name.trim().is_empty() {
        anyhow::bail!("name is required");
    }
    if info.description.trim().is_empty() {
        anyhow::bail!("description is required");
    }
    if body.trim().is_empty() {
        anyhow::bail!("skill body is empty");
    }

    let tests_dir =
        SkillAuthoringConfig::load().tests_dir_path(info.path.parent().unwrap_or(Path::new(".")));
    if !tests_dir.is_dir() {
        anyhow::bail!("tests/ directory is missing");
    }

    let cases = load_skill_test_cases(&tests_dir)?;
    if cases.is_empty() {
        anyhow::bail!("tests/ must contain at least one test case (.json)");
    }

    for case in &cases {
        run_skill_test_case(&case.name, &info.description, body, case)?;
    }
    Ok(())
}

fn load_skill_test_cases(tests_dir: &Path) -> anyhow::Result<Vec<SkillTestCase>> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(tests_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let case: SkillTestCase = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("parse test case {}: {}", path.display(), e))?;
        cases.push(case);
    }
    Ok(cases)
}

fn run_skill_test_case(
    test_name: &str,
    description: &str,
    body: &str,
    case: &SkillTestCase,
) -> anyhow::Result<()> {
    if let Some(min_len) = case.expect_description_min_length {
        if description.len() < min_len {
            anyhow::bail!(
                "test '{}': description length {} < required {}",
                test_name,
                description.len(),
                min_len
            );
        }
    }
    if let Some(min_len) = case.expect_body_min_length {
        if body.len() < min_len {
            anyhow::bail!(
                "test '{}': body length {} < required {}",
                test_name,
                body.len(),
                min_len
            );
        }
    }
    if let Some(expected) = &case.expect_body_contains {
        if !body.contains(expected.as_str()) {
            anyhow::bail!(
                "test '{}': body missing expected fragment '{}'",
                test_name,
                expected
            );
        }
    }
    Ok(())
}

fn scan_skills(base_dir: &Path) -> anyhow::Result<Vec<SkillInfo>> {
    let config = SkillAuthoringConfig::load();
    let mut skills = Vec::new();
    for entry in fs::read_dir(base_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = config.skill_file_path(&path);
        if !skill_md.is_file() {
            continue;
        }
        let content = fs::read_to_string(&skill_md)?;
        let Some(info) = parse_frontmatter(&content, &skill_md) else {
            continue;
        };
        let body = extract_body(&content);
        if let Err(err) = validate_and_test_skill(&info, body) {
            eprintln!(
                "[skills] skip '{}' ({}): {}",
                info.name,
                skill_md.display(),
                err
            );
            continue;
        }
        skills.push(info);
    }

    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(skills)
}

fn extract_body(content: &str) -> &str {
    let mut lines = content.splitn(4, '\n');
    // skip opening "---"
    if lines.next().map(str::trim) != Some("---") {
        return content;
    }
    // skip frontmatter lines until closing "---"
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
    }
    // remainder is body; find the position in the original string
    let closing = content
        .match_indices("---")
        .nth(1)
        .map(|(i, _)| i + 3)
        .unwrap_or(0);
    content[closing..].trim_start()
}

fn parse_frontmatter(content: &str, path: &Path) -> Option<SkillInfo> {
    let mut lines = content.lines();
    let first = lines.next()?.trim();
    if first != "---" {
        return None;
    }

    let mut name = None;
    let mut description = None;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let name = name?;
    let description = description.unwrap_or_else(|| "".to_string());

    Some(SkillInfo {
        name,
        description,
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_capability::ToolCapabilityLevel;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            );
            let path = std::env::temp_dir()
                .join("rustpilot_skills_tests")
                .join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn load_and_get_skill_from_env_dir() {
        let temp = TestDir::new("skill_registry");
        let skill_dir = temp.path().join("demo");
        fs::create_dir_all(skill_dir.join("tests")).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: test skill\n---\n\n## Overview\n\nThis is the skill body content.\n",
        )
        .expect("write skill");
        fs::write(
            skill_dir.join("tests").join("smoke.json"),
            "{\n  \"name\": \"smoke\",\n  \"expect_description_min_length\": 5,\n  \"expect_body_contains\": \"## Overview\"\n}\n",
        )
        .expect("write smoke test");

        unsafe {
            std::env::set_var("SKILLS_DIR", temp.path());
        }
        let registry = SkillRegistry::load().expect("load registry");
        unsafe {
            std::env::remove_var("SKILLS_DIR");
        }

        assert_eq!(registry.base_dir(), temp.path());
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].name, "demo");
        assert_eq!(registry.list()[0].description, "test skill");

        let content = registry.get("demo").expect("get skill");
        assert!(content.contains("## Overview"));
    }

    #[test]
    fn skill_without_tests_dir_is_rejected() {
        let temp = TestDir::new("skill_no_tests");
        let skill_dir = temp.path().join("notested");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: notested\ndescription: a skill without tests\n---\n\n## Body\n\nsome content\n",
        )
        .expect("write skill");

        unsafe {
            std::env::set_var("SKILLS_DIR", temp.path());
        }
        let registry = SkillRegistry::load().expect("load registry");
        unsafe {
            std::env::remove_var("SKILLS_DIR");
        }

        assert_eq!(
            registry.list().len(),
            0,
            "skill without tests/ should be rejected"
        );
    }

    #[test]
    fn skill_with_failing_test_is_rejected() {
        let temp = TestDir::new("skill_fail_test");
        let skill_dir = temp.path().join("failing");
        fs::create_dir_all(skill_dir.join("tests")).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: failing\ndescription: skill with failing test\n---\n\n## Body\n\nsome content here\n",
        )
        .expect("write skill");
        fs::write(
            skill_dir.join("tests").join("check.json"),
            "{\n  \"name\": \"check\",\n  \"expect_body_contains\": \"SECTION_THAT_DOES_NOT_EXIST\"\n}\n",
        )
        .expect("write test");

        unsafe {
            std::env::set_var("SKILLS_DIR", temp.path());
        }
        let registry = SkillRegistry::load().expect("load registry");
        unsafe {
            std::env::remove_var("SKILLS_DIR");
        }

        assert_eq!(
            registry.list().len(),
            0,
            "skill with failing test should be rejected"
        );
    }

    #[test]
    fn init_tool_skill_creates_template_files() {
        let temp = TestDir::new("init_tool_skill");
        unsafe {
            std::env::set_var("TOOLS_DIR", temp.path());
        }

        let created =
            init_tool_skill("Echo Tool", ToolCapabilityLevel::Generic).expect("init tool skill");
        assert!(created.join("tool.toml").is_file());
        assert!(created.join("README.md").is_file());
        assert!(created.join("tool.sh").is_file());
        assert!(created.join("tests").join("smoke.json").is_file());
        assert!(created.ends_with(Path::new("generic").join("echo-tool")));

        let manifest = fs::read_to_string(created.join("tool.toml")).expect("read tool manifest");
        assert!(manifest.contains("command = \"python\""));
        assert!(manifest.contains("language = \"python\""));
        assert!(manifest.contains("level = \"generic\""));

        unsafe {
            std::env::remove_var("TOOLS_DIR");
        }
    }

    #[test]
    fn create_prompt_skill_writes_loadable_skill_markdown() {
        let temp = TestDir::new("create_prompt_skill");
        unsafe {
            std::env::set_var("SKILLS_DIR", temp.path());
        }

        let created = create_prompt_skill(
            "Frontend Engineer",
            "frontend implementation help",
            "Use React or Vue when appropriate.",
            None,
            &[],
        )
        .expect("create prompt skill");
        assert!(created.join("SKILL.md").is_file());
        let content = fs::read_to_string(created.join("SKILL.md")).expect("read skill");
        assert!(content.starts_with("---\nname: frontend-engineer\n"));
        assert!(content.contains("description: frontend implementation help"));
        assert!(content.contains("# Frontend Engineer"));

        unsafe {
            std::env::remove_var("SKILLS_DIR");
        }
    }
}
