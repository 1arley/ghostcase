use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use tempfile::NamedTempFile;

const STDOUT_LIMIT: usize = 16 * 1024;
const STDERR_LIMIT: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    TargetFailure,
    NotReproduced,
    InvalidCandidate,
    WrongTarget,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum ProtocolResult {
    TargetFailure { target: String },
    NotReproduced,
    InvalidCandidate,
}

pub struct Oracle {
    program: String,
    args: Vec<String>,
    current_dir: std::path::PathBuf,
}

pub fn new(config: &crate::config::OracleConfig, config_path: &Path) -> Result<Oracle> {
    let config_path = config_path
        .canonicalize()
        .context("could not resolve configuration path")?;
    let current_dir = config_path
        .parent()
        .context("configuration has no parent directory")?
        .to_owned();
    let program = crate::config::resolve_program(&config_path, &config.program);
    Ok(Oracle {
        program: program.to_string_lossy().into_owned(),
        args: config.args.clone(),
        current_dir,
    })
}

pub fn run(
    oracle: &mut Oracle,
    document: &Value,
    target: &str,
    timeout_seconds: u64,
) -> Result<Outcome> {
    let mut candidate = NamedTempFile::new().context("could not create oracle input")?;
    serde_json::to_writer(&mut candidate, document).context("could not serialize oracle input")?;
    candidate
        .as_file()
        .sync_all()
        .context("could not sync oracle input")?;
    let input_path = candidate
        .path()
        .to_str()
        .context("temporary input path is not valid Unicode")?;
    let args: Vec<String> = oracle
        .args
        .iter()
        .map(|arg| arg.replace("{input}", input_path))
        .collect();
    let mut command = Command::new(&oracle.program);
    command
        .args(&args)
        .current_dir(&oracle.current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A dedicated process group lets one kill signal reach the adapter and every
    // descendant it spawns. Both supported platforms expose the same POSIX calls.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    bail!("the oracle runner currently supports Linux and macOS only");

    let mut child = command.spawn().context("could not start oracle program")?;
    let stdout = child
        .stdout
        .take()
        .context("could not capture oracle output")?;
    let stderr = child
        .stderr
        .take()
        .context("could not capture oracle diagnostics")?;
    let exceeded_limit = Arc::new(AtomicBool::new(false));
    let stdout_overflow = Arc::clone(&exceeded_limit);
    let stderr_overflow = Arc::clone(&exceeded_limit);
    let stdout_thread = thread::spawn(move || read_limited(stdout, STDOUT_LIMIT, stdout_overflow));
    let stderr_thread = thread::spawn(move || read_limited(stderr, STDERR_LIMIT, stderr_overflow));

    let timeout = Duration::from_secs(timeout_seconds);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if exceeded_limit.load(Ordering::Relaxed) {
            terminate_process_group(child.id());
            let _ = child.wait();
            break None;
        }
        match exit_state(&mut child)? {
            ExitState::Exited => {
                terminate_process_group(child.id());
                break Some(child.wait().context("could not collect oracle result")?);
            }
            ExitState::Reaped(status) => {
                terminate_process_group(child.id());
                break Some(status);
            }
            ExitState::Running => {}
        }
        if Instant::now() >= deadline {
            terminate_process_group(child.id());
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("oracle output reader failed"))??;
    let _stderr = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("oracle diagnostics reader failed"))??;
    anyhow::ensure!(
        !exceeded_limit.load(Ordering::Relaxed),
        "oracle exceeded its output limit"
    );
    if status.is_none() {
        bail!("oracle timed out or exceeded its output limit");
    }
    anyhow::ensure!(
        status.is_some_and(|status| status.success()),
        "oracle exited unsuccessfully"
    );

    let result: ProtocolResult = serde_json::from_slice(&stdout)
        .map_err(|_| anyhow::anyhow!("oracle returned an invalid protocol response"))?;
    match result {
        ProtocolResult::TargetFailure { target: actual } if actual == target => {
            Ok(Outcome::TargetFailure)
        }
        ProtocolResult::TargetFailure { .. } => Ok(Outcome::WrongTarget),
        ProtocolResult::NotReproduced => Ok(Outcome::NotReproduced),
        ProtocolResult::InvalidCandidate => Ok(Outcome::InvalidCandidate),
    }
}

enum ExitState {
    /// The process finished but has not been reaped yet.
    Exited,
    /// The process finished and was already reaped, carrying its exit status.
    Reaped(std::process::ExitStatus),
    Running,
}

/// Reports whether the adapter has finished while leaving it unreaped.
///
/// An unreaped child keeps its PID reserved, so the process group it leads cannot
/// be recycled and the negative-PID kill below stays confined to this run. Where
/// `waitid` is unavailable the status is reaped directly, which is correct but
/// leaves that narrow window open.
fn exit_state(child: &mut std::process::Child) -> Result<ExitState> {
    #[cfg(unix)]
    match has_exited_without_reaping(child.id()) {
        Some(true) => return Ok(ExitState::Exited),
        Some(false) => return Ok(ExitState::Running),
        None => {}
    }
    Ok(
        match child
            .try_wait()
            .context("could not inspect oracle process")?
        {
            Some(status) => ExitState::Reaped(status),
            None => ExitState::Running,
        },
    )
}

#[cfg(unix)]
fn has_exited_without_reaping(id: u32) -> Option<bool> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    // SAFETY: `info` is a live, correctly sized siginfo_t that waitid may fill.
    // WNOWAIT leaves the child waitable, so Child::wait still collects it later.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if result == 0 {
        // SAFETY: waitid reported a state change, so the siginfo_t is initialized.
        Some(unsafe { info.si_pid() } != 0)
    } else {
        None
    }
}

fn read_limited(mut reader: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let mut collected = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let size = reader
            .read(&mut buffer)
            .context("could not read oracle output")?;
        if size == 0 {
            return Ok(collected);
        }
        let remaining = limit.saturating_sub(collected.len());
        collected.extend_from_slice(&buffer[..size.min(remaining)]);
        if size > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(unix)]
fn terminate_process_group(id: u32) {
    // SAFETY: a negative child PID targets only the process group we created for this
    // run, and a leading minus never addresses the caller's own group.
    unsafe {
        libc::kill(-(id as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_id: u32) {}
