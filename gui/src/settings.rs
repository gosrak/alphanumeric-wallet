//! What the wallet remembers between runs that is not a secret: which node
//! it runs and how, and whether and how it mines. Plain JSON beside the
//! wallet file. A missing or unreadable file is the defaults -- settings
//! must never stop a wallet from opening.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub node: NodeSettings,
    pub mining: MiningSettings,
}

/// The owned node's binary and ports. `None` means the wallet's default
/// (the binary beside it, the standard ports).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSettings {
    pub binary: Option<String>,
    pub p2p_port: Option<u16>,
    pub explorer_port: Option<u16>,
    pub stats_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Gpu,
    Cpu,
}

impl Backend {
    /// The value `ALPHANUMERIC_MINE_BACKEND` takes.
    pub fn as_env(self) -> &'static str {
        match self {
            Backend::Gpu => "gpu",
            Backend::Cpu => "cpu",
        }
    }
}

/// Whether the wallet's node mines, and how. `address` is where the reward
/// goes -- one of this wallet's own, chosen on F5. `disabled_gpus` are the
/// node's device indices the user switched off.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MiningSettings {
    pub enabled: bool,
    pub backend: Backend,
    pub address: Option<String>,
    pub cpu_threads: Option<u32>,
    pub disabled_gpus: Vec<u32>,
}

/// `~/.alphanumeric-gui/settings.json`, beside `seed.enc`.
pub fn default_path() -> Option<PathBuf> {
    crate::storage::home_dir().map(|h| h.join(".alphanumeric-gui").join("settings.json"))
}

/// Never fails: a missing, unreadable or malformed file is the defaults.
pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Written whole to a temporary beside the file and renamed into place, so
/// a crash mid-write leaves the old settings rather than half of the new.
pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not replace {}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = load(&dir.path().join("settings.json"));
        assert_eq!(s, Settings::default());
        assert!(!s.mining.enabled);
        assert_eq!(s.mining.backend, Backend::Gpu);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.mining.enabled = true;
        s.mining.backend = Backend::Cpu;
        s.mining.address = Some("089b61914421754ca33e03b42c6dcd9c709c6cc1".into());
        s.mining.cpu_threads = Some(4);
        s.mining.disabled_gpus = vec![1];
        s.node.p2p_port = Some(7180);
        s.node.binary = Some("/opt/alphanumeric".into());
        save(&path, &s).expect("save");
        assert_eq!(load(&path), s);
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    // An older or newer wallet's file must open: unknown keys are ignored,
    // missing ones take their defaults.
    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"mining":{"enabled":true,"future":1},"other":{}}"#,
        )
        .expect("write");
        let s = load(&path);
        assert!(s.mining.enabled);
        assert_eq!(s.mining.backend, Backend::Gpu);
        assert_eq!(s.node, NodeSettings::default());
    }

    // Garbage is not an error either: the wallet opens with defaults.
    #[test]
    fn a_corrupt_file_is_the_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"{not json").expect("write");
        assert_eq!(load(&path), Settings::default());
    }

    #[test]
    fn the_default_path_sits_beside_the_wallet_file() {
        assert_eq!(
            default_path(),
            crate::storage::home_dir().map(|h| h.join(".alphanumeric-gui").join("settings.json"))
        );
    }

    #[test]
    fn the_backend_names_match_the_nodes_variable() {
        assert_eq!(Backend::Gpu.as_env(), "gpu");
        assert_eq!(Backend::Cpu.as_env(), "cpu");
    }
}
