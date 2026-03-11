use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapabilityLevel {
    Feature,
    Project,
    Generic,
    Kernel,
}

impl ToolCapabilityLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Project => "project",
            Self::Generic => "generic",
            Self::Kernel => "kernel",
        }
    }

    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "feature" | "feature_level" => Ok(Self::Feature),
            "project" | "project_level" => Ok(Self::Project),
            "generic" | "generic_level" => Ok(Self::Generic),
            "kernel" | "kernel_level" => Ok(Self::Kernel),
            other => anyhow::bail!("unsupported tool_level: {}", other),
        }
    }

    pub fn directory_name(self) -> &'static str {
        self.as_str()
    }

    pub fn all() -> [Self; 4] {
        [Self::Feature, Self::Project, Self::Generic, Self::Kernel]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRuntimeKind {
    RustBinary,
    Script,
    Mcp,
}

impl ToolRuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RustBinary => "rust_binary",
            Self::Script => "script",
            Self::Mcp => "mcp",
        }
    }

    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rust_binary" | "binary" | "rust" => Ok(Self::RustBinary),
            "script" => Ok(Self::Script),
            "mcp" => Ok(Self::Mcp),
            other => anyhow::bail!("unsupported runtime_kind: {}", other),
        }
    }
}
