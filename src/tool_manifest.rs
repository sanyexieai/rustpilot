use crate::tool_capability::{ToolCapabilityLevel, ToolRuntimeKind};
use anyhow::Context;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct ToolManifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub tool: ToolManifestTool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolManifestTool {
    pub name: String,
    pub description: String,
    pub level: String,
    #[serde(default)]
    pub runtime_kind: Option<String>,
    pub language: String,
    #[serde(default)]
    pub runtime: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_schema_version() -> u32 {
    1
}

impl ToolManifest {
    pub fn load_from_dir(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("tool.toml");
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read tool manifest {}", path.display()))?;
        let manifest: Self =
            toml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
        manifest.validate(&path)?;
        Ok(manifest)
    }

    pub fn validate(&self, path: &Path) -> anyhow::Result<()> {
        if self.schema_version != 1 {
            anyhow::bail!(
                "unsupported tool manifest schema_version {} in {}",
                self.schema_version,
                path.display()
            );
        }
        if self.tool.name.trim().is_empty() {
            anyhow::bail!("tool.name is required in {}", path.display());
        }
        if self.tool.description.trim().is_empty() {
            anyhow::bail!("tool.description is required in {}", path.display());
        }
        if self.tool.language.trim().is_empty() {
            anyhow::bail!("tool.language is required in {}", path.display());
        }
        if self.tool.command.trim().is_empty() {
            anyhow::bail!("tool.command is required in {}", path.display());
        }
        let _ = self.level()?;
        let _ = self.runtime_kind()?;
        Ok(())
    }

    pub fn level(&self) -> anyhow::Result<ToolCapabilityLevel> {
        ToolCapabilityLevel::parse(&self.tool.level)
    }

    pub fn runtime_kind(&self) -> anyhow::Result<ToolRuntimeKind> {
        if let Some(kind) = &self.tool.runtime_kind {
            ToolRuntimeKind::parse(kind)
        } else {
            Ok(infer_runtime_kind(
                &self.tool.language,
                self.tool.runtime.as_deref(),
                &self.tool.command,
            ))
        }
    }
}

pub fn resolve_tools_dir() -> anyhow::Result<Option<PathBuf>> {
    if let Ok(dir) = std::env::var("TOOLS_DIR") {
        return Ok(Some(PathBuf::from(dir)));
    }

    let cwd = std::env::current_dir()?;
    let direct = cwd.join("tools");
    Ok(direct.is_dir().then_some(direct))
}

pub fn resolve_or_create_tools_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("TOOLS_DIR") {
        let path = PathBuf::from(dir);
        fs::create_dir_all(&path)?;
        return Ok(path);
    }

    let cwd = std::env::current_dir()?;
    let direct = cwd.join("tools");
    fs::create_dir_all(&direct)?;
    Ok(direct)
}

pub fn infer_runtime_kind(language: &str, runtime: Option<&str>, command: &str) -> ToolRuntimeKind {
    let language = language.trim().to_ascii_lowercase();
    let runtime = runtime.unwrap_or_default().trim().to_ascii_lowercase();
    let command = command.trim().to_ascii_lowercase();
    if language == "rust" && runtime.is_empty() && command != "rustc" && command != "cargo" {
        ToolRuntimeKind::RustBinary
    } else {
        ToolRuntimeKind::Script
    }
}
