//! An owned harness child speaking newline-delimited JSON over stdio, as Codex
//! `app-server` and OpenCode `acp` do. This module owns the process group, the
//! nonblocking pipes and bounded framing only; each adapter keeps its own protocol.
//! [`LineReader`] is the newline framing every stdio adapter, OMP and Claude Code included,
//! reads its child's stdout with.
use crate::ConversationCommand;
use anyhow::{Context, Result, bail, ensure};
use drostje_platform::WakePipe;
use serde_json::Value;
use smol::stream::Stream;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::fd::{AsFd, BorrowedFd},
    pin::Pin,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::Arc,
    task::{Context as TaskContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};

/// Largest stdout frame accepted from a harness; history replies can be large.
const OUTPUT_FRAME: usize = 67_108_864;
/// Unwritten frames allowed before the input pipe counts as stuck.
const WRITE_QUEUE: usize = 128;
/// A line buffer that grew past this for one large frame is released after it.
const RETAINED_LINE: usize = 1_048_576;

/// Longest an idle adapter loop sleeps, so every deadline it checks fires at most this late.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// The fixed tick for work no descriptor announces: a display that stopped consuming
/// events, or a socket write in progress.
const BUSY_WAIT: Duration = Duration::from_millis(5);

/// How long an adapter loop may sleep after a pass: not at all while queued `events` can
/// still be delivered, [`BUSY_WAIT`] while `output` is full or the caller has `busy` work,
/// otherwise [`IDLE_WAIT`].
pub(super) fn sleep_for<T>(
    events: &VecDeque<T>,
    output: &async_channel::Sender<T>,
    busy: bool,
) -> Duration {
    if !events.is_empty() && !output.is_full() {
        Duration::ZERO
    } else if busy || !events.is_empty() {
        BUSY_WAIT
    } else {
        IDLE_WAIT
    }
}

/// An adapter's command channel, plus the wake pipe that lets its loop block until the
/// child's pipes, a command or a timeout need it, instead of polling on a fixed tick.
pub(super) struct Commands {
    receiver: Pin<Box<async_channel::Receiver<ConversationCommand>>>,
    signal: Arc<Signal>,
    waker: Waker,
    /// The last [`Self::recv`] left a waker registered: a new command, or the channel
    /// closing, signals the pipe. Otherwise commands may be left and nothing sleeps.
    armed: bool,
}

struct Signal(WakePipe);

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake();
    }
}

impl Commands {
    pub(super) fn new(receiver: async_channel::Receiver<ConversationCommand>) -> Result<Self> {
        let signal = Arc::new(Signal(
            WakePipe::new().context("Create conversation wake pipe")?,
        ));
        Ok(Self {
            receiver: Box::pin(receiver),
            waker: Waker::from(signal.clone()),
            signal,
            armed: false,
        })
    }

    /// `try_recv` that, when the channel is empty, registers for the next command.
    pub(super) fn recv(
        &mut self,
    ) -> std::result::Result<ConversationCommand, async_channel::TryRecvError> {
        let mut context = TaskContext::from_waker(&self.waker);
        match self.receiver.as_mut().poll_next(&mut context) {
            Poll::Ready(Some(command)) => {
                self.armed = false;
                Ok(command)
            }
            // A closed channel delivers nothing more, so waiting cannot miss a command.
            Poll::Ready(None) => {
                self.armed = true;
                Err(async_channel::TryRecvError::Closed)
            }
            Poll::Pending => {
                self.armed = true;
                Err(async_channel::TryRecvError::Empty)
            }
        }
    }

