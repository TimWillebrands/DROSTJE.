//! An owned harness child speaking newline-delimited JSON over stdio, as Codex
//! `app-server` and OpenCode `acp` do. This module owns the process group, the
//! nonblocking pipes and bounded framing only; each adapter keeps its own protocol.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

/// Largest stdout frame accepted from a harness; history replies can be large.
const OUTPUT_FRAME: usize = 67_108_864;
/// Unwritten frames allowed before the input pipe counts as stuck.
const WRITE_QUEUE: usize = 128;

pub(super) struct StdioChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    reaped: bool,
    /// Product name used in errors, such as `Codex`.
    label: &'static str,
    input_limit: usize,
    line: Vec<u8>,
    read_buffer: Box<[u8; 16_384]>,
    read_start: usize,
    read_end: usize,
    writes: VecDeque<Vec<u8>>,
    write_offset: usize,
}

fn nonblocking(fd: i32, label: &str) -> Result<()> {
    // SAFETY: fcntl only reads flags from this live pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: the descriptor is owned by this process; adding O_NONBLOCK preserves its flags.
    let changed = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    ensure!(
        flags >= 0 && changed >= 0,
        "Set {label} pipe nonblocking: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

impl StdioChild {
    /// Starts `command` as the leader of a new process group with piped stdio.
    /// `input_limit` bounds one outgoing frame, newline included.
    pub(super) fn spawn(
        mut command: Command,
        label: &'static str,
        input_limit: usize,
    ) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: pre_exec runs after fork and before exec; setsid has no Rust-managed inputs.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let program = command.get_program().to_string_lossy().into_owned();
        let mut child = command
            .spawn()
            .with_context(|| format!("Start {label} at {program}"))?;
        let stdin = child.stdin.take().context("Open harness stdin")?;
        let stdout = child.stdout.take().context("Open harness stdout")?;
        let stderr = child.stderr.take().context("Open harness stderr")?;
        let owned = Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr,
            reaped: false,
            label,
            input_limit,
            line: Vec::new(),
            read_buffer: Box::new([0; 16_384]),
            read_start: 0,
            read_end: 0,
            writes: VecDeque::new(),
            write_offset: 0,
        };
        if let Some(stdin) = &owned.stdin {
            nonblocking(stdin.as_raw_fd(), label)?;
        }
        nonblocking(owned.stdout.as_raw_fd(), label)?;
        nonblocking(owned.stderr.as_raw_fd(), label)?;
        Ok(owned)
    }

    /// Queues one frame; nothing is written until `flush`.
    pub(super) fn queue(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() <= self.input_limit,
            "{} input exceeds {} KiB",
            self.label,
            self.input_limit / 1024
        );
        ensure!(
            self.writes.len() < WRITE_QUEUE,
            "{} input pipe is not accepting commands",
            self.label
        );
        self.writes.push_back(bytes);
        Ok(())
    }

    /// Writes queued frames until the pipe would block.
    pub(super) fn flush(&mut self) -> Result<()> {
        while let Some(write) = self.writes.front() {
            let Some(stdin) = self.stdin.as_mut() else {
                bail!("{} input closed", self.label);
            };
            match stdin.write(&write[self.write_offset..]) {
                Ok(0) => bail!("{} input closed", self.label),
                Ok(count) => {
                    self.write_offset += count;
                    if self.write_offset == write.len() {
                        self.writes.pop_front();
                        self.write_offset = 0;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// The next complete stdout frame, or `None` when no whole line is available yet.
    /// End of file and malformed frames are errors: the protocol cannot resynchronise.
    pub(super) fn read_frame(&mut self) -> Result<Option<Value>> {
        loop {
            if self.read_start == self.read_end {
                match self.stdout.read(&mut self.read_buffer[..]) {
                    Ok(0) => bail!("{} disconnected (stdout EOF)", self.label),
                    Ok(count) => {
                        self.read_start = 0;
                        self.read_end = count;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        return Ok(None);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let pending = &self.read_buffer[self.read_start..self.read_end];
            let length = pending
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(pending.len());
            ensure!(
                self.line.len() + length <= OUTPUT_FRAME,
                "{} stdout frame exceeds 64 MiB",
                self.label
            );
            self.line.extend_from_slice(&pending[..length]);
            self.read_start += length;
            if self.line.last() == Some(&b'\n') {
                let line = std::mem::take(&mut self.line);
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let value: Value = serde_json::from_slice(&line)
                    .with_context(|| format!("Malformed {} stdout JSON/UTF-8", self.label))?;
                ensure!(
                    value.is_object(),
                    "{} stdout frame must be an object",
                    self.label
                );
                return Ok(Some(value));
            }
        }
    }

    /// Diagnostic text the harness wrote to stderr since the last call.
    pub(super) fn read_stderr(&mut self) -> Result<Option<String>> {
        let mut bytes = [0; 4096];
        match self.stderr.read(&mut bytes) {
            Ok(count) if count > 0 => Ok(Some(String::from_utf8_lossy(&bytes[..count]).into())),
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn exited(&mut self) -> Result<Option<ExitStatus>> {
        if self.reaped {
            return Ok(None);
        }
        let status = self
            .child
            .try_wait()
            .with_context(|| format!("Wait for {} child", self.label))?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    pub(super) fn is_running(&mut self) -> bool {
        !self.reaped && matches!(self.child.try_wait(), Ok(None))
    }

    /// Closes stdin, gives the harness five seconds to exit on EOF, then signals its group.
    pub(super) fn close(&mut self) {
        self.stdin.take();
        if self.reaped {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    let mut discard = [0; 16_384];
                    let _ = self.stdout.read(&mut discard);
                    let _ = self.stderr.read(&mut discard);
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
        // SAFETY: this PID is the unreaped child whose setsid call succeeded before exec.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.reaped = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(error) = self.terminate() {
            eprintln!(
                "Could not terminate the owned {} process group: {error:#}",
                self.label
            );
        }
    }

    /// Kills the whole process group at once, without the graceful EOF wait.
    pub(super) fn terminate(&mut self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        // SAFETY: this is the same unreaped owned process group.
        let result = unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.raw_os_error() == Some(libc::ESRCH),
                "Kill owned {} process group: {error}",
                self.label
            );
        }
        self.stdin.take();
        self.child
            .wait()
            .with_context(|| format!("Reap terminated owned {} process", self.label))?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for StdioChild {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    fn next_frame(child: &mut StdioChild) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            child.flush()?;
            if let Some(frame) = child.read_frame()? {
                return Ok(frame);
            }
            ensure!(Instant::now() < deadline, "No frame within 5 seconds");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn frames_round_trip_through_a_real_pipe() -> Result<()> {
        // `cat` echoes each queued frame back as one stdout line.
        let mut child = StdioChild::spawn(shell("exec cat"), "Echo", 1024)?;
        child.queue(&json!({"id":1,"method":"ping"}))?;
        child.queue(&json!({"id":2,"text":"two\nlines"}))?;
        assert_eq!(next_frame(&mut child)?, json!({"id":1,"method":"ping"}));
        assert_eq!(next_frame(&mut child)?["text"], "two\nlines");
        child.close();
        Ok(())
    }

    #[test]
    fn oversized_input_is_refused_before_writing() -> Result<()> {
        let mut child = StdioChild::spawn(shell("exec cat"), "Echo", 32)?;
        let error = child
            .queue(&json!({"text":"x".repeat(64)}))
            .expect_err("frame over the limit");
        assert!(format!("{error:#}").contains("Echo input exceeds"));
        Ok(())
    }

    #[test]
    fn malformed_output_and_eof_are_errors() -> Result<()> {
        let mut child = StdioChild::spawn(shell("printf 'not json\\n'; sleep 5"), "Bad", 1024)?;
        let error = next_frame(&mut child).expect_err("malformed frame");
        assert!(format!("{error:#}").contains("Malformed Bad stdout"));
        child.terminate()?;

        let mut child = StdioChild::spawn(shell("exit 0"), "Gone", 1024)?;
        let error = next_frame(&mut child).expect_err("end of file");
        assert!(format!("{error:#}").contains("Gone disconnected"));
        Ok(())
    }

    #[test]
    fn close_stops_a_child_that_ignores_end_of_input() -> Result<()> {
        let mut child = StdioChild::spawn(shell("trap '' TERM; sleep 30"), "Stuck", 1024)?;
        let started = Instant::now();
        child.terminate()?;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!child.is_running());
        Ok(())
    }
}
