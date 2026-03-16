use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPresentationMode {
    MultiWindow,
    SingleWindow,
    ImplicitMultiWindow,
}

impl LaunchPresentationMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "multi_window" | "multi-window" | "window" => Some(Self::MultiWindow),
            "single_window" | "single-window" | "single" => Some(Self::SingleWindow),
            "implicit_multi_window" | "implicit-multi-window" | "implicit" => {
                Some(Self::ImplicitMultiWindow)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MultiWindow => "multi_window",
            Self::SingleWindow => "single_window",
            Self::ImplicitMultiWindow => "implicit_multi_window",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::MultiWindow => "each launch opens a dedicated visible OS window",
            Self::SingleWindow => "keep one operator window; child launches run without extra windows",
            Self::ImplicitMultiWindow => {
                "child launches stay isolated in background processes with logs, without extra windows"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSettings {
    #[serde(default = "default_mode")]
    pub mode: LaunchPresentationMode,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            mode: default_mode(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSettingsManager {
    path: PathBuf,
}

impl LaunchSettingsManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        let manager = Self {
            path: dir.join("launch_settings.json"),
        };
        manager.ensure_defaults()?;
        Ok(manager)
    }

    pub fn get(&self) -> anyhow::Result<LaunchSettings> {
        if !self.path.exists() {
            return Ok(LaunchSettings::default());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(LaunchSettings::default());
        }
        Ok(serde_json::from_str(&content)?)
    }

    pub fn set_mode(&self, mode: LaunchPresentationMode) -> anyhow::Result<LaunchSettings> {
        let settings = LaunchSettings { mode };
        self.save(&settings)?;
        Ok(settings)
    }

    pub fn render_summary(&self) -> anyhow::Result<String> {
        let settings = self.get()?;
        let backend = crate::launch_backend::backend_status(settings.mode);
        Ok(format!(
            "launch mode: requested={} effective={} backend={} | requested_note={} | backend_note={}",
            settings.mode.as_str(),
            backend.effective_mode.as_str(),
            backend.backend,
            settings.mode.description(),
            backend.note
        ))
    }

    pub fn mode_for_kind(&self, kind: &str) -> anyhow::Result<LaunchPresentationMode> {
        let settings = self.get()?;
        Ok(match (settings.mode, kind.trim()) {
            (LaunchPresentationMode::MultiWindow, "resident") => {
                LaunchPresentationMode::ImplicitMultiWindow
            }
            (mode, _) => mode,
        })
    }

    fn ensure_defaults(&self) -> anyhow::Result<()> {
        if !self.path.exists() {
            self.save(&LaunchSettings::default())?;
        }
        Ok(())
    }

    fn save(&self, settings: &LaunchSettings) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(settings)?)?;
        Ok(())
    }
}

fn default_mode() -> LaunchPresentationMode {
    LaunchPresentationMode::MultiWindow
}

#[cfg(test)]
mod tests {
    use super::{LaunchPresentationMode, LaunchSettingsManager};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos();
            let path = std::env::temp_dir()
                .join("tests")
                .join(format!("launch_settings_{}_{}", std::process::id(), unique));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn launch_settings_default_to_multi_window() {
        let temp = TestDir::new();
        let manager = LaunchSettingsManager::new(temp.path.clone()).expect("manager");
        let settings = manager.get().expect("settings");
        assert_eq!(settings.mode, LaunchPresentationMode::MultiWindow);
    }

    #[test]
    fn launch_settings_can_switch_modes() {
        let temp = TestDir::new();
        let manager = LaunchSettingsManager::new(temp.path.clone()).expect("manager");
        manager
            .set_mode(LaunchPresentationMode::ImplicitMultiWindow)
            .expect("set mode");
        let settings = manager.get().expect("settings");
        assert_eq!(settings.mode, LaunchPresentationMode::ImplicitMultiWindow);
    }

    #[test]
    fn resident_launches_default_to_background_when_global_mode_is_multi_window() {
        let temp = TestDir::new();
        let manager = LaunchSettingsManager::new(temp.path.clone()).expect("manager");
        let mode = manager.mode_for_kind("resident").expect("resident mode");
        assert_eq!(mode, LaunchPresentationMode::ImplicitMultiWindow);
        let worker_mode = manager.mode_for_kind("worker").expect("worker mode");
        assert_eq!(worker_mode, LaunchPresentationMode::MultiWindow);
    }
}
