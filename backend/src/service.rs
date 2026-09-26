use crate::engine::{Engine, EngineEvent};
use crate::model::{self, ModelError, ModelEvent};
use crate::protocol::{ErrorBody, Event, PROTOCOL_VERSION};
use crate::settings::{Settings, SettingsStore};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendPhase {
    Stopped,
    SetupRequired,
    Ready,
    Recording,
    Transcribing,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub protocol_version: u32,
    pub instance_id: String,
    pub seq: u64,
    pub settings: Settings,
    pub phase: BackendPhase,
    pub enabled: bool,
    pub model_installed: bool,
    pub engine_backend: Option<String>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug)]
pub enum Effect {
    InstallModel { model: String },
    CancelModel,
}

#[derive(Debug)]
pub struct CommandResult {
    pub value: Value,
    pub events: Vec<Event>,
    pub effects: Vec<Effect>,
    pub shutdown: bool,
}

impl CommandResult {
    fn value(value: Value) -> Self {
        Self {
            value,
            events: Vec::new(),
            effects: Vec::new(),
            shutdown: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
}

impl CoreError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn body(&self) -> ErrorBody {
        ErrorBody::new(self.code, &self.message)
    }
}

#[derive(Debug, Clone)]
struct Session {
    id: u64,
    stop_requested: bool,
}

type ModelChecker = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct Service<E: Engine> {
    engine: E,
    store: SettingsStore,
    settings: Settings,
    instance_id: String,
    seq: u64,
    phase: BackendPhase,
    enabled: bool,
    auto_start_pending: bool,
    engine_backend: Option<String>,
    error: Option<ErrorBody>,
    session: Option<Session>,
    last_finished_session: Option<u64>,
    model_installing: Option<String>,
    model_checker: ModelChecker,
    model_installed: bool,
}

impl<E: Engine> Service<E> {
    pub fn new(
        engine: E,
        store: SettingsStore,
        settings_dir: PathBuf,
        instance_id: String,
    ) -> Result<Self, CoreError> {
        let checker = Arc::new(move |name: &str| model::is_installed(&settings_dir, name));
        Self::with_model_checker(engine, store, instance_id, checker)
    }

    pub fn with_model_checker(
        engine: E,
        store: SettingsStore,
        instance_id: String,
        model_checker: ModelChecker,
    ) -> Result<Self, CoreError> {
        let settings = store
            .load()
            .map_err(|error| CoreError::new("SETTINGS_IO_FAILED", error.to_string()))?;
        let auto_start_pending = settings.auto_start;
        let service = Self {
            engine,
            store,
            settings,
            instance_id,
            seq: 0,
            phase: BackendPhase::Stopped,
            enabled: false,
            auto_start_pending,
            engine_backend: None,
            error: None,
            session: None,
            last_finished_session: None,
            model_installing: None,
            model_checker,
            // Full SHA-256 verification can be expensive for medium. It is
            // deliberately deferred until after the hello response.
            model_installed: false,
        };
        Ok(service)
    }

    /// Called only after the protocol hello response has been flushed. Model
    /// loading must never delay or prevent the bridge/core version handshake.
    pub fn activate_auto_start(&mut self) -> Vec<Event> {
        if !self.auto_start_pending {
            return Vec::new();
        }
        self.auto_start_pending = false;
        self.refresh_model_installed();
        self.arm_backend();
        vec![self.snapshot_event()]
    }

    pub fn handle(&mut self, method: &str, params: Value) -> Result<CommandResult, CoreError> {
        match method {
            "hello" => Ok(CommandResult::value(json!({
                "protocol_version": PROTOCOL_VERSION,
                "instance_id": self.instance_id,
                "capabilities": [
                    "settings_v1",
                    "voxtype_engine",
                    "model_install",
                    "session_output"
                ]
            }))),
            "get_snapshot" => Ok(CommandResult::value(to_value(self.snapshot())?)),
            "update_settings" => self.update_settings(params),
            "set_enabled" => self.set_enabled(params),
            "record_start" => self.record_start(params),
            "record_stop" => self.record_stop(params),
            "cancel_session" => self.cancel_session(params),
            "install_model" => self.install_model(params),
            "cancel_model" => self.cancel_model(),
            "shutdown" => Ok(self.shutdown()),
            _ => Err(CoreError::new(
                "UNKNOWN_METHOD",
                format!("unknown method {method}"),
            )),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            protocol_version: PROTOCOL_VERSION,
            instance_id: self.instance_id.clone(),
            seq: self.seq,
            settings: self.settings.clone(),
            phase: self.phase,
            enabled: self.enabled,
            model_installed: self.model_installed,
            engine_backend: self.engine_backend.clone(),
            error: self.error.clone(),
        }
    }

