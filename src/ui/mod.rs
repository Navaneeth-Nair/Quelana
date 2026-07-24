use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub mod overlay;
pub mod settings;

pub use overlay::OverlayApp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub auto_answer_mode: bool,
    pub keep_on_top: bool,
    pub hide_during_screen_share: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_answer_mode: false,
            keep_on_top: true,
            hide_during_screen_share: true,
        }
    }
}

pub const SETTINGS_FILE_NAME: &str = ".quelana_settings.json";

pub fn settings_path() -> Result<PathBuf> {
    let mut path = std::env::current_dir()?;
    path.push(SETTINGS_FILE_NAME);
    Ok(path)
}

pub fn load_settings() -> AppSettings {
    if let Ok(path) = settings_path() {
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(settings) = serde_json::from_str(&contents) {
                return settings;
            }
        }
    }

    AppSettings::default()
}

pub fn save_settings(settings: &AppSettings) -> Result<()> {
    let path = settings_path()?;
    let contents = serde_json::to_string_pretty(settings)?;
    fs::write(path, contents)?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::AppSettings;

    #[test]
    fn defaults_keep_auto_answer_disabled() {
        let settings = AppSettings::default();
        assert!(!settings.auto_answer_mode);
        assert!(settings.keep_on_top);
        assert!(settings.hide_during_screen_share);
    }
}