    /// Sleeps until one of `readable` has input or hung up, one of `writable` accepts
    /// output, a command arrives, or `timeout` passes. Returns at once when commands may
    /// still be queued.
    pub(super) fn wait(
        &mut self,
        readable: &[BorrowedFd<'_>],
        writable: &[BorrowedFd<'_>],
        timeout: Duration,
    ) -> Result<()> {
        if self.armed && !timeout.is_zero() {
            let mut fds = Vec::with_capacity(readable.len() + 1);
            fds.push(self.signal.0.fd());
            fds.extend_from_slice(readable);
            drostje_platform::wait_until_ready(&fds, writable, timeout)
                .context("Wait for harness input")?;
        }
        self.signal.0.clear();
        Ok(())
    }
}

/// What [`LineReader::next`] found.
pub(super) enum Line<'a> {
    /// One complete line, newline included, valid until the next call.
    Frame(&'a [u8]),
    /// No complete line is available without blocking.
    Pending,
    /// The writer closed the pipe. [`LineReader::partial`] tells whether a line was cut off.
    Eof,
    /// The line would exceed the reader's limit; the stream cannot resynchronise.
    TooLong,
}

/// Bounded newline framing over a nonblocking reader. A line that fits in one read is
/// returned straight from the read buffer; only lines spanning reads are assembled.
pub(super) struct LineReader {
    buffer: Box<[u8; 16_384]>,
    start: usize,
    end: usize,
    line: Vec<u8>,
    /// `line` holds a line already returned, cleared on the next call.
    returned: bool,
    /// Largest line accepted, newline included.
    limit: usize,
}

impl LineReader {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            buffer: Box::new([0; 16_384]),
            start: 0,
            end: 0,
            line: Vec::new(),
            returned: false,
            limit,
        }
    }

    /// Bytes of an unfinished line are buffered.
    pub(super) fn partial(&self) -> bool {
        !self.returned && !self.line.is_empty()
    }

    /// Read bytes are waiting in the buffer, so the descriptor may never become readable
    /// for them: the caller must not sleep.
    pub(super) fn buffered(&self) -> bool {
        self.start < self.end
    }

    /// Reads until one whole line is buffered or `source` would block.
    pub(super) fn next(&mut self, source: &mut impl Read) -> std::io::Result<Line<'_>> {
        if self.returned {
            self.returned = false;
            if self.line.capacity() > RETAINED_LINE {
                self.line = Vec::new();
            } else {
                self.line.clear();
            }
        }
        loop {
            if self.start == self.end {
                match source.read(&mut self.buffer[..]) {
                    Ok(0) => return Ok(Line::Eof),
                    Ok(count) => {
                        self.start = 0;
                        self.end = count;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        return Ok(Line::Pending);
                    }
                    Err(error) => return Err(error),
                }
            }
            let pending = &self.buffer[self.start..self.end];
            let newline = pending.iter().position(|byte| *byte == b'\n');
            let length = newline.map_or(pending.len(), |index| index + 1);
            if self.line.len() + length > self.limit {
                return Ok(Line::TooLong);
            }
            let start = self.start;
            self.start += length;
            if newline.is_some() && self.line.is_empty() {
                return Ok(Line::Frame(&self.buffer[start..start + length]));
            }
            self.line
                .extend_from_slice(&self.buffer[start..start + length]);
            if newline.is_some() {
                self.returned = true;
                return Ok(Line::Frame(&self.line));
            }
        }
    }
}

pub(super) struct StdioChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    /// Cleared at stderr end of file, after which a hung-up stderr would end every wait.
    stderr_open: bool,
    reaped: bool,
    /// Product name used in errors, such as `Codex`.
    label: &'static str,
    input_limit: usize,
    lines: LineReader,
    writes: VecDeque<Vec<u8>>,
    write_offset: usize,
}