    pub fn has_background_work(&self) -> bool {
        self.session.is_some() || self.model_installing.is_some()
    }

    pub fn error_events(&mut self, error: &CoreError) -> Vec<Event> {
        vec![
            self.event(
                "error",
                json!({"code": error.code, "message": error.message}),
            ),
            self.snapshot_event(),
        ]
    }

    pub fn handle_engine_event(&mut self, event: EngineEvent) -> Vec<Event> {
        match event {
            EngineEvent::Transcription {
                session_id,
                text,
                stop_requested,
            } => {
                let Some(session) = self.session.as_ref() else {
                    return Vec::new();
                };
                let valid_terminal_phase = match self.phase {
                    // max_duration can finish before the controller is released
                    BackendPhase::Recording => !session.stop_requested && !stop_requested,
                    BackendPhase::Transcribing => session.stop_requested || stop_requested,
                    _ => false,
                };
                if session.id != session_id || !valid_terminal_phase {
                    return Vec::new();
                }
                self.session = None;
                self.last_finished_session = Some(session_id);
                self.error = None;
                let output = if text.trim().is_empty() {
                    json!({
                        "session_id": session_id,
                        "ok": false,
                        "text": "",
                        "error": "No speech recognized",
                        "error_code": "NO_SPEECH"
                    })
                } else {
                    json!({
                        "session_id": session_id,
                        "ok": true,
                        "text": text
                    })
                };
                self.finish_session();
                vec![self.event("output", output), self.snapshot_event()]
            }
            EngineEvent::Failure {
                session_id,
                code,
                message,
            } => {
                if self.engine_backend.is_none() {
                    return Vec::new();
                }
                if let Some(session_id) = session_id {
                    if self.session.as_ref().map(|session| session.id) != Some(session_id) {
                        return Vec::new();
                    }
                }
                let output = self.session.take().map(|session| {
                    self.last_finished_session = Some(session.id);
                    self.event(
                        "output",
                        json!({
                            "session_id": session.id,
                            "ok": false,
                            "text": "",
                            "error": message,
                            "error_code": code
                        }),
                    )
                });
                self.engine.disable();
                self.engine_backend = None;
                self.phase = BackendPhase::Failed;
                self.error = Some(ErrorBody::new(&code, &message));
                let mut events = Vec::new();
                if let Some(output) = output {
                    events.push(output);
                }
                events.push(self.event("error", json!({"code": code, "message": message})));
                events.push(self.snapshot_event());
                events
            }
        }
    }

