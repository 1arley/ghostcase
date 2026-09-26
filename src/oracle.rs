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
        if let Some(status) = child
            .try_wait()
            .context("could not inspect oracle process")?
        {
            terminate_process_group(child.id());
            break Some(status);
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
