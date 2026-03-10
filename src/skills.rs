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

fn resolve_skills_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("SKILLS_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let cwd = std::env::current_dir()?;
    let direct = cwd.join("skills");
    if direct.is_dir() {
        return Ok(direct);
    }

    let fallback = cwd.join("s05").join("skills");
    if fallback.is_dir() {
        return Ok(fallback);
    }

    anyhow::bail!("skills directory not found")
}

pub fn init_tool_skill(raw_name: &str) -> anyhow::Result<PathBuf> {
    let name = normalize_skill_name(raw_name);
    if name.is_empty() {
        anyhow::bail!("invalid skill name: {}", raw_name);
    }

    let base_dir = resolve_or_create_skills_dir()?;
    let skill_dir = base_dir.join(&name);
    if skill_dir.exists() {
        anyhow::bail!("skill already exists: {}", skill_dir.display());
    }

    fs::create_dir_all(skill_dir.join("tests"))?;
    fs::write(skill_dir.join("SKILL.md"), skill_md_template(&name))?;
    fs::write(skill_dir.join("tool.sh"), tool_script_template())?;
    fs::write(
        skill_dir.join("tests").join("smoke.json"),
        test_case_template(),
    )?;
    Ok(skill_dir)
}

fn resolve_or_create_skills_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("SKILLS_DIR") {
        let path = PathBuf::from(dir);
        fs::create_dir_all(&path)?;
        return Ok(path);
    }

    let cwd = std::env::current_dir()?;
    let direct = cwd.join("skills");
    fs::create_dir_all(&direct)?;
    Ok(direct)
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

fn skill_md_template(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: external tool skill\ntool_language: python\ntool_runtime: python 3\ntool_command: python\ntool_args_json: [\"./tool.py\"]\n---\n\n# {name}\n\nThis skill provides a minimal external tool template.\n"
    )
}

fn tool_script_template() -> &'static str {
    "#!/usr/bin/env python3\nimport sys\n\n\ndef main() -> int:\n    data = sys.stdin.read()\n    sys.stdout.write(data)\n    return 0\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n"
}

fn test_case_template() -> &'static str {
    "{\n  \"name\": \"smoke\",\n  \"arguments\": { \"input\": \"hello\" },\n  \"expect_status\": 0,\n  \"expect_stdout_contains\": \"hello\"\n}\n"
}

fn scan_skills(base_dir: &Path) -> anyhow::Result<Vec<SkillInfo>> {
    let mut skills = Vec::new();
    for entry in fs::read_dir(base_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let content = fs::read_to_string(&skill_md)?;
        if let Some(info) = parse_frontmatter(&content, &skill_md) {
            skills.push(info);
        }
    }

    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(skills)
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
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: test skill\n---\nbody\n",
        )
        .expect("write skill");

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
        assert!(content.contains("body"));
    }

    #[test]
    fn init_tool_skill_creates_template_files() {
        let temp = TestDir::new("init_tool_skill");
        unsafe {
            std::env::set_var("SKILLS_DIR", temp.path());
        }

        let created = init_tool_skill("Echo Tool").expect("init tool skill");
        assert!(created.join("SKILL.md").is_file());
        assert!(created.join("tool.sh").is_file());
        assert!(created.join("tests").join("smoke.json").is_file());

        let skill_md = fs::read_to_string(created.join("SKILL.md")).expect("read skill md");
        assert!(skill_md.contains("tool_command"));
        assert!(skill_md.contains("tool_language"));

        unsafe {
            std::env::remove_var("SKILLS_DIR");
        }
    }
}