    pub fn handle_model_event(&mut self, event: ModelEvent) -> Vec<Event> {
        match event {
            ModelEvent::Progress {
                model,
                downloaded_bytes,
                total_bytes,
            } => {
                if self.model_installing.as_deref() != Some(&model) {
                    return Vec::new();
                }
                let percent = if total_bytes == 0 {
                    0
                } else {
                    ((downloaded_bytes as u128 * 100) / total_bytes as u128).min(100) as u64
                };
                vec![self.event(
                    "model_progress",
                    json!({
                        "model": model,
                        "status": "downloading",
                        "downloaded_bytes": downloaded_bytes,
                        "total_bytes": total_bytes,
                        "percent": percent
                    }),
                )]
            }
            ModelEvent::Finished { model, result } => {
                if self.model_installing.as_deref() != Some(&model) {
                    return Vec::new();
                }
                self.model_installing = None;
                match result {
                    Ok(path) => {
                        self.error = None;
                        if model == self.settings.model {
                            self.model_installed = true;
                        }
                        if self.enabled && model == self.settings.model && self.session.is_none() {
                            self.arm_backend();
                        }
                        let size = model::spec(&model).map(|spec| spec.size).unwrap_or(0);
                        vec![
                            self.event(
                                "model_progress",
                                json!({
                                    "model": model,
                                    "status": "completed",
                                    "path": path,
                                    "downloaded_bytes": size,
                                    "total_bytes": size,
                                    "percent": 100
                                }),
                            ),
                            self.snapshot_event(),
                        ]
                    }
                    Err(ModelError::Cancelled) => {
                        self.phase = if self.enabled {
                            BackendPhase::SetupRequired
                        } else {
                            BackendPhase::Stopped
                        };
                        let size = model::spec(&model).map(|spec| spec.size).unwrap_or(0);
                        vec![
                            self.event(
                                "model_progress",
                                json!({
                                    "model": model,
                                    "status": "cancelled",
                                    "downloaded_bytes": 0,
                                    "total_bytes": size,
                                    "percent": 0
                                }),
                            ),
                            self.snapshot_event(),
                        ]
                    }
                    Err(error) => {
                        self.phase = BackendPhase::Failed;
                        self.error = Some(ErrorBody::new(error.code(), error.to_string()));
                        let size = model::spec(&model).map(|spec| spec.size).unwrap_or(0);
                        vec![
                            self.event(
                                "model_progress",
                                json!({
                                    "model": model,
                                    "status": "failed",
                                    "downloaded_bytes": 0,
                                    "total_bytes": size,
                                    "percent": 0
                                }),
                            ),
                            self.event(
                                "error",
                                json!({"code": error.code(), "message": error.to_string()}),
                            ),
                            self.snapshot_event(),
                        ]
                    }
                }
            }
        }
    }

    fn update_settings(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let patch = params
            .get("settings")
            .filter(|value| value.is_object())
            .ok_or_else(|| CoreError::new("INVALID_SETTINGS", "settings must be an object"))?;
        let old = self.settings.clone();
        let updated = self.settings.apply_patch(patch);
        self.store
            .save(&updated)
            .map_err(|error| CoreError::new("SETTINGS_IO_FAILED", error.to_string()))?;
        self.settings = updated;
        if old.model != self.settings.model {
            if self.session.is_some() {
                // Do not hash a multi-hundred-MiB model while recording.
                self.model_installed = false;
            } else {
                self.refresh_model_installed();
            }
        }
        self.error = None;

        if engine_settings_changed(&old, &self.settings) && self.session.is_none() {
            self.finish_session();
        }

        let mut result = CommandResult::value(to_value(self.snapshot())?);
        result.events.push(self.snapshot_event());
        Ok(result)
    }

    fn set_enabled(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| CoreError::new("INVALID_SETTINGS", "enabled must be a boolean"))?;
        self.auto_start_pending = false;
        if enabled && (!self.enabled || self.phase == BackendPhase::Failed) {
            self.refresh_model_installed();
            self.arm_backend();
        } else if !enabled && self.enabled {
            self.enabled = false;
            if let Some(session) = self.session.take() {
                self.last_finished_session = Some(session.id);
            }
            self.engine.disable();
            self.engine_backend = None;
            self.phase = BackendPhase::Stopped;
            self.error = None;
        }
        let mut result = CommandResult::value(to_value(self.snapshot())?);
        result.events.push(self.snapshot_event());
        Ok(result)
    }

    fn record_start(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let session_id = required_session_id(&params)?;
        if self.last_finished_session == Some(session_id) {
            return Ok(CommandResult::value(to_value(self.snapshot())?));
        }
        if self.session.as_ref().map(|session| session.id) == Some(session_id) {
            return Ok(CommandResult::value(to_value(self.snapshot())?));
        }
        if self.session.is_some()
            || matches!(
                self.phase,
                BackendPhase::Recording | BackendPhase::Transcribing
            )
        {
            return Err(CoreError::new("BUSY", "another session is active"));
        }
        if self.model_installing.is_some() {
            return Err(CoreError::new("BUSY", "a model is downloading"));
        }
        if !self.enabled || self.phase != BackendPhase::Ready {
            return Err(CoreError::new("NOT_READY", "backend is not ready"));
        }
        let started = self.engine.enable(&self.settings).and_then(|backend| {
            self.engine.start_recording(session_id)?;
            Ok(backend)
        });
        let backend = match started {
            Ok(backend) => backend,
            Err(error) => {
                self.engine.disable();
                self.phase = BackendPhase::Failed;
                self.engine_backend = None;
                self.error = Some(ErrorBody::new(error.code(), error.to_string()));
                return Err(CoreError::new(error.code(), error.to_string()));
            }
        };
        self.engine_backend = Some(backend);
        self.session = Some(Session {
            id: session_id,
            stop_requested: false,
        });
        self.phase = BackendPhase::Recording;
        self.error = None;
        let mut result = CommandResult::value(to_value(self.snapshot())?);
        result.events.push(self.snapshot_event());
        Ok(result)
    }

