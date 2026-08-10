use decky_vox_core::engine::{EngineEvent, VoxtypeEngine, VoxtypePaths};
use decky_vox_core::model::{self, ModelEvent};
use decky_vox_core::protocol::{
    parse_request, write_message, ErrorBody, ParseError, Request, Response, MAX_LINE_BYTES,
    PROTOCOL_VERSION,
};
use decky_vox_core::service::{Effect, Service};
use decky_vox_core::settings::SettingsStore;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use uuid::Uuid;

enum Inbound {
    Request(Request),
    Invalid { id: Option<u64>, error: ParseError },
    Eof,
}

#[derive(Debug)]
struct Args {
    settings_dir: PathBuf,
    runtime_dir: PathBuf,
    log_dir: PathBuf,
    voxtype_cpu: PathBuf,
    voxtype_vulkan: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("decky-vox-core: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    install_parent_death_signal()?;
    let args = Args::parse(std::env::args().skip(1))?;
    fs::create_dir_all(&args.settings_dir)?;
    fs::create_dir_all(&args.runtime_dir)?;
    fs::create_dir_all(&args.log_dir)?;

    let instance_id = Uuid::new_v4().to_string();
    let (engine_tx, engine_rx) = mpsc::channel::<EngineEvent>();
    let (model_tx, model_rx) = mpsc::channel::<ModelEvent>();
    let engine = VoxtypeEngine::new(
        VoxtypePaths {
            cpu_binary: args.voxtype_cpu,
            vulkan_binary: args.voxtype_vulkan,
            settings_dir: args.settings_dir.clone(),
            runtime_dir: args.runtime_dir,
            audio_runtime_dir: host_audio_runtime_dir(),
            log_dir: args.log_dir,
            instance_id: instance_id.clone(),
        },
        engine_tx,
    )?;
    let store = SettingsStore::new(&args.settings_dir);
    let mut service = Service::new(engine, store, args.settings_dir.clone(), instance_id)?;

    let (input_tx, input_rx) = mpsc::channel();
    std::thread::spawn(move || read_stdin(input_tx));
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let mut handshaken = false;
    let mut model_cancel: Option<Arc<AtomicBool>> = None;
    let mut model_thread: Option<std::thread::JoinHandle<()>> = None;

    'actor: loop {
        if handshaken {
            while let Ok(event) = engine_rx.try_recv() {
                for event in service.handle_engine_event(event) {
                    write_message(&mut writer, &event)?;
                }
            }
            while let Ok(event) = model_rx.try_recv() {
                let finished = matches!(event, ModelEvent::Finished { .. });
                for event in service.handle_model_event(event) {
                    write_message(&mut writer, &event)?;
                }
                if finished {
                    model_cancel = None;
                    if let Some(thread) = model_thread.take() {
                        let _ = thread.join();
                    }
                }
            }
        }

        let inbound = match input_rx.recv_timeout(Duration::from_millis(40)) {
            Ok(inbound) => inbound,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => Inbound::Eof,
        };
        match inbound {
            Inbound::Eof => break,
            Inbound::Invalid { id, error } => {
                let response =
                    Response::failure(id, ErrorBody::new(error.code(), error.to_string()));
                write_message(&mut writer, &response)?;
            }
            Inbound::Request(request) => {
                if !handshaken && request.method != "hello" {
                    write_message(
                        &mut writer,
                        &Response::failure(
                            Some(request.id),
                            ErrorBody::new("HANDSHAKE_REQUIRED", "hello must be the first request"),
                        ),
                    )?;
                    continue;
                }
                if request.method == "hello" {
                    let version = request
                        .params
                        .get("protocol_version")
                        .and_then(Value::as_u64);
                    if version != Some(PROTOCOL_VERSION as u64) {
                        let requested = request
                            .params
                            .get("protocol_version")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "missing".to_string());
                        write_message(
                            &mut writer,
                            &Response::failure(
                                Some(request.id),
                                ErrorBody::new(
                                    "PROTOCOL_MISMATCH",
                                    format!(
                                        "bridge requested protocol {requested}; core supports {PROTOCOL_VERSION}"
                                    ),
                                ),
                            ),
                        )?;
                        continue;
                    }
                }

                match service.handle(&request.method, request.params) {
                    Ok(result) => {
                        write_message(&mut writer, &Response::success(request.id, result.value))?;
                        if request.method == "hello" {
                            handshaken = true;
                            // The hello response is already line-flushed above. Only
                            // now may potentially slow local model/daemon startup run.
                            for event in service.activate_auto_start() {
                                write_message(&mut writer, &event)?;
                            }
                        }
                        for event in result.events {
                            write_message(&mut writer, &event)?;
                        }
                        for effect in result.effects {
                            match effect {
                                Effect::InstallModel { model } => {
                                    let cancel = Arc::new(AtomicBool::new(false));
                                    model_thread = Some(model::spawn_install(
                                        args.settings_dir.clone(),
                                        model,
                                        cancel.clone(),
                                        model_tx.clone(),
                                    ));
                                    model_cancel = Some(cancel);
                                }
                                Effect::CancelModel => {
                                    if let Some(cancel) = &model_cancel {
                                        cancel.store(true, Ordering::Release);
                                    }
                                }
                            }
                        }
                        if result.shutdown {
                            break 'actor;
                        }
                    }
                    Err(error) => {
                        write_message(
                            &mut writer,
                            &Response::failure(Some(request.id), error.body()),
                        )?;
                        if handshaken {
                            for event in service.error_events(&error) {
                                write_message(&mut writer, &event)?;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(cancel) = model_cancel {
        cancel.store(true, Ordering::Release);
    }
    // Do not wait indefinitely on a blocked network read. The process exit closes it;
    // completed workers are joined in the normal Finished path above.
    drop(model_thread);
    Ok(())
}

fn install_parent_death_signal() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Install this before any potentially blocking model hash or engine
        // startup. If Decky's Python bridge dies, SIGTERM must interrupt the
        // core even while its stdin thread cannot yet observe EOF.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // Close the race where the parent exits between exec and prctl.
        if unsafe { libc::getppid() } == 1 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Decky bridge exited before parent-death monitoring was installed",
            ));
        }
    }
    Ok(())
}

fn read_stdin(sender: mpsc::Sender<Inbound>) {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        match read_capped_line(&mut reader) {
            Ok(None) => {
                let _ = sender.send(Inbound::Eof);
                return;
            }
            Ok(Some(Ok(line))) => {
                if line.is_empty() {
                    continue;
                }
                let inbound = match parse_request(&line) {
                    Ok(request) => Inbound::Request(request),
                    Err(error) => Inbound::Invalid {
                        id: best_effort_id(&line),
                        error,
                    },
                };
                if sender.send(inbound).is_err() {
                    return;
                }
            }
            Ok(Some(Err(error))) => {
                if sender.send(Inbound::Invalid { id: None, error }).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(Inbound::Invalid {
                    id: None,
                    error: ParseError::Malformed(error.to_string()),
                });
                let _ = sender.send(Inbound::Eof);
                return;
            }
        }
    }
}

