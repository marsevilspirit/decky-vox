// The process/config strategy is derived in part from mimed95/decky-voxtype
// (commit cbb2201dcad36cf5291ec360c9fb1183fe9071be, BSD-3-Clause).
// In particular: --no-hotkey daemon mode, status --follow JSON, isolated XDG
// paths, file-only output, and restoring Decky/PyInstaller's library path.

use crate::process::{
    isolate_process, kill_group, restore_host_library_path, run_checked, stop_child_group,
};
use crate::settings::Settings;
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, mpsc::Sender, Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const RECORD_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const RECORDING_READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    Transcription {
        session_id: u64,
        text: String,
        stop_requested: bool,
    },
    Failure {
        session_id: Option<u64>,
        code: String,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("voxtype binary is missing: {0}")]
    BinaryMissing(PathBuf),
    #[error("failed to write voxtype configuration: {0}")]
    Config(#[source] io::Error),
    #[error("failed to start voxtype: {0}")]
    Start(String),
    #[error("voxtype command failed: {0}")]
    Command(String),
    #[error("session {0} is not active in the engine")]
    SessionMismatch(u64),
}

impl EngineError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BinaryMissing(_) => "ENGINE_BINARY_MISSING",
            Self::Config(_) | Self::Start(_) => "ENGINE_START_FAILED",
            Self::Command(_) => "MICROPHONE_UNAVAILABLE",
            Self::SessionMismatch(_) => "STALE_SESSION",
        }
    }
}

pub trait Engine: Send {
    fn enable(&mut self, settings: &Settings) -> Result<String, EngineError>;
    fn disable(&mut self);
    fn start_recording(&mut self, session_id: u64) -> Result<(), EngineError>;
    fn stop_recording(&mut self, session_id: u64) -> Result<(), EngineError>;
}

#[derive(Debug, Clone)]
pub struct VoxtypePaths {
    pub cpu_binary: PathBuf,
    pub vulkan_binary: PathBuf,
    pub settings_dir: PathBuf,
    /// Isolated Decky Vox scratch space, not the host audio runtime directory.
    pub runtime_dir: PathBuf,
    /// The deck user's `/run/user/<uid>` (or inherited XDG equivalent).
    pub audio_runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    pub instance_id: String,
}

#[derive(Debug, Clone)]
struct ActiveCapture {
    session_id: u64,
    output_path: PathBuf,
    recording_seen: bool,
    stop_requested: bool,
    recording_ack: Option<mpsc::SyncSender<()>>,
}

pub struct VoxtypeEngine {
    paths: VoxtypePaths,
    events: Sender<EngineEvent>,
    daemon: Option<Arc<Mutex<Child>>>,
    daemon_stop: Option<Arc<AtomicBool>>,
    status_pid: Option<u32>,
    status_stop: Option<Arc<AtomicBool>>,
    daemon_thread: Option<std::thread::JoinHandle<()>>,
    status_thread: Option<std::thread::JoinHandle<()>>,
    active_binary: Option<PathBuf>,
    active_capture: Arc<Mutex<Option<ActiveCapture>>>,
}

impl VoxtypeEngine {
    pub fn new(paths: VoxtypePaths, events: Sender<EngineEvent>) -> Result<Self, EngineError> {
        let engine = Self {
            paths,
            events,
            daemon: None,
            daemon_stop: None,
            status_pid: None,
            status_stop: None,
            daemon_thread: None,
            status_thread: None,
            active_binary: None,
            active_capture: Arc::new(Mutex::new(None)),
        };
        // Privacy cleanup cannot depend on a model being installed or the
        // engine being enabled: SetupRequired must still remove crash leftovers.
        engine.cleanup_stale_session_directories()?;
        Ok(engine)
    }

    fn select_binary(&self, settings: &Settings) -> Result<(PathBuf, String), EngineError> {
        if settings.gpu_enabled && self.paths.vulkan_binary.is_file() {
            return Ok((self.paths.vulkan_binary.clone(), "vulkan".to_string()));
        }
        if self.paths.cpu_binary.is_file() {
            let backend = if settings.gpu_enabled {
                "cpu_fallback"
            } else {
                "cpu"
            };
            return Ok((self.paths.cpu_binary.clone(), backend.to_string()));
        }
        Err(EngineError::BinaryMissing(self.paths.cpu_binary.clone()))
    }