    fn record_stop(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let session_id = required_session_id(&params)?;
        let Some(session) = self.session.as_ref() else {
            if self.last_finished_session == Some(session_id) {
                return Ok(CommandResult::value(to_value(self.snapshot())?));
            }
            return Err(CoreError::new("STALE_SESSION", "no active session"));
        };
        if session.id != session_id {
            return Err(CoreError::new(
                "STALE_SESSION",
                "session does not own recording",
            ));
        }
        if self.phase == BackendPhase::Transcribing {
            return Ok(CommandResult::value(to_value(self.snapshot())?));
        }
        if self.phase != BackendPhase::Recording {
            return Err(CoreError::new("NOT_RECORDING", "session is not recording"));
        }
        if let Some(session) = self.session.as_mut() {
            session.stop_requested = true;
        }
        if let Err(error) = self.engine.stop_recording(session_id) {
            self.session = None;
            self.last_finished_session = Some(session_id);
            self.engine.disable();
            self.engine_backend = None;
            self.phase = BackendPhase::Failed;
            self.error = Some(ErrorBody::new(error.code(), error.to_string()));
            return Err(CoreError::new(error.code(), error.to_string()));
        }
        self.phase = BackendPhase::Transcribing;
        let mut result = CommandResult::value(to_value(self.snapshot())?);
        result.events.push(self.snapshot_event());
        Ok(result)
    }

    fn cancel_session(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let requested = params.get("session_id").and_then(Value::as_u64);
        if let Some(requested) = requested {
            if self.session.as_ref().map(|session| session.id) != Some(requested) {
                return Ok(CommandResult::value(to_value(self.snapshot())?));
            }
        }
        self.cancel_current_session();
        let mut result = CommandResult::value(to_value(self.snapshot())?);
        result.events.push(self.snapshot_event());
        Ok(result)
    }

    fn install_model(&mut self, params: Value) -> Result<CommandResult, CoreError> {
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&self.settings.model);
        model::spec(model).map_err(|error| CoreError::new(error.code(), error.to_string()))?;
        if let Some(active) = &self.model_installing {
            if active == model {
                return Ok(CommandResult::value(to_value(self.snapshot())?));
            }
            return Err(CoreError::new("BUSY", "another model is downloading"));
        }
        if self.session.is_some() {
            return Err(CoreError::new(
                "BUSY",
                "cannot install a model during recording",
            ));
        }
        self.model_installing = Some(model.to_string());
        let size = model::spec(model)
            .map(|spec| spec.size)
            .expect("model was validated above");
        let mut result = CommandResult::value(json!({"accepted": true, "model": model}));
        result.effects.push(Effect::InstallModel {
            model: model.to_string(),
        });
        result.events.push(self.event(
            "model_progress",
            json!({
                "model": model,
                "status": "starting",
                "downloaded_bytes": 0,
                "total_bytes": size,
                "percent": 0
            }),
        ));
        Ok(result)
    }

    fn cancel_model(&mut self) -> Result<CommandResult, CoreError> {
        let mut result = CommandResult::value(json!({
            "accepted": self.model_installing.is_some()
        }));
        if self.model_installing.is_some() {
            result.effects.push(Effect::CancelModel);
        }
        Ok(result)
    }

    fn shutdown(&mut self) -> CommandResult {
        self.enabled = false;
        if let Some(session) = self.session.take() {
            self.last_finished_session = Some(session.id);
        }
        self.engine.disable();
        self.engine_backend = None;
        self.phase = BackendPhase::Stopped;
        let mut result = CommandResult::value(json!({"shutting_down": true}));
        result.shutdown = true;
        result
    }

    fn arm_backend(&mut self) {
        self.enabled = true;
        self.error = None;
        self.phase = if self.model_installed {
            BackendPhase::Ready
        } else {
            BackendPhase::SetupRequired
        };
    }

    fn cancel_current_session(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.last_finished_session = Some(session.id);
        self.finish_session();
    }

    fn finish_session(&mut self) {
        self.engine.disable();
        self.engine_backend = None;
        self.error = None;
        if self.enabled && !self.model_installed {
            self.refresh_model_installed();
        }
        self.phase = if !self.enabled {
            BackendPhase::Stopped
        } else if !self.model_installed {
            BackendPhase::SetupRequired
        } else {
            BackendPhase::Ready
        };
    }

    fn refresh_model_installed(&mut self) {
        self.model_installed = (self.model_checker)(&self.settings.model);
    }

    fn snapshot_event(&mut self) -> Event {
        self.seq = self.seq.saturating_add(1);
        Event {
            v: PROTOCOL_VERSION,
            kind: "event",
            instance_id: self.instance_id.clone(),
            seq: self.seq,
            name: "snapshot".to_string(),
            payload: serde_json::to_value(self.snapshot())
                .expect("serializing Snapshot is infallible"),
        }
    }

    fn event(&mut self, name: &str, payload: Value) -> Event {
        self.seq = self.seq.saturating_add(1);
        Event {
            v: PROTOCOL_VERSION,
            kind: "event",
            instance_id: self.instance_id.clone(),
            seq: self.seq,
            name: name.to_string(),
            payload,
        }
    }
}

