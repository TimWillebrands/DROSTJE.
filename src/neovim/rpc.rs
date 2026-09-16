use super::NvimConfig;
use anyhow::{Context, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use rmpv::Value;
use std::{
    collections::HashSet,
    io::{BufReader, BufWriter, Read, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(super) enum Incoming {
    Notification(String, Vec<Value>),
    Request {
        id: u64,
        method: String,
        args: Vec<Value>,
    },
    Response {
        id: u64,
        result: Response,
    },
    Failed(String),
}
type Response = std::result::Result<Value, String>;
enum Outgoing {
    Frame(Value),
    ServerRequest(u64),
    ServerReply(u64, Response),
    ClientResponse(u64),
}
enum CallFrame {
    Server { id: u64, reply: Option<Response> },
    Client { id: u64, done: bool },
}
struct Shared {
    pending: Mutex<HashSet<u64>>,
    failed: AtomicBool,
    incoming: Sender<Incoming>,
    stderr: Mutex<String>,
}
impl Shared {
    fn fail(&self, message: String) {
        if self.failed.swap(true, Ordering::SeqCst) {
            return;
        }
        let stderr = self.stderr.lock();
        let message = if stderr.is_empty() {
            message
        } else {
            format!("{message}\n{stderr}")
        };
        self.pending.lock().clear();
        let _ = self.incoming.try_send(Incoming::Failed(message));
        self.incoming.close();
    }
    fn deliver(&self, message: Incoming) -> bool {
        if let Err(error) = self.incoming.try_send(message) {
            self.fail(format!(
                "Neovim notification queue overflow/closure: {error}"
            ));
            false
        } else {
            true
        }
    }
}

pub(super) struct Rpc {
    child: Child,
    outgoing: Sender<Outgoing>,
    pub incoming: Receiver<Incoming>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
}
impl Rpc {
    pub fn spawn(config: &NvimConfig) -> Result<Self> {
        let mut command = Command::new(&config.executable);
        command
            .args(["--embed", "-u", "NONE", "-i", "NONE", "--noplugin", "-n"])
            .env_remove("NVIM")
            .env_remove("NVIM_LISTEN_ADDRESS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: The only pre-exec operation is async-signal-safe; we own this child's group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "Could not start isolated Neovim at {}. Configure Neovim in Settings.",
                config.executable.display()
            )
        })?;
        let stdin = child.stdin.take().context("Neovim stdin pipe missing")?;
        let stdout = child.stdout.take().context("Neovim stdout pipe missing")?;
        let mut stderr = child.stderr.take().context("Neovim stderr pipe missing")?;
        let (outgoing, writer_queue) = async_channel::bounded::<Outgoing>(1024);
        let (incoming_sender, incoming) = async_channel::bounded(2048);
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashSet::new()),
            failed: AtomicBool::new(false),
            incoming: incoming_sender,
            stderr: Mutex::new(String::new()),
        });
        // Establish kill/wait ownership before any fallible thread creation.
        let rpc = Self {
            child,
            outgoing: outgoing.clone(),
            incoming,
            shared: shared.clone(),
            next_id: AtomicU64::new(1),
        };
        let writer_state = shared.clone();
        thread::Builder::new()
            .name("drostje-nvim-writer".into())
            .spawn(move || {
                let mut writer = BufWriter::new(stdin);
                let mut stack: Vec<CallFrame> = Vec::new();
                let result = (|| -> Result<()> {
                    while let Ok(message) = writer_queue.recv_blocking() {
                        match message {
                            Outgoing::Frame(frame) => {
                                if !stack.is_empty()
                                    && let Some(values) = frame.as_array()
                                    && values.first().and_then(Value::as_u64) == Some(0)
                                    && let Some(id) = values.get(1).and_then(Value::as_u64)
                                {
                                    stack.push(CallFrame::Client { id, done: false });
                                }
                                rmpv::encode::write_value(&mut writer, &frame)?;
                                writer.flush()?;
                            }
                            Outgoing::ServerRequest(id) => {
                                stack.push(CallFrame::Server { id, reply: None })
                            }
                            Outgoing::ServerReply(id, result) => {
                                let reply = stack
                                    .iter_mut()
                                    .find_map(|entry| match entry {
                                        CallFrame::Server { id: pending, reply }
                                            if *pending == id =>
                                        {
                                            Some(reply)
                                        }
                                        _ => None,
                                    })
                                    .context("Reply for unknown Neovim server request")?;
                                *reply = Some(result);
                            }
                            Outgoing::ClientResponse(id) => {
                                if let Some(done) = stack.iter_mut().find_map(|entry| match entry {
                                    CallFrame::Client { id: pending, done } if *pending == id => {
                                        Some(done)
                                    }
                                    _ => None,
                                }) {
                                    *done = true;
                                }
                            }
                        }
                        // A reentrant client API call must finish before its enclosing
                        // Neovim request can receive a reply. Reverse server IDs alone
                        // are insufficient: sending both replies in one read batch can
                        // reach Neovim before its inner rpc call stack has unwound.
                        while matches!(
                            stack.last(),
                            Some(CallFrame::Server { reply: Some(_), .. })
                                | Some(CallFrame::Client { done: true, .. })
                        ) {
                            if let Some(CallFrame::Server {
                                id,
                                reply: Some(reply),
                            }) = stack.pop()
                            {
                                let (error, result) = match reply {
                                    Ok(result) => (Value::Nil, result),
                                    Err(error) => (Value::from(error), Value::Nil),
                                };
                                rmpv::encode::write_value(
                                    &mut writer,
                                    &Value::Array(vec![1.into(), id.into(), error, result]),
                                )?;
                                writer.flush()?;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    writer_state.fail(format!("Neovim RPC write failed: {error:#}"));
                }
            })?;
        let reader_state = shared.clone();
        let server_order = outgoing.clone();
        thread::Builder::new()
            .name("drostje-nvim-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let result = (|| -> Result<()> {
                    loop {
                        let value = rmpv::decode::read_value(&mut reader)
                            .context("Neovim RPC EOF or decode error")?;
                        let frame = value.as_array().context("RPC frame is not an array")?;
                        match frame.first().and_then(Value::as_u64) {
                            Some(1) if frame.len() == 4 => {
                                let id = frame[1].as_u64().context("Invalid RPC response ID")?;
                                server_order
                                    .send_blocking(Outgoing::ClientResponse(id))
                                    .map_err(|_| anyhow!("Neovim writer closed"))?;
                                let reply = reader_state.pending.lock().remove(&id);
                                if reply {
                                    let result = if frame[2].is_nil() {
                                        Ok(frame[3].clone())
                                    } else {
                                        Err(format!("Neovim RPC error: {}", frame[2]))
                                    };
                                    if !reader_state.deliver(Incoming::Response { id, result }) {
                                        break;
                                    }
                                }
                            }
                            Some(2) if frame.len() == 3 => {
                                let method = frame[1]
                                    .as_str()
                                    .context("Invalid notification method")?
                                    .to_owned();
                                let args = frame[2]
                                    .as_array()
                                    .context("Invalid notification arguments")?
                                    .clone();
                                if !reader_state.deliver(Incoming::Notification(method, args)) {
                                    break;
                                }
                            }
                            Some(0) if frame.len() == 4 => {
                                let id = frame[1].as_u64().context("Invalid server request ID")?;
                                let method = frame[2]
                                    .as_str()
                                    .context("Invalid server request method")?
                                    .to_owned();
                                let args = frame[3]
                                    .as_array()
                                    .context("Invalid server request arguments")?
                                    .clone();
                                server_order
                                    .send_blocking(Outgoing::ServerRequest(id))
                                    .map_err(|_| anyhow!("Neovim writer closed"))?;
                                if !reader_state.deliver(Incoming::Request { id, method, args }) {
                                    break;
                                }
                            }
                            _ => bail!("Malformed Neovim RPC frame"),
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    reader_state.fail(format!("{error:#}"));
                }
            })?;
        let stderr_state = shared.clone();
        thread::Builder::new()
            .name("drostje-nvim-stderr".into())
            .spawn(move || {
                let mut bytes = [0; 2048];
                while let Ok(count) = stderr.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    let mut retained = stderr_state.stderr.lock();
                    retained.push_str(&String::from_utf8_lossy(&bytes[..count]));
                    if retained.len() > 8192 {
                        let mut start = retained.len() - 8192;
                        while !retained.is_char_boundary(start) {
                            start += 1;
                        }
                        retained.drain(..start);
                    }
                }
            })?;
        Ok(rpc)
    }

    pub async fn call(&self, method: &str, args: Vec<Value>) -> Result<u64> {
        if self.shared.failed.load(Ordering::SeqCst) {
            bail!("Neovim transport disconnected; restart the editor");
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.pending.lock().insert(id);
        let frame = Value::Array(vec![0.into(), id.into(), method.into(), Value::Array(args)]);
        if self.outgoing.send(Outgoing::Frame(frame)).await.is_err() {
            self.shared.fail("Neovim writer closed".into());
            bail!("Neovim writer closed");
        }
        Ok(id)
    }
    pub fn cancel_call(&self, id: u64) {
        self.shared.pending.lock().remove(&id);
    }
    pub fn reply_later(
        &self,
        id: u64,
        receiver: Receiver<std::result::Result<super::ClipboardContent, String>>,
    ) {
        let writer = self.outgoing.clone();
        thread::spawn(move || {
            let reply = smol::block_on(smol::future::race(
                async {
                    receiver
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("Native clipboard callback closed".into()))
                },
                async {
                    smol::Timer::after(Duration::from_secs(2)).await;
                    Err("Native clipboard timed out after 2 seconds".into())
                },
            ));
            let result = reply.map(|content| {
                Value::Array(vec![
                    Value::Array(content.lines.into_iter().map(Value::from).collect()),
                    content.regtype.into(),
                ])
            });
            let _ = writer.send_blocking(Outgoing::ServerReply(id, result));
        });
    }
    pub fn reply_error(&self, id: u64, error: String) {
        let _ = self
            .outgoing
            .try_send(Outgoing::ServerReply(id, Err(error)));
    }
    pub fn is_failed(&self) -> bool {
        self.shared.failed.load(Ordering::SeqCst)
    }
    pub async fn stop(&mut self) -> Result<()> {
        self.outgoing.close();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            smol::Timer::after(Duration::from_millis(10)).await;
        }
        self.terminate();
        Ok(())
    }
    fn terminate(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        self.outgoing.close();
        self.terminate();
        self.shared.fail("Neovim process closed".into());
    }
}

