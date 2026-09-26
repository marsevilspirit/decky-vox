use fs2::available_space;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};
use std::time::Duration;

const DOWNLOAD_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
const PROGRESS_STEP_BYTES: u64 = 4 * 1024 * 1024;
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        name: "tiny",
        filename: "ggml-tiny.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        size: 77_691_713,
    },
    ModelSpec {
        name: "base",
        filename: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        size: 147_951_465,
    },
    ModelSpec {
        name: "small",
        filename: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        size: 487_601_967,
    },
    ModelSpec {
        name: "medium",
        filename: "ggml-medium.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        size: 1_533_763_059,
    },
];

#[derive(Debug)]
pub enum ModelEvent {
    Progress {
        model: String,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Finished {
        model: String,
        result: Result<PathBuf, ModelError>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown model {0}")]
    UnknownModel(String),
    #[error("not enough free space: need {needed} bytes, have {available} bytes")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("model download was cancelled")]
    Cancelled,
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("model checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ModelError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownModel(_) => "INVALID_SETTINGS",
            Self::InsufficientSpace { .. } => "INSUFFICIENT_SPACE",
            Self::Cancelled => "MODEL_DOWNLOAD_CANCELLED",
            Self::Download(_) => "MODEL_DOWNLOAD_FAILED",
            Self::SizeMismatch { .. } | Self::ChecksumMismatch { .. } => "MODEL_CHECKSUM_MISMATCH",
            Self::Io(_) => "MODEL_INSTALL_FAILED",
        }
    }
}

pub fn spec(name: &str) -> Result<&'static ModelSpec, ModelError> {
    MODELS
        .iter()
        .find(|candidate| candidate.name == name)
        .ok_or_else(|| ModelError::UnknownModel(name.to_string()))
}

pub fn models_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.join("data").join("voxtype").join("models")
}

pub fn is_installed(settings_dir: &Path, name: &str) -> bool {
    let Ok(model) = spec(name) else {
        return false;
    };
    let path = models_dir(settings_dir).join(model.filename);
    verify_file(&path, model).is_ok()
}

pub fn spawn_install(
    settings_dir: PathBuf,
    model_name: String,
    cancel: Arc<AtomicBool>,
    events: Sender<ModelEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = install(&settings_dir, &model_name, &cancel, &events);
        let _ = events.send(ModelEvent::Finished {
            model: model_name,
            result,
        });
    })
}

fn install(
    settings_dir: &Path,
    model_name: &str,
    cancel: &AtomicBool,
    events: &Sender<ModelEvent>,
) -> Result<PathBuf, ModelError> {
    let model = spec(model_name)?;
    let directory = models_dir(settings_dir);
    fs::create_dir_all(&directory)?;
    let destination = directory.join(model.filename);

    if verify_file(&destination, model).is_ok() {
        return Ok(destination);
    }

    let needed = model.size.saturating_add(DOWNLOAD_HEADROOM_BYTES);
    let available = available_space(&directory)?;
    if available < needed {
        return Err(ModelError::InsufficientSpace { needed, available });
    }
    if cancel.load(Ordering::Acquire) {
        return Err(ModelError::Cancelled);
    }

    // Read timeout is an inactivity limit, not a total model-download
    // deadline. It bounds cancel_model's worst-case wait on a stalled socket.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout_read(DOWNLOAD_READ_TIMEOUT)
        .build();
    let response = agent
        .get(model.url)
        .set("User-Agent", "Decky-Vox/0.1")
        .call()
        .map_err(|error| ModelError::Download(error.to_string()))?;
    install_from_reader(
        response.into_reader(),
        &destination,
        model,
        cancel,
        |downloaded| {
            let _ = events.send(ModelEvent::Progress {
                model: model.name.to_string(),
                downloaded_bytes: downloaded,
                total_bytes: model.size,
            });
        },
    )?;
    Ok(destination)
}

fn install_from_reader<R, F>(
    mut reader: R,
    destination: &Path,
    model: &ModelSpec,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<(), ModelError>
where
    R: Read,
    F: FnMut(u64),
{
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid model path"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.part", model.filename));
    let result = (|| {
        let mut output = File::create(&temporary)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut downloaded = 0_u64;
        let mut next_progress = 0_u64;

        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(ModelError::Cancelled);
            }
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            hasher.update(&buffer[..count]);
            downloaded = downloaded.saturating_add(count as u64);
            if downloaded >= next_progress || downloaded == model.size {
                on_progress(downloaded.min(model.size));
                next_progress = downloaded.saturating_add(PROGRESS_STEP_BYTES);
            }
            if downloaded > model.size {
                return Err(ModelError::SizeMismatch {
                    expected: model.size,
                    actual: downloaded,
                });
            }
        }

        if downloaded != model.size {
            return Err(ModelError::SizeMismatch {
                expected: model.size,
                actual: downloaded,
            });
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != model.sha256 {
            return Err(ModelError::ChecksumMismatch {
                expected: model.sha256.to_string(),
                actual,
            });
        }
        output.sync_all()?;
        fs::rename(&temporary, destination)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn verify_file(path: &Path, model: &ModelSpec) -> Result<(), ModelError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != model.size {
        return Err(ModelError::SizeMismatch {
            expected: model.size,
            actual: metadata.len(),
        });
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != model.sha256 {
        return Err(ModelError::ChecksumMismatch {
            expected: model.sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec() -> ModelSpec {
        ModelSpec {
            name: "fixture",
            filename: "fixture.bin",
            url: "https://invalid.example/fixture.bin",
            sha256: "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
            size: 11,
        }
    }

    #[test]
    fn catalog_contains_only_multilingual_v1_models() {
        assert_eq!(
            MODELS.iter().map(|model| model.name).collect::<Vec<_>>(),
            vec!["tiny", "base", "small", "medium"]
        );
        assert_eq!(spec("small").unwrap().size, 487_601_967);
        assert!(matches!(spec("small.en"), Err(ModelError::UnknownModel(_))));
        assert_eq!(DOWNLOAD_CONNECT_TIMEOUT, Duration::from_secs(30));
        assert_eq!(DOWNLOAD_READ_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn reader_install_verifies_hash_and_renames_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fixture.bin");
        let cancel = AtomicBool::new(false);
        let mut progress = Vec::new();
        install_from_reader(
            &b"hello world"[..],
            &destination,
            &fixture_spec(),
            &cancel,
            |value| progress.push(value),
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"hello world");
        assert!(!directory.path().join(".fixture.bin.part").exists());
        assert_eq!(progress.last(), Some(&11));
    }

    #[test]
    fn hash_failure_removes_partial_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fixture.bin");
        let cancel = AtomicBool::new(false);
        let error = install_from_reader(
            &b"HELLO WORLD"[..],
            &destination,
            &fixture_spec(),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(error, ModelError::ChecksumMismatch { .. }));
        assert!(!destination.exists());
        assert!(!directory.path().join(".fixture.bin.part").exists());
    }

    #[test]
    fn cancellation_removes_partial_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fixture.bin");
        let cancel = AtomicBool::new(true);
        let error = install_from_reader(
            &b"hello world"[..],
            &destination,
            &fixture_spec(),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(error, ModelError::Cancelled));
        assert!(!directory.path().join(".fixture.bin.part").exists());
    }
}