impl<E: Engine> Drop for Service<E> {
    fn drop(&mut self) {
        self.engine.disable();
    }
}

fn required_session_id(params: &Value) -> Result<u64, CoreError> {
    params
        .get("session_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| CoreError::new("INVALID_REQUEST", "session_id must be an unsigned integer"))
}

fn engine_settings_changed(old: &Settings, updated: &Settings) -> bool {
    old.model != updated.model
        || old.language != updated.language
        || old.gpu_enabled != updated.gpu_enabled
}

fn to_value<T: Serialize>(value: T) -> Result<Value, CoreError> {
    serde_json::to_value(value).map_err(|error| CoreError::new("INTERNAL_ERROR", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineError;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        enabled: usize,
        disabled: usize,
        enabled_settings: Vec<(String, String, bool)>,
        starts: Vec<u64>,
        stops: Vec<u64>,
        fail_enable: bool,
        fail_start: bool,
    }

    struct FakeEngine(Arc<Mutex<FakeState>>);

    impl Engine for FakeEngine {
        fn enable(&mut self, settings: &Settings) -> Result<String, EngineError> {
            let mut state = self.0.lock().unwrap();
            state.enabled += 1;
            state.enabled_settings.push((
                settings.model.clone(),
                settings.language.clone(),
                settings.gpu_enabled,
            ));
            if state.fail_enable {
                return Err(EngineError::Start("model failed to load".to_string()));
            }
            Ok("fake".to_string())
        }

        fn disable(&mut self) {
            self.0.lock().unwrap().disabled += 1;
        }

        fn start_recording(&mut self, session_id: u64) -> Result<(), EngineError> {
            let mut state = self.0.lock().unwrap();
            state.starts.push(session_id);
            if state.fail_start {
                return Err(EngineError::Command("microphone unavailable".to_string()));
            }
            Ok(())
        }

        fn stop_recording(&mut self, session_id: u64) -> Result<(), EngineError> {
            self.0.lock().unwrap().stops.push(session_id);
            Ok(())
        }
    }

    fn service_unstarted() -> (
        Service<FakeEngine>,
        Arc<Mutex<FakeState>>,
        tempfile::TempDir,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeState::default()));
        let engine = FakeEngine(state.clone());
        let service = Service::with_model_checker(
            engine,
            SettingsStore::new(directory.path()),
            "test-instance".to_string(),
            Arc::new(|_| true),
        )
        .unwrap();
        (service, state, directory)
    }

    fn service() -> (
        Service<FakeEngine>,
        Arc<Mutex<FakeState>>,
        tempfile::TempDir,
    ) {
        let (mut service, state, directory) = service_unstarted();
        service.activate_auto_start();
        (service, state, directory)
    }

    #[test]
    fn auto_start_arms_ready_without_loading_backend() {
        let (mut service, state, _directory) = service_unstarted();
        assert!(!service.snapshot().enabled);
        assert_eq!(state.lock().unwrap().enabled, 0);
        let events = service.activate_auto_start();
        assert_eq!(events.len(), 1);
        assert!(service.snapshot().enabled);
        assert_eq!(service.snapshot().phase, BackendPhase::Ready);
        assert!(service.snapshot().engine_backend.is_none());
        assert!(!service.has_background_work());
        assert_eq!(state.lock().unwrap().enabled, 0);
    }

    #[test]
    fn engines_live_only_during_sessions_and_background_work_tracks_downloads() {
        let (mut service, state, directory) = service();
        service
            .handle("set_enabled", json!({"enabled": false}))
            .unwrap();
        service
            .handle("set_enabled", json!({"enabled": true}))
            .unwrap();
        service
            .handle("update_settings", json!({"settings": {"language": "zh"}}))
            .unwrap();
        service
            .handle("install_model", json!({"model": "small"}))
            .unwrap();
        assert!(service.has_background_work());
        assert_eq!(
            service
                .handle("record_start", json!({"session_id": 1}))
                .unwrap_err()
                .code,
            "BUSY"
        );
        assert_eq!(state.lock().unwrap().enabled, 0);
        service.handle("cancel_model", json!({})).unwrap();
        assert!(service.has_background_work());
        service.handle_model_event(ModelEvent::Finished {
            model: "small".to_string(),
            result: Ok(directory.path().join("model.bin")),
        });
        assert!(!service.has_background_work());
        assert_eq!(state.lock().unwrap().enabled, 0);
        assert!(service.snapshot().engine_backend.is_none());

        for (session_id, text, stop_requested) in
            [(1, "完成", true), (2, " ", true), (3, "截止", false)]
        {
            let disabled_before = state.lock().unwrap().disabled;
            service
                .handle("record_start", json!({"session_id": session_id}))
                .unwrap();
            service
                .handle("record_start", json!({"session_id": session_id}))
                .unwrap();
            assert_eq!(state.lock().unwrap().enabled, session_id as usize);
            assert!(service.has_background_work());
            assert_eq!(service.snapshot().engine_backend.as_deref(), Some("fake"));
            if stop_requested {
                service
                    .handle("record_stop", json!({"session_id": session_id}))
                    .unwrap();
                assert_eq!(state.lock().unwrap().disabled, disabled_before);
            }
            let terminal = EngineEvent::Transcription {
                session_id,
                text: text.to_string(),
                stop_requested,
            };
            assert!(!service.handle_engine_event(terminal.clone()).is_empty());
            assert!(service.handle_engine_event(terminal).is_empty());
            assert_eq!(state.lock().unwrap().disabled, disabled_before + 1);
            assert!(!service.has_background_work());
            assert_eq!(service.snapshot().phase, BackendPhase::Ready);
            assert!(service.snapshot().engine_backend.is_none());
        }

        service
            .handle("record_start", json!({"session_id": 4}))
            .unwrap();
        let disabled_before = state.lock().unwrap().disabled;
        service
            .handle("cancel_session", json!({"session_id": 4}))
            .unwrap();
        assert_eq!(state.lock().unwrap().disabled, disabled_before + 1);
        assert!(!service.has_background_work());
        assert!(service.snapshot().engine_backend.is_none());
        assert!(service
            .handle_engine_event(EngineEvent::Transcription {
                session_id: 4,
                text: "late".to_string(),
                stop_requested: true,
            })
            .is_empty());
        assert!(service
            .handle_engine_event(EngineEvent::Failure {
                session_id: None,
                code: "ENGINE_EXITED".to_string(),
                message: "old daemon exited".to_string(),
            })
            .is_empty());
        assert_eq!(service.snapshot().phase, BackendPhase::Ready);

        for (session_id, fail_enable) in [(5, true), (6, false)] {
            {
                let mut state = state.lock().unwrap();
                state.fail_enable = fail_enable;
                state.fail_start = !fail_enable;
            }
            service
                .handle("set_enabled", json!({"enabled": true}))
                .unwrap();
            let disabled_before = state.lock().unwrap().disabled;
            assert!(service
                .handle("record_start", json!({"session_id": session_id}))
                .is_err());
            assert_eq!(state.lock().unwrap().enabled, session_id as usize);
            assert_eq!(state.lock().unwrap().disabled, disabled_before + 1);
            assert!(!service.has_background_work());
            assert!(service.snapshot().engine_backend.is_none());
            assert_eq!(service.snapshot().phase, BackendPhase::Failed);
        }
    }

    #[test]
    fn quick_start_stop_is_serial_and_idempotent() {
        let (mut service, state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 9}))
            .unwrap();
        service
            .handle("record_start", json!({"session_id": 9}))
            .unwrap();
        service
            .handle("record_stop", json!({"session_id": 9}))
            .unwrap();
        service
            .handle("record_stop", json!({"session_id": 9}))
            .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.starts, vec![9]);
        assert_eq!(state.stops, vec![9]);
        assert_eq!(service.snapshot().phase, BackendPhase::Transcribing);
    }

    #[test]
    fn another_session_is_busy_and_cannot_stop_owner() {
        let (mut service, _state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 1}))
            .unwrap();
        assert_eq!(
            service
                .handle("record_start", json!({"session_id": 2}))
                .unwrap_err()
                .code,
            "BUSY"
        );
        assert_eq!(
            service
                .handle("record_stop", json!({"session_id": 2}))
                .unwrap_err()
                .code,
            "STALE_SESSION"
        );
    }

    #[test]
    fn empty_transcription_emits_failed_output_and_never_success() {
        let (mut service, _state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 4}))
            .unwrap();
        service
            .handle("record_stop", json!({"session_id": 4}))
            .unwrap();
        let events = service.handle_engine_event(EngineEvent::Transcription {
            session_id: 4,
            text: " \n\t".to_string(),
            stop_requested: true,
        });
        let output = events.iter().find(|event| event.name == "output").unwrap();
        assert_eq!(output.payload["ok"], false);
        assert_eq!(output.payload["text"], "");
        assert_eq!(output.payload["error_code"], "NO_SPEECH");
        assert_eq!(service.snapshot().phase, BackendPhase::Ready);
    }

    #[test]
    fn late_result_after_cancel_is_ignored() {
        let (mut service, state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 5}))
            .unwrap();
        service
            .handle("cancel_session", json!({"session_id": 5}))
            .unwrap();
        assert!(service
            .handle_engine_event(EngineEvent::Transcription {
                session_id: 5,
                text: "late".to_string(),
                stop_requested: true,
            })
            .is_empty());
        assert_eq!(state.lock().unwrap().disabled, 1);
    }

    #[test]
    fn disabling_drops_session_and_late_result() {
        let (mut service, _state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 6}))
            .unwrap();
        service
            .handle("set_enabled", json!({"enabled": false}))
            .unwrap();
        assert_eq!(service.snapshot().phase, BackendPhase::Stopped);
        assert!(service
            .handle_engine_event(EngineEvent::Transcription {
                session_id: 6,
                text: "late".to_string(),
                stop_requested: true,
            })
            .is_empty());
    }

    #[test]
    fn output_event_has_monotonic_instance_sequence() {
        let (mut service, _state, _directory) = service();
        let first = service
            .handle("set_enabled", json!({"enabled": false}))
            .unwrap()
            .events
            .pop()
            .unwrap();
        let second = service
            .handle("set_enabled", json!({"enabled": true}))
            .unwrap()
            .events
            .pop()
            .unwrap();
        assert_eq!(first.instance_id, "test-instance");
        assert!(second.seq > first.seq);
        assert_eq!(second.payload["seq"], second.seq);
    }

    #[test]
    fn engine_settings_changed_during_session_are_loaded_on_next_recording() {
        for (patch, model, language) in [
            (json!({"model": "base"}), "base", "auto"),
            (json!({"language": "zh"}), "small", "zh"),
        ] {
            let (mut service, state, _directory) = service();
            service
                .handle("record_start", json!({"session_id": 30}))
                .unwrap();
            let disables_before = state.lock().unwrap().disabled;
            service
                .handle("update_settings", json!({"settings": patch}))
                .unwrap();
            assert_eq!(service.snapshot().phase, BackendPhase::Recording);
            assert_eq!(state.lock().unwrap().disabled, disables_before);

            service
                .handle("record_stop", json!({"session_id": 30}))
                .unwrap();
            let events = service.handle_engine_event(EngineEvent::Transcription {
                session_id: 30,
                text: "完成".to_string(),
                stop_requested: true,
            });
            assert!(events.iter().any(|event| event.name == "output"));
            assert_eq!(service.snapshot().phase, BackendPhase::Ready);
            assert!(service.snapshot().engine_backend.is_none());
            assert_eq!(state.lock().unwrap().enabled, 1);
            service
                .handle("record_start", json!({"session_id": 35}))
                .unwrap();
            let state = state.lock().unwrap();
            assert!(state.disabled > disables_before);
            assert_eq!(
                state.enabled_settings.last(),
                Some(&(model.to_string(), language.to_string(), true))
            );
        }
    }

    #[test]
    fn cancelling_after_engine_settings_change_loads_them_on_next_recording() {
        for (patch, language, gpu_enabled) in [
            (json!({"gpu_enabled": false}), "auto", false),
            (json!({"language": "zh"}), "zh", true),
        ] {
            let (mut service, state, _directory) = service();
            service
                .handle("record_start", json!({"session_id": 31}))
                .unwrap();
            let disables_before = state.lock().unwrap().disabled;
            service
                .handle("update_settings", json!({"settings": patch}))
                .unwrap();
            assert_eq!(service.snapshot().phase, BackendPhase::Recording);

            service
                .handle("cancel_session", json!({"session_id": 31}))
                .unwrap();

            assert_eq!(service.snapshot().phase, BackendPhase::Ready);
            assert!(service.snapshot().engine_backend.is_none());
            assert_eq!(state.lock().unwrap().enabled, 1);
            service
                .handle("record_start", json!({"session_id": 35}))
                .unwrap();
            let state = state.lock().unwrap();
            assert!(state.disabled > disables_before);
            assert_eq!(
                state.enabled_settings.last(),
                Some(&("small".to_string(), language.to_string(), gpu_enabled))
            );
        }
    }

    #[test]
    fn language_change_without_active_session_waits_for_recording() {
        let (mut service, state, _directory) = service();
        let disables_before = state.lock().unwrap().disabled;

        let result = service
            .handle("update_settings", json!({"settings": {"language": "zh"}}))
            .unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].name, "snapshot");
        assert_eq!(result.events[0].payload["settings"]["language"], "zh");
        assert_eq!(service.snapshot().phase, BackendPhase::Ready);
        assert_eq!(service.snapshot().settings.language, "zh");
        assert!(service.snapshot().engine_backend.is_none());
        assert_eq!(state.lock().unwrap().enabled, 0);
        service
            .handle("record_start", json!({"session_id": 35}))
            .unwrap();
        let state = state.lock().unwrap();
        assert!(state.disabled > disables_before);
        assert_eq!(
            state.enabled_settings.last(),
            Some(&("small".to_string(), "zh".to_string(), true))
        );
    }

    #[test]
    fn automatic_max_duration_terminal_finishes_recording_and_late_stop_is_idempotent() {
        let (mut service, _state, _directory) = service();
        service
            .handle("record_start", json!({"session_id": 32}))
            .unwrap();

        let events = service.handle_engine_event(EngineEvent::Transcription {
            session_id: 32,
            text: "自动截止".to_string(),
            stop_requested: false,
        });

        let output = events.iter().find(|event| event.name == "output").unwrap();
        assert_eq!(output.payload["ok"], true);
        assert_eq!(service.snapshot().phase, BackendPhase::Ready);
        assert!(service
            .handle("record_stop", json!({"session_id": 32}))
            .is_ok());
    }

    #[test]
    fn failed_model_install_emits_terminal_progress_before_error() {
        let (mut service, _state, _directory) = service();
        service
            .handle("install_model", json!({"model": "base"}))
            .unwrap();
        let events = service.handle_model_event(ModelEvent::Finished {
            model: "base".to_string(),
            result: Err(ModelError::Download("network unavailable".to_string())),
        });
        assert_eq!(events[0].name, "model_progress");
        assert_eq!(events[0].payload["status"], "failed");
        assert_eq!(events[0].payload["percent"], 0);
        assert_eq!(events[1].name, "error");
        assert_eq!(events[2].name, "snapshot");
    }
}