fn read_capped_line<R: BufRead>(reader: &mut R) -> io::Result<Option<Result<Vec<u8>, ParseError>>> {
    let mut line = Vec::new();
    let mut too_large = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() && !too_large {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(available.len());
        if !too_large {
            if line.len().saturating_add(count) > MAX_LINE_BYTES {
                too_large = true;
            } else {
                line.extend_from_slice(&available[..count]);
            }
        }
        let consumed = count + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if too_large {
        return Ok(Some(Err(ParseError::TooLarge)));
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(Ok(line)))
}

fn best_effort_id(line: &[u8]) -> Option<u64> {
    serde_json::from_slice::<Value>(line)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_u64))
}

impl Args {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut values = HashMap::new();
        let mut arguments = arguments;
        while let Some(flag) = arguments.next() {
            if !flag.starts_with("--") {
                return Err(format!("unexpected argument {flag}"));
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(format!("duplicate argument {flag}"));
            }
        }
        let plugin_dir = required_absolute(&mut values, "--plugin-dir")?;
        let settings_dir = required_absolute(&mut values, "--settings-dir")?;
        let runtime_dir = required_absolute(&mut values, "--runtime-dir")?;
        let log_dir = required_absolute(&mut values, "--log-dir")?;
        let voxtype_cpu = optional_absolute(&mut values, "--voxtype-cpu")?
            .unwrap_or_else(|| plugin_dir.join("bin").join("voxtype"));
        let voxtype_vulkan = optional_absolute(&mut values, "--voxtype-vulkan")?
            .unwrap_or_else(|| plugin_dir.join("bin").join("voxtype-vulkan"));
        if let Some(flag) = values.keys().next() {
            return Err(format!("unknown argument {flag}"));
        }
        Ok(Self {
            settings_dir,
            runtime_dir,
            log_dir,
            voxtype_cpu,
            voxtype_vulkan,
        })
    }
}

fn required_absolute(values: &mut HashMap<String, String>, flag: &str) -> Result<PathBuf, String> {
    let value = values
        .remove(flag)
        .ok_or_else(|| format!("missing required argument {flag}"))?;
    absolute_path(flag, value)
}

fn optional_absolute(
    values: &mut HashMap<String, String>,
    flag: &str,
) -> Result<Option<PathBuf>, String> {
    values
        .remove(flag)
        .map(|value| absolute_path(flag, value))
        .transpose()
}

fn absolute_path(flag: &str, value: String) -> Result<PathBuf, String> {
    let path = Path::new(&value);
    if !path.is_absolute() {
        return Err(format!("{flag} must be an absolute path"));
    }
    Ok(path.to_path_buf())
}

fn host_audio_runtime_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        // Decky drops the backend to the deck user. The audio session sockets
        // live at this stable per-user path even when plugin_loader strips or
        // replaces XDG_RUNTIME_DIR.
        let uid = unsafe { libc::geteuid() };
        PathBuf::from(format!("/run/user/{uid}"))
    }
    #[cfg(not(target_os = "linux"))]
    if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if path.is_absolute() {
            return path;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_absolute_controlled_paths() {
        let error = Args::parse(
            [
                "--plugin-dir",
                ".",
                "--settings-dir",
                "/settings",
                "--runtime-dir",
                "/run/user/1000",
                "--log-dir",
                "/logs",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap_err();
        assert!(error.contains("--plugin-dir must be an absolute path"));
    }

    #[test]
    fn extracts_id_from_structurally_invalid_request() {
        assert_eq!(best_effort_id(br#"{"id":44,"kind":"wrong"}"#), Some(44));
        assert_eq!(best_effort_id(b"{"), None);
    }

    #[test]
    fn oversized_line_is_drained_without_losing_the_next_request() {
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 10];
        bytes.extend_from_slice(b"\nnext\n");
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        assert!(matches!(
            read_capped_line(&mut reader).unwrap(),
            Some(Err(ParseError::TooLarge))
        ));
        assert_eq!(
            read_capped_line(&mut reader).unwrap().unwrap().unwrap(),
            b"next"
        );
    }

    #[test]
    fn parent_death_monitoring_installs_on_supported_platforms() {
        let result = install_parent_death_signal();
        #[cfg(target_os = "linux")]
        if unsafe { libc::getppid() } == 1 {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            return;
        }
        result.unwrap();
    }
}