fn nonblocking(fd: BorrowedFd<'_>, label: &str) -> Result<()> {
    drostje_platform::set_nonblocking(fd).with_context(|| format!("Set {label} pipe nonblocking"))
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
        drostje_platform::new_session(&mut command);
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
            stderr_open: true,
            reaped: false,
            label,
            input_limit,
            lines: LineReader::new(OUTPUT_FRAME),
            writes: VecDeque::new(),
            write_offset: 0,
        };
        if let Some(stdin) = &owned.stdin {
            nonblocking(stdin.as_fd(), label)?;
        }
        nonblocking(owned.stdout.as_fd(), label)?;
        nonblocking(owned.stderr.as_fd(), label)?;
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
            match self.lines.next(&mut self.stdout)? {
                Line::Pending => return Ok(None),
                Line::Eof => bail!("{} disconnected (stdout EOF)", self.label),
                Line::TooLong => bail!("{} stdout frame exceeds 64 MiB", self.label),
                Line::Frame(line) if line.iter().all(u8::is_ascii_whitespace) => {}
                Line::Frame(line) => {
                    let value: Value = serde_json::from_slice(line)
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
    }

    /// Diagnostic text the harness wrote to stderr since the last call.
    pub(super) fn read_stderr(&mut self) -> Result<Option<String>> {
        if !self.stderr_open {
            return Ok(None);
        }
        let mut bytes = [0; 4096];
        match self.stderr.read(&mut bytes) {
            Ok(count) if count > 0 => Ok(Some(String::from_utf8_lossy(&bytes[..count]).into())),
            Ok(_) => {
                self.stderr_open = false;
                Ok(None)
            }
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

    /// Sleeps on this child's pipes and `commands` for up to `timeout`; see
    /// [`Commands::wait`].
    pub(super) fn wait(&self, commands: &mut Commands, timeout: Duration) -> Result<()> {
        let timeout = if self.lines.buffered() {
            Duration::ZERO
        } else {
            timeout
        };
        let stdout = self.stdout.as_fd();
        let readable = [stdout, self.stderr.as_fd()];
        let readable = &readable[..if self.stderr_open { 2 } else { 1 }];
        let writable: Vec<_> = self
            .stdin
            .as_ref()
            .filter(|_| !self.writes.is_empty())
            .map(AsFd::as_fd)
            .into_iter()
            .collect();
        commands.wait(readable, &writable, timeout)
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
        // This PID is the unreaped child whose setsid call succeeded before exec.
        let _ = drostje_platform::terminate_process_group(self.child.id());
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
        // This is the same unreaped owned process group.
        drostje_platform::kill_process_group(self.child.id())
            .with_context(|| format!("Kill owned {} process group", self.label))?;
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

    /// A reader replaying scripted reads; `None` stands for "would block".
    struct Script(VecDeque<Option<Vec<u8>>>);
    impl Read for Script {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                Some(Some(bytes)) => {
                    assert!(bytes.len() <= buffer.len(), "script reads fit one buffer");
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(None) => Err(std::io::ErrorKind::WouldBlock.into()),
                None => Ok(0),
            }
        }
    }

    fn script(reads: &[Option<&[u8]>]) -> Script {
        Script(reads.iter().map(|read| read.map(<[u8]>::to_vec)).collect())
    }

    fn frame(line: std::io::Result<Line<'_>>) -> Option<Vec<u8>> {
        match line.expect("scripted reads do not fail") {
            Line::Frame(bytes) => Some(bytes.to_vec()),
            _ => None,
        }
    }

    #[test]
    fn line_reader_splits_joins_and_reports_partial_lines() {
        let mut source = script(&[Some(b"a\nb"), Some(b"c\n"), None, Some(b"d"), None]);
        let mut reader = LineReader::new(64);
        assert_eq!(
            frame(reader.next(&mut source)).as_deref(),
            Some(&b"a\n"[..])
        );
        assert_eq!(
            frame(reader.next(&mut source)).as_deref(),
            Some(&b"bc\n"[..])
        );
        assert!(matches!(reader.next(&mut source), Ok(Line::Pending)));
        assert!(!reader.partial());
        assert!(matches!(reader.next(&mut source), Ok(Line::Pending)));
        assert!(reader.partial(), "the cut-off `d` is still buffered");
        assert!(matches!(reader.next(&mut source), Ok(Line::Eof)));
        assert!(reader.partial());

        let mut source = script(&[Some(b"abc"), Some(b"de\n")]);
        let mut reader = LineReader::new(5);
        assert!(matches!(reader.next(&mut source), Ok(Line::TooLong)));
        let mut source = script(&[Some(b"abcd\n")]);
        assert_eq!(
            frame(LineReader::new(5).next(&mut source)).as_deref(),
            Some(&b"abcd\n"[..]),
            "the limit counts the newline"
        );
    }

    #[test]
    fn line_reader_releases_a_buffer_grown_by_one_large_line() {
        let piece = vec![b'x'; 16_384];
        let mut reads: Vec<Option<&[u8]>> = vec![Some(&piece); 80];
        reads.push(Some(b"\nnext\n"));
        let mut source = script(&reads);
        let mut reader = LineReader::new(OUTPUT_FRAME);
        let large = frame(reader.next(&mut source)).expect("large line");
        assert_eq!(large.len(), 80 * 16_384 + 1);
        assert_eq!(
            frame(reader.next(&mut source)).as_deref(),
            Some(&b"next\n"[..])
        );
        assert_eq!(
            reader.line.capacity(),
            0,
            "the 1.25 MiB buffer was released"
        );
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
    fn idle_wait_sleeps_until_a_command_output_or_timeout() -> Result<()> {
        let (sender, receiver) = async_channel::bounded(4);
        let mut commands = Commands::new(receiver)?;
        let mut child =
            StdioChild::spawn(shell("read line; printf '{}\\n'; sleep 30"), "Idle", 1024)?;

        // Not yet registered for commands: one may be queued, so nothing sleeps.
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_secs(30))?;
        assert!(started.elapsed() < Duration::from_secs(5));

        assert!(commands.recv().is_err(), "no command yet");
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_millis(60))?;
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "an idle child does not wake it"
        );

        let send = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            sender.send_blocking(ConversationCommand::Interrupt)
        });
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_secs(30))?;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a queued command wakes it"
        );
        assert!(matches!(
            commands.recv(),
            Ok(ConversationCommand::Interrupt)
        ));
        send.join().expect("sender thread")?;

        assert!(commands.recv().is_err());
        child.queue(&json!({"go":true}))?;
        child.flush()?;
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_secs(30))?;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child output wakes it"
        );
        assert_eq!(next_frame(&mut child)?, json!({}));
        assert!(
            matches!(commands.recv(), Err(async_channel::TryRecvError::Closed)),
            "the sender thread dropped its end"
        );
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_millis(20))?;
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "a closed channel does not spin"
        );
        child.terminate()?;
        Ok(())
    }

    #[test]
    fn closed_stderr_stops_waking_the_loop() -> Result<()> {
        let (_sender, receiver) = async_channel::bounded(4);
        let mut commands = Commands::new(receiver)?;
        let mut child = StdioChild::spawn(shell("exec 2>&-; sleep 30"), "Quiet", 1024)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.stderr_open {
            assert!(child.read_stderr()?.is_none());
            ensure!(Instant::now() < deadline, "stderr never closed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(commands.recv().is_err());
        let started = Instant::now();
        child.wait(&mut commands, Duration::from_millis(60))?;
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "a hung-up stderr is not input"
        );
        child.terminate()?;
        Ok(())
    }

    #[test]
    fn sleeps_only_when_no_event_can_be_delivered() {
        let (output, _events) = async_channel::bounded(1);
        let mut events = VecDeque::new();
        assert_eq!(sleep_for(&events, &output, false), IDLE_WAIT);
        assert_eq!(sleep_for(&events, &output, true), BUSY_WAIT);
        events.push_back(1);
        assert_eq!(
            sleep_for(&events, &output, false),
            Duration::ZERO,
            "deliver first"
        );
        output.try_send(0).unwrap();
        assert_eq!(
            sleep_for(&events, &output, false),
            BUSY_WAIT,
            "a full display is polled on the short tick"
        );
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
