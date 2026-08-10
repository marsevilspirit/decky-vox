use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to start process: {0}")]
    Spawn(#[source] io::Error),
    #[error("process timed out after {0:?}")]
    Timeout(Duration),
    #[error("process exited with {status}: {stderr}")]
    Exit { status: ExitStatus, stderr: String },
    #[error("process I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub fn isolate_process(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Safety: only async-signal-safe libc functions are called between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    // The parent may have exited between fork and prctl, in
                    // which case the signal edge has already been missed.
                    if libc::getppid() == 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "core exited before child parent-death monitoring was installed",
                        ));
                    }
                }
                Ok(())
            });
        }
    }
}

pub fn restore_host_library_path(command: &mut Command) {
    if let Some(original) = std::env::var_os("LD_LIBRARY_PATH_ORIG") {
        command.env("LD_LIBRARY_PATH", original);
    } else {
        command.env_remove("LD_LIBRARY_PATH");
    }
    command.env_remove("LD_LIBRARY_PATH_ORIG");
}

pub fn run_checked(mut command: Command, timeout: Duration) -> Result<(), ProcessError> {
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    isolate_process(&mut command);
    let mut child = command.spawn().map_err(ProcessError::Spawn)?;
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            terminate_group(child.id());
            if child.wait_timeout(Duration::from_millis(500))?.is_none() {
                kill_group(child.id());
                let _ = child.wait();
            }
            return Err(ProcessError::Timeout(timeout));
        }
    };
    let stderr = read_pipe(child.stderr.take());
    if status.success() {
        Ok(())
    } else {
        Err(ProcessError::Exit { status, stderr })
    }
}

fn read_pipe<R: Read>(pipe: Option<R>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).trim().to_string()
}

pub fn stop_child_group(child: &mut Child, timeout: Duration) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    terminate_group(child.id());
    if child.wait_timeout(timeout)?.is_none() {
        kill_group(child.id());
        let _ = child.wait();
    }
    Ok(())
}

pub fn terminate_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_command_reports_nonzero_exit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let error = run_checked(command, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(error, ProcessError::Exit { .. }));
    }
}
