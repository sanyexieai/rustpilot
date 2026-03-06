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
