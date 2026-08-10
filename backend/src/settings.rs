use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const MIN_SEND_DELAY_MS: u64 = 100;
pub const MAX_SEND_DELAY_MS: u64 = 5000;

const MODELS: &[&str] = &["tiny", "base", "small", "medium"];
const PTT_MODES: &[&str] = &["hold", "toggle"];
const OUTPUT_MODES: &[&str] = &["steam_input", "steam_input_send", "clipboard"];
// v1 exposes only the four back-grip buttons whose Steam event mappings were
// audited against the reference plugin. Broader controller support is deferred.
const CONTROLLER_BUTTONS: &[&str] = &["R4", "L4", "R5", "L5"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,
    pub model: String,
    pub language: String,
    pub gpu_enabled: bool,
    pub ptt_mode: String,
    pub controller_primary: String,
    pub controller_secondary: Option<String>,
    pub output_mode: String,
    pub send_delay_ms: u64,
    pub auto_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            model: "small".to_string(),
            language: "auto".to_string(),
            gpu_enabled: true,
            ptt_mode: "hold".to_string(),
            controller_primary: "R4".to_string(),
            controller_secondary: None,
            output_mode: "steam_input".to_string(),
            send_delay_ms: 250,
            auto_start: true,
        }
    }
}

impl Settings {
    pub fn normalize(value: &Value) -> Self {
        let defaults = Self::default();
        let Some(object) = value.as_object() else {
            return defaults;
        };

        let model = allowed_string(object, "model", MODELS).unwrap_or(defaults.model);
        let ptt_mode = allowed_string(object, "ptt_mode", PTT_MODES).unwrap_or(defaults.ptt_mode);
        let controller_primary = allowed_string(object, "controller_primary", CONTROLLER_BUTTONS)
            .unwrap_or(defaults.controller_primary);
        let mut controller_secondary = match object.get("controller_secondary") {
            Some(Value::Null) | None => None,
            Some(Value::String(value)) if CONTROLLER_BUTTONS.contains(&value.as_str()) => {
                Some(value.clone())
            }
            _ => None,
        };
        if controller_secondary.as_deref() == Some(controller_primary.as_str()) {
            controller_secondary = None;
        }

        let output_mode =
            allowed_string(object, "output_mode", OUTPUT_MODES).unwrap_or(defaults.output_mode);
        let send_delay_ms = object
            .get("send_delay_ms")
            .and_then(Value::as_u64)
            .unwrap_or(defaults.send_delay_ms)
            .clamp(MIN_SEND_DELAY_MS, MAX_SEND_DELAY_MS);

        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            model,
            // v1 deliberately exposes no remote/language-specific backend.
            language: "auto".to_string(),
            gpu_enabled: strict_bool(object, "gpu_enabled").unwrap_or(defaults.gpu_enabled),
            ptt_mode,
            controller_primary,
            controller_secondary,
            output_mode,
            send_delay_ms,
            auto_start: strict_bool(object, "auto_start").unwrap_or(defaults.auto_start),
        }
    }

    pub fn apply_patch(&self, patch: &Value) -> Self {
        let mut merged = serde_json::to_value(self)
            .expect("serializing Settings is infallible")
            .as_object()
            .expect("Settings serializes as an object")
            .clone();
        if let Some(patch) = patch.as_object() {
            for (key, value) in patch {
                if is_known_key(key) {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
        Self::normalize(&Value::Object(merged))
    }
}

fn strict_bool(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

fn allowed_string(object: &Map<String, Value>, key: &str, allowed: &[&str]) -> Option<String> {
    let value = object.get(key)?.as_str()?;
    allowed.contains(&value).then(|| value.to_string())
}

fn is_known_key(key: &str) -> bool {
    matches!(
        key,
        "schema_version"
            | "model"
            | "language"
            | "gpu_enabled"
            | "ptt_mode"
            | "controller_primary"
            | "controller_secondary"
            | "output_mode"
            | "send_delay_ms"
            | "auto_start"
    )
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(settings_dir: impl AsRef<Path>) -> Self {
        Self {
            path: settings_dir.as_ref().join("settings.json"),
        }
    }

    pub fn load(&self) -> io::Result<Settings> {
        let settings = match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map(|value| Settings::normalize(&value))
                .unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(error) => return Err(error),
        };
        self.save(&settings)?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid settings path"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".settings.json.{}.tmp", std::process::id()));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            serde_json::to_writer_pretty(&mut file, settings)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            sync_directory(parent)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_product_contract() {
        let settings = Settings::default();
        assert_eq!(settings.model, "small");
        assert_eq!(settings.language, "auto");
        assert!(settings.gpu_enabled);
        assert_eq!(settings.ptt_mode, "hold");
        assert_eq!(settings.controller_primary, "R4");
        assert_eq!(settings.output_mode, "steam_input");
        assert_eq!(settings.send_delay_ms, 250);
        assert!(settings.auto_start);
    }

    #[test]
    fn normalization_is_strict_and_fail_closed() {
        let settings = Settings::normalize(&json!({
            "schema_version": 999,
            "model": "small.en",
            "language": "zh",
            "gpu_enabled": "yes",
            "ptt_mode": "press",
            "controller_primary": "R4",
            "controller_secondary": "R4",
            "output_mode": "send_anywhere",
            "send_delay_ms": 50,
            "auto_start": 1,
            "unknown": "discard me"
        }));
        assert_eq!(settings.schema_version, 1);
        assert_eq!(settings.model, "small");
        assert_eq!(settings.language, "auto");
        assert!(settings.gpu_enabled);
        assert_eq!(settings.ptt_mode, "hold");
        assert_eq!(settings.controller_secondary, None);
        assert_eq!(settings.output_mode, "steam_input");
        assert_eq!(settings.send_delay_ms, 100);
        assert!(settings.auto_start);
        assert!(serde_json::to_value(settings)
            .unwrap()
            .get("unknown")
            .is_none());
    }

    #[test]
    fn delay_is_clamped_at_both_bounds() {
        assert_eq!(
            Settings::normalize(&json!({"send_delay_ms": 99})).send_delay_ms,
            100
        );
        assert_eq!(
            Settings::normalize(&json!({"send_delay_ms": 9000})).send_delay_ms,
            5000
        );
    }

    #[test]
    fn patch_preserves_omitted_fields_and_normalizes_invalid_output_safely() {
        let current = Settings::default().apply_patch(&json!({"model": "base"}));
        let updated = current.apply_patch(&json!({"output_mode": "invalid"}));
        assert_eq!(updated.model, "base");
        assert_eq!(updated.output_mode, "steam_input");
    }

    #[test]
    fn store_recovers_from_bad_json_and_writes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        fs::write(store.path(), b"not json").unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
        let bytes = fs::read(store.path()).unwrap();
        let persisted: Settings = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted, Settings::default());
        assert!(!directory
            .path()
            .join(format!(".settings.json.{}.tmp", std::process::id()))
            .exists());
    }
}