    fn prepare_binary(path: &Path) -> Result<(), EngineError> {
        let metadata =
            fs::metadata(path).map_err(|_| EngineError::BinaryMissing(path.to_path_buf()))?;
        if !metadata.is_file() {
            return Err(EngineError::BinaryMissing(path.to_path_buf()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(EngineError::Start(format!(
                    "voxtype binary is not executable: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn xdg_config_home(&self) -> PathBuf {
        self.paths.settings_dir.join("config")
    }

    fn xdg_data_home(&self) -> PathBuf {
        self.paths.settings_dir.join("data")
    }

    fn session_directory(&self) -> PathBuf {
        self.session_root().join(&self.paths.instance_id)
    }

    fn session_root(&self) -> PathBuf {
        self.paths.runtime_dir.join("decky-vox")
    }

    fn isolated_xdg_runtime(&self) -> PathBuf {
        self.session_directory().join("xdg-runtime")
    }

    fn prepare_session_directory(&self) -> Result<(), EngineError> {
        self.cleanup_stale_session_directories()?;
        let directory = self.session_directory();
        fs::create_dir_all(&directory).map_err(EngineError::Config)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(EngineError::Config)?;
        }
        remove_result_files(&directory)?;
        Ok(())
    }

    fn cleanup_stale_session_directories(&self) -> Result<(), EngineError> {
        let root = self.session_root();
        fs::create_dir_all(&root).map_err(EngineError::Config)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(EngineError::Config)?;
        }
        for entry in fs::read_dir(&root).map_err(EngineError::Config)? {
            let entry = entry.map_err(EngineError::Config)?;
            if !entry.file_type().map_err(EngineError::Config)?.is_dir()
                || entry.file_name() == self.paths.instance_id.as_str()
                || Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
            {
                continue;
            }
            // A second core should not normally coexist, but deleting only its
            // direct result files is fail-closed and avoids following symlinks
            // or destroying unrelated runtime state. The older core will emit
            // NO_SPEECH instead of leaving plaintext behind.
            remove_result_files(&entry.path())?;
        }
        Ok(())
    }

    fn write_config(&self, settings: &Settings, output_path: &Path) -> Result<(), EngineError> {
        let config_dir = self.xdg_config_home().join("voxtype");
        fs::create_dir_all(&config_dir).map_err(EngineError::Config)?;
        fs::create_dir_all(self.xdg_data_home()).map_err(EngineError::Config)?;
        self.prepare_session_directory()?;
        let isolated_runtime = self.isolated_xdg_runtime();
        fs::create_dir_all(&isolated_runtime).map_err(EngineError::Config)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&isolated_runtime, fs::Permissions::from_mode(0o700))
                .map_err(EngineError::Config)?;
        }
        let config_path = config_dir.join("config.toml");
        let temporary = config_dir.join(".config.toml.tmp");
        let text = render_config(settings, output_path);
        let result = (|| {
            let mut file = File::create(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, &config_path)?;
            File::open(&config_dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result.map_err(EngineError::Config)
    }

    fn configure_environment(&self, command: &mut Command) {
        command.env("XDG_CONFIG_HOME", self.xdg_config_home());
        command.env("XDG_DATA_HOME", self.xdg_data_home());
        // Keep voxtype's state, pid and control files isolated while routing the
        // audio clients back to the deck user's real PipeWire/Pulse sockets.
        command.env("XDG_RUNTIME_DIR", self.isolated_xdg_runtime());
        command.env("PIPEWIRE_RUNTIME_DIR", &self.paths.audio_runtime_dir);
        command.env(
            "PULSE_RUNTIME_PATH",
            self.paths.audio_runtime_dir.join("pulse"),
        );
        // voxtype applies VOXTYPE_* after its config file. Strip all inherited
        // prefixed keys so they cannot enable remote ASR, clipboard/type output,
        // or auto-submit behind this adapter's file-only contract.
        command.env_remove("RUST_LOG");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("VOXTYPE_") {
                command.env_remove(key);
            }
        }
        restore_host_library_path(command);
    }

    fn start_daemon(&mut self, binary: &Path, settings: &Settings) -> Result<(), EngineError> {
        self.stop_daemon();
        let idle_output = self.session_directory().join("idle.txt");
        self.write_config(settings, &idle_output)?;
        fs::create_dir_all(&self.paths.log_dir).map_err(EngineError::Config)?;
        let log_path = self.paths.log_dir.join("voxtype-daemon.log");
        let log = File::create(log_path).map_err(EngineError::Config)?;
        let status_log = log.try_clone().map_err(EngineError::Config)?;
        let daemon_error_log = log.try_clone().map_err(EngineError::Config)?;

        let mut daemon_command = Command::new(binary);
        daemon_command
            .args(daemon_argv())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(daemon_error_log));
        self.configure_environment(&mut daemon_command);
        isolate_process(&mut daemon_command);
        let daemon = daemon_command
            .spawn()
            .map_err(|error| EngineError::Start(error.to_string()))?;
        let daemon = Arc::new(Mutex::new(daemon));
        let daemon_stop = Arc::new(AtomicBool::new(false));
        self.daemon = Some(daemon.clone());
        self.daemon_stop = Some(daemon_stop.clone());

        let mut status_command = Command::new(binary);
        status_command
            .args(status_argv())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(status_log));
        self.configure_environment(&mut status_command);
        isolate_process(&mut status_command);
        let mut status = match status_command.spawn() {
            Ok(status) => status,
            Err(error) => {
                self.stop_daemon();
                return Err(EngineError::Start(error.to_string()));
            }
        };
        let status_pid = status.id();
        let stdout = match status.stdout.take() {
            Some(stdout) => stdout,
            None => {
                self.stop_daemon();
                return Err(EngineError::Start(
                    "status monitor has no stdout".to_string(),
                ));
            }
        };
        let status_stop = Arc::new(AtomicBool::new(false));
        let status_alive = Arc::new(AtomicBool::new(true));
        let status_published = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        self.status_thread = Some(self.spawn_status_monitor(
            status,
            stdout,
            status_stop.clone(),
            status_alive.clone(),
            status_published.clone(),
            ready_tx,
        ));
        self.status_pid = Some(status_pid);
        self.status_stop = Some(status_stop);

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(())) => break,
                Ok(Err(message)) => {
                    self.stop_daemon();
                    return Err(EngineError::Start(message));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop_daemon();
                    return Err(EngineError::Start(
                        "status monitor ended before ready".to_string(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let daemon_exit = daemon
                .lock()
                .map_err(|_| EngineError::Start("daemon lock poisoned".to_string()))?
                .try_wait()
                .map_err(|error| EngineError::Start(error.to_string()))?;
            if let Some(status) = daemon_exit {
                self.stop_daemon();
                return Err(EngineError::Start(format!(
                    "daemon exited during startup with {status}"
                )));
            }
            if !status_alive.load(Ordering::Acquire) {
                self.stop_daemon();
                return Err(EngineError::Start(
                    "status monitor exited during startup".to_string(),
                ));
            }
            if Instant::now() >= deadline {
                self.stop_daemon();
                return Err(EngineError::Start(format!(
                    "daemon did not become idle within {READY_TIMEOUT:?}"
                )));
            }
        }

        status_published.store(true, Ordering::Release);
        if !status_alive.load(Ordering::Acquire) {
            self.stop_daemon();
            return Err(EngineError::Start(
                "status monitor exited after ready signal".to_string(),
            ));
        }
        self.daemon_thread = Some(self.spawn_daemon_watcher(daemon, daemon_stop));
        Ok(())
    }

    fn spawn_daemon_watcher(
        &self,
        daemon: Arc<Mutex<Child>>,
        stopping: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        let events = self.events.clone();
        let active_capture = self.active_capture.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let exited = daemon
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok().flatten());
            if let Some(status) = exited {
                if !stopping.load(Ordering::Acquire) {
                    let session_id = active_capture
                        .lock()
                        .ok()
                        .and_then(|capture| capture.as_ref().map(|capture| capture.session_id));
                    let _ = events.send(EngineEvent::Failure {
                        session_id,
                        code: "ENGINE_EXITED".to_string(),
                        message: format!("voxtype daemon exited with {status}"),
                    });
                }
                break;
            }
            if stopping.load(Ordering::Acquire) {
                break;
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_status_monitor(
        &self,
        mut child: Child,
        stdout: impl io::Read + Send + 'static,
        stopping: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
        published: Arc<AtomicBool>,
        ready: Sender<Result<(), String>>,
    ) -> std::thread::JoinHandle<()> {
        let events = self.events.clone();
        let active_capture = self.active_capture.clone();
        std::thread::spawn(move || {
            let mut ready = Some(ready);
            for line in BufReader::new(stdout).lines() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(line) = line else {
                    break;
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(state) = value.get("class").and_then(Value::as_str) else {
                    continue;
                };
                if state == "idle" {
                    if let Some(sender) = ready.take() {
                        let _ = sender.send(Ok(()));
                    }
                } else if matches!(state, "failed" | "error") {
                    if let Some(sender) = ready.take() {
                        let _ =
                            sender.send(Err(format!("voxtype reported {state} during startup")));
                    }
                }

                // v0.6.5 can go recording -> idle directly and inotify may
                // coalesce an intermediate transcribing write. Only a capture
                // that first reached recording/transcribing may terminate on
                // idle; the daemon's initial idle cannot consume a session.
                if let Some(capture) = observe_capture_state(&active_capture, state) {
                    let text = fs::read_to_string(&capture.output_path).unwrap_or_default();
                    let _ = fs::remove_file(&capture.output_path);
                    let _ = events.send(EngineEvent::Transcription {
                        session_id: capture.session_id,
                        text,
                        stop_requested: capture.stop_requested,
                    });
                }
            }
            alive.store(false, Ordering::Release);
            let status = child.wait().ok();
            if let Some(sender) = ready.take() {
                let _ = sender.send(Err(format!(
                    "status monitor exited before ready ({status:?})"
                )));
            }
            if !stopping.load(Ordering::Acquire) && published.load(Ordering::Acquire) {
                let session_id = active_capture
                    .lock()
                    .ok()
                    .and_then(|capture| capture.as_ref().map(|capture| capture.session_id));
                let _ = events.send(EngineEvent::Failure {
                    session_id,
                    code: "ENGINE_STATUS_FAILED".to_string(),
                    message: format!("voxtype status monitor exited ({status:?})"),
                });
            }
        })
    }

    fn stop_daemon(&mut self) {
        if let Some(stop) = self.status_stop.take() {
            stop.store(true, Ordering::Release);
        }
        if let Some(pid) = self.status_pid.take() {
            kill_group(pid);
        }
        if let Some(stop) = self.daemon_stop.take() {
            stop.store(true, Ordering::Release);
        }
        if let Some(daemon) = self.daemon.take() {
            if let Ok(mut daemon) = daemon.lock() {
                let _ = stop_child_group(&mut daemon, STOP_TIMEOUT);
            }
        }
        // Reap the monitors before another session can start. Otherwise an old
        // watcher could report a failure against the next session's capture.
        for thread in [self.status_thread.take(), self.daemon_thread.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        if let Ok(mut active_capture) = self.active_capture.lock() {
            if let Some(capture) = active_capture.take() {
                let _ = fs::remove_file(capture.output_path);
            }
        }
    }

    fn run_record_command(&self, arguments: Vec<OsString>) -> Result<(), EngineError> {
        let binary = self
            .active_binary
            .as_ref()
            .ok_or_else(|| EngineError::Start("engine is not enabled".to_string()))?;
        let mut command = Command::new(binary);
        command.args(arguments);
        self.configure_environment(&mut command);
        run_checked(command, RECORD_COMMAND_TIMEOUT)
            .map_err(|error| EngineError::Command(error.to_string()))
    }
}

impl Engine for VoxtypeEngine {
    fn enable(&mut self, settings: &Settings) -> Result<String, EngineError> {
        let (preferred, mut backend) = self.select_binary(settings)?;
        Self::prepare_binary(&preferred)?;
        let initial = self.start_daemon(&preferred, settings);
        let selected = if initial.is_err()
            && settings.gpu_enabled
            && preferred == self.paths.vulkan_binary
            && self.paths.cpu_binary.is_file()
        {
            Self::prepare_binary(&self.paths.cpu_binary)?;
            let cpu = self.paths.cpu_binary.clone();
            self.start_daemon(&cpu, settings)?;
            backend = "cpu_fallback".to_string();
            cpu
        } else {
            initial?;
            preferred
        };
        self.active_binary = Some(selected);
        Ok(backend)
    }

    fn disable(&mut self) {
        self.stop_daemon();
        self.active_binary = None;
    }

    fn start_recording(&mut self, session_id: u64) -> Result<(), EngineError> {
        if self.active_binary.is_none() {
            return Err(EngineError::Start("engine is not enabled".to_string()));
        }
        let output_path = self
            .session_directory()
            .join(format!("session-{session_id}.txt"));
        let _ = fs::remove_file(&output_path);
        let (recording_tx, recording_rx) = mpsc::sync_channel(1);
        let mut capture = self
            .active_capture
            .lock()
            .map_err(|_| EngineError::Start("capture lock poisoned".to_string()))?;
        if capture.is_some() {
            return Err(EngineError::Command(
                "another engine capture is active".to_string(),
            ));
        }
        *capture = Some(ActiveCapture {
            session_id,
            output_path: output_path.clone(),
            recording_seen: false,
            stop_requested: false,
            recording_ack: Some(recording_tx),
        });
        drop(capture);
        if let Err(error) = self.run_record_command(record_start_argv(&output_path)) {
            self.disable();
            return Err(error);
        }
        if let Err(error) = wait_for_recording(&recording_rx, RECORDING_READY_TIMEOUT) {
            self.disable();
            return Err(error);
        }
        Ok(())
    }

    fn stop_recording(&mut self, session_id: u64) -> Result<(), EngineError> {
        {
            let mut active = self
                .active_capture
                .lock()
                .map_err(|_| EngineError::Start("capture lock poisoned".to_string()))?;
            match active.as_mut() {
                Some(capture) if capture.session_id == session_id => {
                    capture.stop_requested = true;
                }
                // The status monitor may have already delivered an automatic
                // max-duration terminal event for this service-owned session.
                None => return Ok(()),
                Some(_) => return Err(EngineError::SessionMismatch(session_id)),
            }
        }
        if let Err(error) = self.run_record_command(record_action_argv("stop")) {
            self.disable();
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for VoxtypeEngine {
    fn drop(&mut self) {
        self.disable();
        let _ = fs::remove_dir_all(self.session_directory());
    }
}

pub fn record_action_argv(action: &str) -> Vec<OsString> {
    vec![
        OsString::from("--quiet"),
        OsString::from("record"),
        OsString::from(action),
    ]
}

pub fn record_start_argv(output_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--quiet"),
        OsString::from("record"),
        OsString::from("start"),
        OsString::from(format!("--file={}", output_path.to_string_lossy())),
    ]
}

fn daemon_argv() -> [&'static str; 3] {
    ["--quiet", "--no-hotkey", "daemon"]
}

fn status_argv() -> [&'static str; 5] {
    ["--quiet", "status", "--follow", "--format", "json"]
}

fn render_config(settings: &Settings, output_path: &Path) -> String {
    format!(
        "state_file = \"auto\"\nengine = \"whisper\"\n\n\
         [hotkey]\nenabled = false\nkey = \"SCROLLLOCK\"\n\n\
         [audio]\ndevice = \"default\"\nsample_rate = 16000\nmax_duration_secs = 60\n\n\
         [audio.feedback]\nenabled = false\n\n\
         [whisper]\nbackend = \"local\"\nmodel = \"{}\"\nlanguage = \"{}\"\ntranslate = false\n\n\
         [output]\nmode = \"file\"\nfile_path = \"{}\"\nfile_mode = \"overwrite\"\nfallback_to_clipboard = false\nauto_submit = false\n\n\
         [output.notification]\non_recording_start = false\non_recording_stop = false\non_transcription = false\n",
        toml_escape(&settings.model),
        toml_escape(&settings.language),
        toml_escape(&output_path.to_string_lossy()),
    )
}

fn observe_capture_state(
    active_capture: &Arc<Mutex<Option<ActiveCapture>>>,
    state: &str,
) -> Option<ActiveCapture> {
    let mut capture = active_capture.lock().ok()?;
    let active = capture.as_mut()?;
    match state {
        "recording" | "transcribing" => {
            active.recording_seen = true;
            if let Some(sender) = active.recording_ack.take() {
                let _ = sender.try_send(());
            }
            None
        }
        "idle" if active.recording_seen => capture.take(),
        _ => None,
    }
}

fn wait_for_recording(receiver: &mpsc::Receiver<()>, timeout: Duration) -> Result<(), EngineError> {
    receiver.recv_timeout(timeout).map_err(|error| {
        EngineError::Command(format!(
            "microphone did not enter recording within {timeout:?}: {error}"
        ))
    })
}

fn remove_result_files(directory: &Path) -> Result<(), EngineError> {
    for entry in fs::read_dir(directory).map_err(EngineError::Config)? {
        let entry = entry.map_err(EngineError::Config)?;
        if !entry.file_type().map_err(EngineError::Config)?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "idle.txt" || (name.starts_with("session-") && name.ends_with(".txt")) {
            fs::remove_file(entry.path()).map_err(EngineError::Config)?;
        }
    }
    Ok(())
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn paths(root: &Path) -> VoxtypePaths {
        VoxtypePaths {
            cpu_binary: root.join("voxtype"),
            vulkan_binary: root.join("voxtype-vulkan"),
            settings_dir: root.join("settings"),
            runtime_dir: root.join("runtime"),
            audio_runtime_dir: root.join("host-audio"),
            log_dir: root.join("logs"),
            instance_id: "test".to_string(),
        }
    }

    #[test]
    fn record_commands_are_fixed_argv_without_a_shell() {
        assert_eq!(
            record_action_argv("stop"),
            vec![
                OsString::from("--quiet"),
                OsString::from("record"),
                OsString::from("stop")
            ]
        );
        assert_eq!(
            record_start_argv(Path::new("/tmp/session 7.txt")),
            vec![
                OsString::from("--quiet"),
                OsString::from("record"),
                OsString::from("start"),
                OsString::from("--file=/tmp/session 7.txt")
            ]
        );
        assert_eq!(daemon_argv(), ["--quiet", "--no-hotkey", "daemon"]);
        assert_eq!(
            status_argv(),
            ["--quiet", "status", "--follow", "--format", "json"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn packaged_binary_permissions_are_validated_without_being_modified() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("voxtype");
        fs::write(&binary, b"binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o555)).unwrap();

        VoxtypeEngine::prepare_binary(&binary).unwrap();

        assert_eq!(
            fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_packaged_binary_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("voxtype");
        fs::write(&binary, b"binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();

        let error = VoxtypeEngine::prepare_binary(&binary).unwrap_err();

        assert_eq!(error.code(), "ENGINE_START_FAILED");
        assert!(error.to_string().contains("not executable"));
        assert_eq!(
            fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn config_uses_top_level_state_file_file_only_output_and_no_notifications() {
        let config = render_config(&Settings::default(), Path::new("/tmp/result.txt"));
        assert!(config.starts_with("state_file = \"auto\"\n"));
        assert!(!config.contains("[integration]"));
        assert!(config.contains("mode = \"file\""));
        assert!(config.contains("model = \"small\""));
        assert!(config.contains("language = \"auto\""));
        assert!(config.contains("backend = \"local\""));
        assert!(config.contains("[hotkey]\nenabled = false"));
        assert!(config.contains("[audio.feedback]\nenabled = false"));
        assert!(config.contains("fallback_to_clipboard = false"));
        assert!(config.contains("auto_submit = false"));
        assert!(config.contains("[output.notification]"));
        assert!(config.contains("on_transcription = false"));
    }

    #[test]
    fn config_uses_selected_language() {
        let settings = Settings {
            language: "zh".to_string(),
            ..Settings::default()
        };
        let config = render_config(&settings, Path::new("/tmp/result.txt"));
        assert!(config.contains("language = \"zh\""));
        assert!(!config.contains("language = \"auto\""));
    }

    #[test]
    fn audio_sockets_use_host_runtime_while_voxtype_state_stays_isolated() {
        const CHILD_FLAG: &str = "DECKY_VOX_ENV_TEST_CHILD";
        if std::env::var_os(CHILD_FLAG).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "engine::tests::audio_sockets_use_host_runtime_while_voxtype_state_stays_isolated",
                ])
                .env(CHILD_FLAG, "1")
                .env("VOXTYPE_AUTO_SUBMIT", "true")
                .env("VOXTYPE_REMOTE_ENDPOINT", "https://invalid.example")
                .env("VOXTYPE_FUTURE_OPTION", "enabled")
                .env("RUST_LOG", "trace")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = mpsc::channel();
        let engine = VoxtypeEngine::new(paths(directory.path()), events).unwrap();
        let mut command = Command::new("/usr/bin/env");
        engine.configure_environment(&mut command);
        let environment = command
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect::<HashMap<_, _>>();
        let isolated_runtime = engine.isolated_xdg_runtime();
        let pulse_runtime = engine.paths.audio_runtime_dir.join("pulse");
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("XDG_RUNTIME_DIR"))
                .copied(),
            Some(isolated_runtime.as_os_str())
        );
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("PIPEWIRE_RUNTIME_DIR"))
                .copied(),
            Some(engine.paths.audio_runtime_dir.as_os_str())
        );
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("PULSE_RUNTIME_PATH"))
                .copied(),
            Some(pulse_runtime.as_os_str())
        );
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "RUST_LOG" && value.is_none()));
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with("VOXTYPE_") || line.starts_with("RUST_LOG=")));
    }

    #[test]
    fn toml_strings_escape_paths() {
        assert_eq!(toml_escape("a\\b\"c"), "a\\\\b\\\"c");
    }

    #[test]
    fn recording_ack_is_required_and_any_later_idle_is_terminal() {
        let (recording_tx, recording_rx) = mpsc::sync_channel(1);
        let captures = Arc::new(Mutex::new(Some(ActiveCapture {
            session_id: 8,
            output_path: PathBuf::from("/tmp/eight.txt"),
            recording_seen: false,
            stop_requested: false,
            recording_ack: Some(recording_tx),
        })));
        assert!(observe_capture_state(&captures, "idle").is_none());
        assert!(recording_rx.try_recv().is_err());
        assert!(observe_capture_state(&captures, "recording").is_none());
        recording_rx
            .recv_timeout(Duration::from_millis(20))
            .unwrap();
        let capture = observe_capture_state(&captures, "idle").unwrap();
        assert_eq!(capture.session_id, 8);
        assert!(captures.lock().unwrap().is_none());
    }

    #[test]
    fn transcribing_ack_covers_coalesced_recording_state() {
        let (recording_tx, recording_rx) = mpsc::sync_channel(1);
        let captures = Arc::new(Mutex::new(Some(ActiveCapture {
            session_id: 9,
            output_path: PathBuf::from("/tmp/nine.txt"),
            recording_seen: false,
            stop_requested: true,
            recording_ack: Some(recording_tx),
        })));
        assert!(observe_capture_state(&captures, "transcribing").is_none());
        recording_rx
            .recv_timeout(Duration::from_millis(20))
            .unwrap();
        assert!(observe_capture_state(&captures, "idle").is_some());
    }

    #[test]
    fn microphone_failure_without_recording_ack_times_out_fail_closed() {
        let (_recording_tx, recording_rx) = mpsc::sync_channel(1);
        let error = wait_for_recording(&recording_rx, Duration::from_millis(1)).unwrap_err();
        assert_eq!(error.code(), "MICROPHONE_UNAVAILABLE");
        assert!(error.to_string().contains("did not enter recording"));
    }

    #[test]
    fn initialization_cleans_old_results_without_enabling_engine() {
        let directory = tempfile::tempdir().unwrap();
        let old_directory = directory
            .path()
            .join("runtime")
            .join("decky-vox")
            .join("00000000-0000-4000-8000-000000000003");
        fs::create_dir_all(&old_directory).unwrap();
        fs::write(old_directory.join("session-crash.txt"), "plaintext").unwrap();
        let (events, _receiver) = mpsc::channel();

        let _engine = VoxtypeEngine::new(paths(directory.path()), events).unwrap();

        assert!(!old_directory.join("session-crash.txt").exists());
    }

    #[test]
    fn session_directory_is_private_and_old_results_are_removed() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = mpsc::channel();
        let engine = VoxtypeEngine::new(paths(directory.path()), events).unwrap();
        let session_directory = engine.session_directory();
        let stale_directory = engine
            .session_root()
            .join("00000000-0000-4000-8000-000000000001");
        let live_directory = engine
            .session_root()
            .join("00000000-0000-4000-8000-000000000002");
        fs::create_dir_all(&session_directory).unwrap();
        fs::create_dir_all(&stale_directory).unwrap();
        fs::create_dir_all(&live_directory).unwrap();
        fs::write(
            live_directory.join("core.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        fs::write(stale_directory.join("session-old.txt"), "stale").unwrap();
        fs::write(live_directory.join("idle.txt"), "live plaintext").unwrap();
        fs::write(session_directory.join("session-1.txt"), "private").unwrap();
        fs::write(session_directory.join("keep.bin"), "keep").unwrap();

        engine.prepare_session_directory().unwrap();

        assert!(stale_directory.exists());
        assert!(live_directory.exists());
        assert!(!stale_directory.join("session-old.txt").exists());
        assert!(!live_directory.join("idle.txt").exists());
        assert!(live_directory.join("core.pid").exists());
        assert!(!session_directory.join("session-1.txt").exists());
        assert!(session_directory.join("keep.bin").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(session_directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn disable_reaps_processes_and_monitors_before_returning() {
        let directory = tempfile::tempdir().unwrap();
        let (events, receiver) = mpsc::channel();
        let mut engine = VoxtypeEngine::new(paths(directory.path()), events).unwrap();
        let mut daemon_command = Command::new("/bin/sleep");
        daemon_command.arg("60");
        isolate_process(&mut daemon_command);
        let daemon = Arc::new(Mutex::new(daemon_command.spawn().unwrap()));
        let daemon_stop = Arc::new(AtomicBool::new(false));
        engine.daemon = Some(daemon.clone());
        engine.daemon_stop = Some(daemon_stop.clone());
        engine.daemon_thread = Some(engine.spawn_daemon_watcher(daemon.clone(), daemon_stop));

        let mut status_command = Command::new("/bin/sleep");
        status_command.arg("60").stdout(Stdio::piped());
        isolate_process(&mut status_command);
        let mut status = status_command.spawn().unwrap();
        engine.status_pid = Some(status.id());
        let stdout = status.stdout.take().unwrap();
        let status_stop = Arc::new(AtomicBool::new(false));
        let status_alive = Arc::new(AtomicBool::new(true));
        let (ready, _ready_receiver) = mpsc::channel();
        engine.status_stop = Some(status_stop.clone());
        engine.status_thread = Some(engine.spawn_status_monitor(
            status,
            stdout,
            status_stop,
            status_alive.clone(),
            Arc::new(AtomicBool::new(true)),
            ready,
        ));

        let output_path = directory.path().join("result.txt");
        fs::write(&output_path, "private").unwrap();
        *engine.active_capture.lock().unwrap() = Some(ActiveCapture {
            session_id: 45,
            output_path: output_path.clone(),
            recording_seen: true,
            stop_requested: false,
            recording_ack: None,
        });
        engine.disable();
        engine.disable();

        assert!(daemon.lock().unwrap().try_wait().unwrap().is_some());
        assert!(!status_alive.load(Ordering::Acquire));
        assert!(engine.daemon_thread.is_none());
        assert!(engine.status_thread.is_none());
        assert!(engine.active_capture.lock().unwrap().is_none());
        assert!(!output_path.exists());
        assert!(receiver.try_recv().is_err());
    }
}