pub(super) fn dictionary(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}
pub(super) fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    value
        .as_map()
        .and_then(|fields| {
            fields
                .iter()
                .find(|(key, _)| key.as_str() == Some(name))
                .map(|(_, value)| value)
        })
        .with_context(|| format!("Neovim response is missing {name}"))
}
pub(super) fn handle_id(value: &Value) -> Result<i64> {
    match value {
        Value::Ext(_, bytes) => rmpv::decode::read_value(&mut bytes.as_slice())?
            .as_i64()
            .context("Invalid Neovim EXT handle"),
        _ => value.as_i64().context("Invalid Neovim buffer handle"),
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::neovim::ClipboardContent;

    async fn next(rpc: &Rpc) -> Result<Incoming> {
        smol::future::race(
            async { rpc.incoming.recv().await.context("RPC reader closed") },
            async {
                smol::Timer::after(Duration::from_secs(3)).await;
                bail!("RPC fixture timed out")
            },
        )
        .await
    }
    async fn response(rpc: &Rpc, expected: u64) -> Result<Value> {
        loop {
            match next(rpc).await? {
                Incoming::Response { id, result } if id == expected => {
                    return result.map_err(anyhow::Error::msg);
                }
                Incoming::Failed(error) => {
                    bail!("{error}; Neovim stderr: {}", rpc.shared.stderr.lock())
                }
                Incoming::Notification(method, args) if method == "nvim_error_event" => {
                    bail!("Neovim protocol error: {args:?}")
                }
                Incoming::Notification(..) => {}
                _ => bail!("Unexpected RPC event while awaiting response"),
            }
        }
    }
    async fn request(rpc: &Rpc) -> Result<u64> {
        loop {
            match next(rpc).await? {
                Incoming::Request { id, method, .. } if method == "drostje_clipboard_get" => {
                    return Ok(id);
                }
                Incoming::Notification(method, args) if method == "nvim_error_event" => {
                    bail!("Neovim protocol error: {args:?}")
                }
                Incoming::Notification(..) => {}
                Incoming::Failed(error) => {
                    bail!("{error}; Neovim stderr: {}", rpc.shared.stderr.lock())
                }
                _ => bail!("Expected a nested clipboard request"),
            }
        }
    }
    #[test]
    #[ignore = "requires installed Neovim; exercises real reentrant MessagePack requests"]
    fn nested_clipboard_replies_unwind_in_reverse_order() -> Result<()> {
        smol::block_on(async {
            let executable = std::env::var_os("NVIM")
                .map(Into::into)
                .unwrap_or_else(|| "/opt/homebrew/bin/nvim".into());
            let mut rpc = Rpc::spawn(&NvimConfig::new(executable))?;
            let id = rpc.call("nvim_get_api_info", vec![]).await?;
            let channel = response(&rpc, id)
                .await
                .context("API-info handshake")?
                .as_array()
                .context("API metadata")?[0]
                .clone();
            let id = rpc
                .call(
                    "nvim_ui_attach",
                    vec![80.into(), 12.into(), dictionary([("rgb", true.into())])],
                )
                .await?;
            response(&rpc, id).await.context("UI attachment")?;
            let code = "return vim.rpcrequest(..., 'drostje_clipboard_get')";
            let outer_call = rpc
                .call(
                    "nvim_exec_lua",
                    vec![code.into(), Value::Array(vec![channel.clone()])],
                )
                .await?;
            let outer_request = request(&rpc)
                .await
                .context("Receiving outer clipboard request")?;
            // A nested host API request is serviced while Neovim awaits its outer
            // clipboard reply. Timers do not run reentrantly inside that wait.
            let inner_call = rpc
                .call(
                    "nvim_exec_lua",
                    vec![code.into(), Value::Array(vec![channel])],
                )
                .await?;
            let inner_request = request(&rpc)
                .await
                .context("Receiving inner clipboard request")?;
            let (outer, outer_result) = async_channel::bounded(1);
            let (inner, inner_result) = async_channel::bounded(1);
            rpc.reply_later(outer_request, outer_result);
            rpc.reply_later(inner_request, inner_result);
            outer
                .send(Ok(ClipboardContent {
                    lines: vec!["outer".into()],
                    regtype: "V".into(),
                }))
                .await?;
            smol::Timer::after(Duration::from_millis(20)).await;
            inner
                .send(Ok(ClipboardContent {
                    lines: vec!["inner".into()],
                    regtype: "\u{16}4".into(),
                }))
                .await?;
            assert_eq!(
                response(&rpc, inner_call)
                    .await
                    .context("Inner call reply")?,
                Value::Array(vec![Value::Array(vec!["inner".into()]), "\u{16}4".into()])
            );
            assert_eq!(
                response(&rpc, outer_call)
                    .await
                    .context("Outer call reply")?,
                Value::Array(vec![Value::Array(vec!["outer".into()]), "V".into()])
            );
            let _ = rpc.call("nvim_command", vec!["qa!".into()]).await;
            rpc.stop().await
        })
    }
}
