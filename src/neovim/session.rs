use super::{
    ClipboardContent, DraftKey, DraftRevision, DraftSnapshot, NvimCommand, NvimConfig, NvimEditor,
    NvimEvent, NvimTheme,
    grid::GridReducer,
    rpc::{self, Incoming, Rpc},
};
use crate::model::PaneDirection;
use anyhow::{Context, Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use rmpv::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

const MARKER_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn open(config: NvimConfig) -> Result<NvimEditor> {
    let (commands, command_rx) = async_channel::bounded(512);
    let (events, event_rx) = async_channel::bounded(128);
    let (ready, initialized) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("drost-nvim-session".into())
        .spawn(move || {
            smol::block_on(async move {
                let mut session = Session::new(config, events);
                match session.initialize().await {
                    Ok(()) => {
                        session.events.emit(NvimEvent::Ready);
                        let _ = ready.try_send(Ok(()));
                        session.run(command_rx).await;
                    }
                    Err(error) => {
                        let _ = ready.try_send(Err(error));
                    }
                }
            });
        })?;
    initialized.recv().await??;
    Ok(NvimEditor {
        commands,
        events: event_rx,
    })
}

struct Draft {
    handle: Value,
    snapshot: DraftSnapshot,
    lines: Vec<String>,
    pending_lines: Option<Vec<String>>,
    insert: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Drain,
    Snapshot,
}
struct Marker {
    kind: MarkerKind,
    sequence: u64,
    response: Option<MarkerResponse>,
}
struct MarkerResponse {
    mode: String,
    snapshot: Option<DraftSnapshot>,
}

/// Coalesce display and durable-shadow updates, never blocking the reader or
/// command actor behind a slow window. Clipboard requests get errors, not stale
/// clipboard data, if the native callback cannot be delivered promptly.
struct Events {
    sender: Sender<NvimEvent>,
    grid: Option<Arc<super::grid::GridSnapshot>>,
    drafts: HashMap<DraftKey, DraftSnapshot>,
    activated: Option<DraftSnapshot>,
    control: VecDeque<NvimEvent>,
    ready: bool,
    overflowed: bool,
}
impl Events {
    fn new(sender: Sender<NvimEvent>) -> Self {
        Self {
            sender,
            grid: None,
            drafts: HashMap::new(),
            activated: None,
            control: VecDeque::new(),
            ready: false,
            overflowed: false,
        }
    }
    fn emit(&mut self, event: NvimEvent) {
        if self.overflowed {
            if let NvimEvent::ClipboardGet { reply } = event {
                let _ = reply.try_send(Err("Native editor event delivery failed".into()));
            }
            return;
        }
        match event {
            NvimEvent::Grid(grid) => self.grid = Some(grid),
            NvimEvent::DraftChanged(draft) => {
                self.drafts.insert(draft.key.clone(), draft);
            }
            NvimEvent::DraftActivated(draft) => self.activated = Some(draft),
            NvimEvent::Ready => self.ready = true,
            NvimEvent::ClipboardGet { reply } => {
                // A clipboard read must never overtake a pending native write.
                self.flush();
                if let Err(error) = self.sender.try_send(NvimEvent::ClipboardGet { reply }) {
                    if let NvimEvent::ClipboardGet { reply } = error.into_inner() {
                        let _ = reply
                            .try_send(Err("Native clipboard callback queue unavailable".into()));
                    }
                }
                return;
            }
            event => {
                // Grid/draft updates coalesce above. Ordered acknowledgements
                // cannot be silently dropped: fail closed on sustained UI lag.
                if self.control.len() == 128 {
                    self.overflowed = true;
                    self.ready = false;
                    self.control.clear();
                    self.control.push_back(NvimEvent::Failed("Native editor event queue overflowed. Restart Editor; retained draft text was not submitted.".into()));
                } else {
                    self.control.push_back(event);
                }
            }
        }
        self.flush();
    }
    fn flush(&mut self) {
        while let Some(event) = self.control.pop_front() {
            if let Err(error) = self.sender.try_send(event) {
                self.control.push_front(error.into_inner());
                return;
            }
        }
        if let Some(draft) = self.activated.take() {
            if let Err(error) = self.sender.try_send(NvimEvent::DraftActivated(draft)) {
                if let NvimEvent::DraftActivated(draft) = error.into_inner() {
                    self.activated = Some(draft);
                }
                return;
            }
        }
        if let Some(grid) = self.grid.take() {
            if let Err(error) = self.sender.try_send(NvimEvent::Grid(grid)) {
                if let NvimEvent::Grid(grid) = error.into_inner() {
                    self.grid = Some(grid);
                }
                return;
            }
        }
        let keys: Vec<_> = self.drafts.keys().cloned().collect();
        for key in keys {
            let draft = self.drafts.remove(&key).unwrap();
            if let Err(error) = self.sender.try_send(NvimEvent::DraftChanged(draft)) {
                if let NvimEvent::DraftChanged(draft) = error.into_inner() {
                    self.drafts.insert(key, draft);
                }
                return;
            }
        }
        if self.ready && self.sender.try_send(NvimEvent::Ready).is_ok() {
            self.ready = false;
        }
    }
}

struct Session {
    config: NvimConfig,
    theme: Option<NvimTheme>,
    rpc: Option<Rpc>,
    generation: Uuid,
    channel: u64,
    methods: HashSet<String>,
    grid: GridReducer,
    events: Events,
    drafts: HashMap<DraftKey, Draft>,
    active: Option<DraftKey>,
    sequence: u64,
    marker: Option<Marker>,
    vim_entered: bool,
    composition: bool,
    frozen: bool,
    failed: Option<String>,
}
impl Session {
    fn new(config: NvimConfig, events: Sender<NvimEvent>) -> Self {
        let generation = Uuid::new_v4();
        Self {
            theme: config.initial_theme,
            config,
            rpc: None,
            generation,
            channel: 0,
            methods: HashSet::new(),
            grid: GridReducer::new(generation),
            events: Events::new(events),
            drafts: HashMap::new(),
            active: None,
            sequence: 0,
            marker: None,
            vim_entered: false,
            composition: false,
            frozen: false,
            failed: None,
        }
    }
    fn rpc(&self) -> Result<&Rpc> {
        self.rpc.as_ref().context("Neovim is not running")
    }
    fn available(&self) -> Result<()> {
        if let Some(error) = &self.failed {
            bail!("Editor disconnected: {error}. Restart Editor to continue.");
        }
        ensure!(
            !self.frozen,
            "Editor input is frozen for durable persistence"
        );
        ensure!(
            !self.events.overflowed,
            "Native event delivery failed; restart the editor"
        );
        Ok(())
    }
    fn fail(&mut self, error: String) {
        if self.failed.is_none() {
            self.failed = Some(error.clone());
            self.marker = None;
            self.events.ready = false;
            // No raw input is retained in a recovery snapshot.
            self.events.emit(NvimEvent::Failed(error));
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        self.rpc = Some(Rpc::spawn(&self.config)?);
        let info = self.call("nvim_get_api_info", vec![]).await?;
        let info = info
            .as_array()
            .context("nvim_get_api_info must return [channel,metadata]")?;
        ensure!(info.len() >= 2, "Neovim API metadata is incomplete");
        self.channel = info[0].as_u64().context("Invalid Neovim channel ID")?;
        let functions = rpc::field(&info[1], "functions")?
            .as_array()
            .context("Neovim API functions missing")?;
        self.methods = functions
            .iter()
            .filter_map(|function| {
                rpc::field(function, "name")
                    .ok()?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        for required in [
            "nvim_get_api_info",
            "nvim_set_client_info",
            "nvim_exec_lua",
            "nvim_ui_attach",
            "nvim_ui_try_resize",
            "nvim_input",
            "nvim_input_mouse",
            "nvim_paste",
            "nvim_create_buf",
            "nvim_buf_attach",
            "nvim_buf_set_lines",
            "nvim_buf_set_text",
            "nvim_buf_get_lines",
            "nvim_buf_get_changedtick",
            "nvim_set_current_buf",
            "nvim_win_set_cursor",
            "nvim_get_mode",
            "nvim_command",
        ] {
            ensure!(
                self.methods.contains(required),
                "Installed Neovim lacks required API {required}; choose a supported Neovim in Settings"
            );
        }
        let options = rpc::field(&info[1], "ui_options")?
            .as_array()
            .context("Neovim UI options missing")?;
        for extension in ["ext_linegrid", "ext_cmdline"] {
            ensure!(
                options
                    .iter()
                    .any(|option| option.as_str() == Some(extension)),
                "Neovim does not advertise {extension}"
            );
        }
        self.call(
            "nvim_set_client_info",
            vec![
                "Drost".into(),
                rpc::dictionary([("major", 0.into()), ("minor", 1.into())]),
                "ui".into(),
                Value::Map(vec![]),
                Value::Map(vec![]),
            ],
        )
        .await?;
        self.lua("local c,g=...; if vim.v.vim_did_enter == 1 then vim.rpcnotify(c,'drost_vim_enter',g) else vim.api.nvim_create_autocmd('VimEnter',{once=true,callback=function() vim.rpcnotify(c,'drost_vim_enter',g) end}) end", vec![self.channel.into(), self.generation.to_string().into()]).await?;
        self.call(
            "nvim_ui_attach",
            vec![
                self.config.cols.max(1).into(),
                self.config.rows.max(2).into(),
                rpc::dictionary([
                    ("rgb", true.into()),
                    ("ext_linegrid", true.into()),
                    ("ext_cmdline", true.into()),
                ]),
            ],
        )
        .await?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        while !self.vim_entered {
            self.pump_until(deadline).await?;
        }
        self.lua(
            include_str!("bridge.lua"),
            vec![self.channel.into(), self.generation.to_string().into()],
        )
        .await?;
        if let Some(theme) = self.theme {
            self.apply_theme(theme).await?;
        }
        self.call("nvim_command", vec!["redraw!".into()]).await?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        while self.grid.snapshot().serial == 0 {
            self.pump_until(deadline).await?;
        }
        Ok(())
    }
    async fn lua(&mut self, code: &str, args: Vec<Value>) -> Result<Value> {
        self.call("nvim_exec_lua", vec![code.into(), Value::Array(args)])
            .await
    }
    async fn apply_theme(&mut self, theme: NvimTheme) -> Result<()> {
        self.lua(
            "local theme=...; drost.apply_theme(theme)",
            vec![rpc::dictionary([
                ("dark", theme.dark.into()),
                ("background", theme.background.into()),
                ("foreground", theme.foreground.into()),
                ("popover", theme.popover.into()),
                ("popover_foreground", theme.popover_foreground.into()),
                ("selection", theme.selection.into()),
                ("caret", theme.caret.into()),
                ("primary", theme.primary.into()),
                ("primary_foreground", theme.primary_foreground.into()),
            ])],
        )
        .await?;
        Ok(())
    }
    async fn call(&mut self, method: &str, args: Vec<Value>) -> Result<Value> {
        self.call_until(method, args, Instant::now() + RPC_TIMEOUT)
            .await
    }
    async fn call_until(
        &mut self,
        method: &str,
        args: Vec<Value>,
        deadline: Instant,
    ) -> Result<Value> {
        let id = self.rpc()?.call(method, args).await?;
        let incoming = self.rpc()?.incoming.clone();
        loop {
            let message = smol::future::race(
                async {
                    incoming
                        .recv()
                        .await
                        .map_err(|_| anyhow!("Neovim disconnected while awaiting {method}"))
                },
                async {
                    smol::Timer::at(deadline).await;
                    Err(anyhow!(
                        "Neovim {method} timed out; restart the editor before continuing"
                    ))
                },
            )
            .await;
            match message {
                // Responses and notifications share one FIFO, preserving the
                // exact wire order of line events and authoritative reads.
                Ok(Incoming::Response {
                    id: response_id,
                    result,
                }) if response_id == id => {
                    return result.map_err(|error| anyhow!("{method}: {error}"));
                }
                Ok(message) => self.incoming(message)?,
                Err(error) => {
                    self.rpc()?.cancel_call(id);
                    self.fail(error.to_string());
                    return Err(error);
                }
            }
        }
    }
    async fn pump_until(&mut self, deadline: Instant) -> Result<()> {
        let incoming = self.rpc()?.incoming.clone();
        let message = smol::future::race(
            async { incoming.recv().await.map_err(|_| anyhow!("Neovim transport closed")) },
            async { smol::Timer::at(deadline).await; Err(anyhow!("Neovim semantic marker timed out after 2 seconds. Cancel or Restart Editor; draft was not submitted.")) },
        ).await;
        match message {
            Ok(message) => self.incoming(message),
            Err(error) => {
                self.fail(error.to_string());
                Err(error)
            }
        }
    }
    fn incoming(&mut self, incoming: Incoming) -> Result<()> {
        let result = self.process_incoming(incoming).and_then(|()| {
            ensure!(
                !self.events.overflowed,
                "Native editor event queue overflowed; restart required"
            );
            Ok(())
        });
        if let Err(error) = &result {
            self.fail(format!("{error:#}"));
        }
        result
    }
    fn process_incoming(&mut self, incoming: Incoming) -> Result<()> {
        match incoming {
            Incoming::Response { .. } => {}
            Incoming::Failed(error) => bail!(error),
            Incoming::Request { id, method, args } => {
                if method == "drost_clipboard_get" && args.is_empty() {
                    let (reply, result) = async_channel::bounded(1);
                    self.rpc()?.reply_later(id, result);
                    self.events.emit(NvimEvent::ClipboardGet { reply });
                } else {
                    self.rpc()?
                        .reply_error(id, format!("Unsupported Neovim host request: {method}"));
                }
            }
            Incoming::Notification(method, args) => match method.as_str() {
                "redraw" => {
                    for grid in self.grid.apply(&args)? {
                        self.events.emit(NvimEvent::Grid(grid));
                    }
                }
                "nvim_buf_lines_event" => self.lines_event(&args)?,
                "nvim_buf_changedtick_event" => {
                    ensure!(args.len() >= 2, "Malformed changedtick notification");
                    if let Some(key) = self.key_for(&args[0])? {
                        let draft = self.drafts.get_mut(&key).unwrap();
                        draft.snapshot.changedtick =
                            args[1].as_u64().context("Invalid changedtick")?;
                        self.events
                            .emit(NvimEvent::DraftChanged(draft.snapshot.clone()));
                    }
                }
                "nvim_buf_detach_event" => {
                    if let Some(buffer) = args.first() {
                        if self.key_for(buffer)?.is_some() {
                            bail!(
                                "Neovim detached a Drost scratch buffer; restart to restore its retained draft"
                            );
                        }
                    }
                }
                "drost_vim_enter"
                | "drost_drain"
                | "drost_snapshot"
                | "drost_cursor"
                | "drost_focus_pane"
                | "drost_clipboard_set" => {
                    if args
                        .first()
                        .and_then(Value::as_str)
                        .and_then(|value| Uuid::parse_str(value).ok())
                        != Some(self.generation)
                    {
                        return Ok(());
                    }
                    match method.as_str() {
                        "drost_vim_enter" => self.vim_entered = true,
                        "drost_drain" => {
                            ensure!(args.len() >= 3, "Malformed input-drain callback");
                            let sequence = args[1].as_u64().context("Invalid drain sequence")?;
                            let mode = args[2].as_str().context("Invalid drain mode")?.to_owned();
                            if let Some(marker) = &mut self.marker {
                                if marker.kind == MarkerKind::Drain && marker.sequence == sequence {
                                    marker.response = Some(MarkerResponse {
                                        mode,
                                        snapshot: None,
                                    });
                                }
                            }
                        }
                        "drost_snapshot" => {
                            ensure!(args.len() >= 7, "Malformed snapshot callback");
                            let sequence = args[1].as_u64().context("Invalid snapshot sequence")?;
                            if !self.marker.as_ref().is_some_and(|marker| {
                                marker.kind == MarkerKind::Snapshot && marker.sequence == sequence
                            }) {
                                return Ok(());
                            }
                            let key = self
                                .key_for(&args[2])?
                                .context("Snapshot refers to unknown scratch buffer")?;
                            let tick = args[3].as_u64().context("Invalid snapshot changedtick")?;
                            let lines = strings(&args[4])?;
                            let cursor = parse_cursor(&args[5])?;
                            let mode = args[6]
                                .as_str()
                                .context("Invalid snapshot mode")?
                                .to_owned();
                            let snapshot = DraftSnapshot {
                                key: key.clone(),
                                generation: self.generation,
                                changedtick: tick,
                                text: lines.join("\n"),
                                cursor,
                            };
                            let draft = self.drafts.get_mut(&key).unwrap();
                            draft.snapshot = snapshot.clone();
                            draft.lines = lines;
                            draft.pending_lines = None;
                            self.events.emit(NvimEvent::DraftChanged(snapshot.clone()));
                            self.marker.as_mut().unwrap().response = Some(MarkerResponse {
                                mode,
                                snapshot: Some(snapshot),
                            });
                        }
                        "drost_cursor" => {
                            ensure!(args.len() >= 4, "Malformed cursor callback");
                            if let Some(key) = self.key_for(&args[1])? {
                                let draft = self.drafts.get_mut(&key).unwrap();
                                let tick =
                                    args[2].as_u64().context("Invalid cursor changedtick")?;
                                if tick == draft.snapshot.changedtick {
                                    draft.snapshot.cursor = parse_cursor(&args[3])?;
                                    self.events
                                        .emit(NvimEvent::DraftChanged(draft.snapshot.clone()));
                                }
                            }
                        }
                        "drost_focus_pane" => {
                            ensure!(args.len() == 3, "Malformed pane-focus callback");
                            let direction = match args[2].as_str() {
                                Some("left") => PaneDirection::Left,
                                Some("down") => PaneDirection::Down,
                                Some("up") => PaneDirection::Up,
                                Some("right") => PaneDirection::Right,
                                _ => bail!("Invalid pane-focus direction"),
                            };
                            if let Some(key) = self.key_for(&args[1])? {
                                if self.active.as_ref() == Some(&key)
                                    && !self.composition
                                    && !self.frozen
                                {
                                    self.events.emit(NvimEvent::FocusPane {
                                        key,
                                        generation: self.generation,
                                        direction,
                                    });
                                }
                            }
                        }
                        "drost_clipboard_set" => {
                            ensure!(args.len() >= 3, "Malformed clipboard notification");
                            let regtype = args[2]
                                .as_str()
                                .context("Invalid clipboard register type")?
                                .to_owned();
                            ensure!(
                                valid_regtype(&regtype),
                                "Unsupported clipboard register type"
                            );
                            self.events.emit(NvimEvent::ClipboardSet(ClipboardContent {
                                lines: strings(&args[1])?,
                                regtype,
                            }));
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
        }
        Ok(())
    }
    fn key_for(&self, handle: &Value) -> Result<Option<DraftKey>> {
        let id = rpc::handle_id(handle)?;
        Ok(self
            .drafts
            .iter()
            .find(|(_, draft)| {
                draft.snapshot.generation == self.generation
                    && rpc::handle_id(&draft.handle).ok() == Some(id)
            })
            .map(|(key, _)| key.clone()))
    }
    fn lines_event(&mut self, args: &[Value]) -> Result<()> {
        ensure!(args.len() >= 6, "Malformed buffer line notification");
        if args[1].is_nil() {
            return Ok(());
        } // inccommand preview is not a durable edit.
        let Some(key) = self.key_for(&args[0])? else {
            return Ok(());
        };
        let changedtick = args[1].as_u64().context("Invalid buffer changedtick")?;
        let first = args[2].as_u64().context("Invalid first changed line")? as usize;
        let last = args[3].as_i64().context("Invalid last changed line")?;
        let replacement = strings(&args[4])?;
        let more = args[5].as_bool().context("Invalid buffer chunk flag")?;
        let draft = self.drafts.get_mut(&key).unwrap();
        let lines = draft
            .pending_lines
            .get_or_insert_with(|| draft.lines.clone());
        let last = if last == -1 {
            lines.len()
        } else {
            usize::try_from(last).context("Negative changed line")?
        };
        ensure!(
            first <= last && last <= lines.len(),
            "Neovim line update range is outside the retained draft"
        );
        lines.splice(first..last, replacement);
        if !more {
            draft.lines = draft.pending_lines.take().unwrap();
            if draft.lines.is_empty() {
                draft.lines.push(String::new());
            }
            draft.snapshot.text = draft.lines.join("\n");
            draft.snapshot.changedtick = changedtick;
            self.events
                .emit(NvimEvent::DraftChanged(draft.snapshot.clone()));
        }
        Ok(())
    }

    async fn raw_input(&mut self, keys: &str) -> Result<()> {
        let deadline = Instant::now() + MARKER_TIMEOUT;
        let mut offset = 0;
        while offset < keys.len() {
            if Instant::now() >= deadline {
                let error = "Neovim input queue did not accept all bytes within 2 seconds; restart required".to_owned();
                self.fail(error.clone());
                bail!(error);
            }
            let value = self
                .call_until("nvim_input", vec![keys[offset..].into()], deadline)
                .await?;
            let Some(accepted) = value.as_u64().and_then(|count| usize::try_from(count).ok())
            else {
                let error = "nvim_input returned an invalid accepted byte count".to_owned();
                self.fail(error.clone());
                bail!(error);
            };
            if accepted > keys.len() - offset || !keys.is_char_boundary(offset + accepted) {
                let error =
                    "Neovim accepted byte count split a UTF-8 codepoint or exceeded the input"
                        .to_owned();
                self.fail(error.clone());
                bail!(error);
            }
            offset += accepted;
            if accepted == 0 {
                smol::Timer::after(Duration::from_millis(1)).await;
            }
        }
        Ok(())
    }
    async fn marker(&mut self, kind: MarkerKind) -> Result<MarkerResponse> {
        ensure!(
            self.marker.is_none(),
            "Only one Neovim input marker may be outstanding"
        );
        let sequence = self
            .sequence
            .checked_add(1)
            .context("Neovim marker sequence exhausted")?;
        self.marker = Some(Marker {
            kind,
            sequence,
            response: None,
        });
        let deadline = Instant::now() + MARKER_TIMEOUT;
        let keys = if kind == MarkerKind::Snapshot {
            "<Esc><F24>"
        } else {
            "<F23>"
        };
        if let Err(error) = self.raw_input(keys).await {
            self.fail(error.to_string());
            return Err(error);
        }
        loop {
            if let Some(response) = self
                .marker
                .as_mut()
                .and_then(|marker| marker.response.take())
            {
                self.sequence = sequence;
                self.marker = None;
                return Ok(response);
            }
            self.pump_until(deadline).await?;
        }
    }
    async fn drain(&mut self) -> Result<String> {
        Ok(self.marker(MarkerKind::Drain).await?.mode)
    }
    async fn snapshot_active(&mut self) -> Result<DraftSnapshot> {
        ensure!(
            !self.composition,
            "Complete or cancel native composition before submitting or switching drafts"
        );
        let key = self.active.clone().context("No active draft")?;
        let mode = self.drain().await?;
        ensure!(
            !mode.starts_with('c') && !mode.starts_with('r'),
            "Complete or cancel the Neovim command line before submitting or switching drafts"
        );
        self.drafts.get_mut(&key).unwrap().insert = mode.starts_with('i');
        let snapshot = self
            .marker(MarkerKind::Snapshot)
            .await?
            .snapshot
            .context("Snapshot marker had no draft")?;
        ensure!(
            snapshot.key == key && snapshot.generation == self.generation,
            "Snapshot draft/generation changed during input barrier"
        );
        Ok(snapshot)
    }
    async fn create_draft(
        &mut self,
        key: DraftKey,
        text: String,
        cursor: (usize, usize),
        insert: bool,
    ) -> Result<()> {
        let handle = self
            .call("nvim_create_buf", vec![false.into(), true.into()])
            .await?;
        let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        self.drafts.insert(
            key.clone(),
            Draft {
                handle: handle.clone(),
                snapshot: DraftSnapshot {
                    key: key.clone(),
                    generation: self.generation,
                    changedtick: 0,
                    text,
                    cursor,
                },
                lines: vec![String::new()],
                pending_lines: None,
                insert,
            },
        );
        self.lua("local b=...; vim.bo[b].buftype='nofile'; vim.bo[b].bufhidden='hide'; vim.bo[b].swapfile=false; vim.bo[b].undofile=false; drost.attach(b)", vec![handle.clone()]).await?;
        self.call(
            "nvim_buf_attach",
            vec![handle.clone(), true.into(), Value::Map(vec![])],
        )
        .await?;
        self.call(
            "nvim_buf_set_lines",
            vec![
                handle,
                0.into(),
                (-1).into(),
                true.into(),
                Value::Array(lines.into_iter().map(Value::from).collect()),
            ],
        )
        .await?;
        self.refresh_draft(&key).await?;
        Ok(())
    }
    async fn refresh_draft(&mut self, key: &DraftKey) -> Result<DraftSnapshot> {
        let draft = self.drafts.get(key).context("Draft is not open")?;
        let handle = draft.handle.clone();
        let cursor = draft.snapshot.cursor;
        let value = self.lua("local b=...; return {vim.api.nvim_buf_get_changedtick(b),vim.api.nvim_buf_get_lines(b,0,-1,true),vim.api.nvim_get_current_buf()==b and vim.api.nvim_win_get_cursor(0) or false}", vec![handle]).await?;
        let value = value.as_array().context("Malformed draft refresh")?;
        ensure!(value.len() >= 2, "Incomplete draft refresh");
        let tick = value[0].as_u64().context("Invalid draft changedtick")?;
        let lines = strings(&value[1])?;
        let cursor = if let Some(value) = value.get(2).filter(|value| value.is_array()) {
            parse_cursor(value)?
        } else {
            cursor
        };
        let snapshot = DraftSnapshot {
            key: key.clone(),
            generation: self.generation,
            changedtick: tick,
            text: lines.join("\n"),
            cursor,
        };
        let draft = self.drafts.get_mut(key).unwrap();
        draft.snapshot = snapshot.clone();
        draft.lines = lines;
        draft.pending_lines = None;
        self.events.emit(NvimEvent::DraftChanged(snapshot.clone()));
        Ok(snapshot)
    }
    async fn focus_draft(&mut self, key: DraftKey) -> Result<()> {
        ensure!(self.drafts.contains_key(&key), "Draft is not open");
        if self.active.as_ref() == Some(&key) {
            return Ok(());
        }
        if self.active.is_some() {
            self.snapshot_active().await?;
        }
        self.activate(key).await
    }
    async fn activate(&mut self, key: DraftKey) -> Result<()> {
        let draft = self.drafts.get(&key).context("Draft is not open")?;
        let handle = draft.handle.clone();
        let row = draft
            .snapshot
            .cursor
            .0
            .min(draft.lines.len().saturating_sub(1));
        let line = draft.lines.get(row).map(String::as_str).unwrap_or("");
        let mut col = draft.snapshot.cursor.1.min(line.len());
        while !line.is_char_boundary(col) {
            col -= 1;
        }
        let insert = draft.insert || draft.snapshot.text.is_empty();
        self.call("nvim_set_current_buf", vec![handle]).await?;
        self.active = Some(key.clone());
        self.call(
            "nvim_win_set_cursor",
            vec![
                0.into(),
                Value::Array(vec![(row as u64 + 1).into(), (col as u64).into()]),
            ],
        )
        .await?;
        if insert {
            self.call("nvim_command", vec!["startinsert".into()])
                .await?;
        }
        let mut snapshot = self.drafts[&key].snapshot.clone();
        snapshot.cursor = (row, col);
        self.drafts.get_mut(&key).unwrap().snapshot.cursor = (row, col);
        self.events.emit(NvimEvent::DraftActivated(snapshot));
        Ok(())
    }
    async fn snapshot(&mut self, key: DraftKey) -> Result<DraftSnapshot> {
        ensure!(
            !self.composition,
            "Complete or cancel native composition before taking a snapshot"
        );
        if self.active.as_ref() == Some(&key) {
            self.snapshot_active().await
        } else {
            self.refresh_draft(&key).await
        }
    }
    async fn paste(&mut self, text: String) -> Result<()> {
        ensure!(
            !self.composition,
            "Complete or cancel native composition before pasting"
        );
        let mode = self.drain().await?;
        ensure!(
            !mode.starts_with("no"),
            "Complete or cancel the unfinished Neovim operator before pasting"
        );
        let accepted = self
            .call("nvim_paste", vec![text.into(), false.into(), (-1).into()])
            .await?;
        ensure!(
            accepted.as_bool() == Some(true),
            "Neovim cancelled the paste"
        );
        Ok(())
    }
    async fn replace(
        &mut self,
        key: DraftKey,
        revision: DraftRevision,
        range: Range<usize>,
        text: String,
    ) -> Result<()> {
        ensure!(
            self.active.as_ref() == Some(&key),
            "Composition belongs to a different draft; recovered text was not inserted"
        );
        let mode = self.drain().await?;
        ensure!(
            !mode.starts_with("no") && !mode.starts_with('c'),
            "Complete or cancel the current Neovim operator/command before committing composition"
        );
        let current = self.refresh_draft(&key).await?;
        ensure!(
            current.revision() == revision,
            "Draft changed while composing; composition retained for recovery"
        );
        let bytes = utf16_range(&current.text, range)?;
        let start = byte_position(&current.text, bytes.start);
        let end = byte_position(&current.text, bytes.end);
        let handle = self.drafts[&key].handle.clone();
        if bytes.is_empty() {
            let result = self.lua("local b,tick,row,col,text=...; local p=vim.api.nvim_win_get_cursor(0); if vim.api.nvim_get_current_buf()~=b or vim.api.nvim_buf_get_changedtick(b)~=tick or p[1]~=row+1 or p[2]~=col then return false end; return vim.api.nvim_paste(text,false,-1)", vec![handle, revision.changedtick.into(), (start.0 as u64).into(), (start.1 as u64).into(), text.into()]).await?;
            ensure!(
                result.as_bool() == Some(true),
                "Neovim cursor/revision changed or composition paste was cancelled; text retained for recovery"
            );
        } else {
            let inserted_lines = text.bytes().filter(|byte| *byte == b'\n').count();
            let col = if inserted_lines == 0 {
                start.1 + text.len()
            } else {
                text.rsplit('\n').next().unwrap_or("").len()
            };
            let lines: Vec<_> = text.split('\n').map(Value::from).collect();
            let result = self.lua("local b,tick,sr,sc,er,ec,lines,row,col=...; if vim.api.nvim_get_current_buf()~=b or vim.api.nvim_buf_get_changedtick(b)~=tick then return false end; vim.api.nvim_buf_set_text(b,sr,sc,er,ec,lines); vim.api.nvim_win_set_cursor(0,{row,col}); return true", vec![handle, revision.changedtick.into(), (start.0 as u64).into(), (start.1 as u64).into(), (end.0 as u64).into(), (end.1 as u64).into(), Value::Array(lines), (start.0 as u64 + inserted_lines as u64 + 1).into(), (col as u64).into()]).await?;
            ensure!(
                result.as_bool() == Some(true),
                "Draft changed during native replacement; text retained for recovery"
            );
        }
        self.events.emit(NvimEvent::CompositionCommitted);
        Ok(())
    }
    async fn replace_cmdline(
        &mut self,
        expected: String,
        range: Range<usize>,
        text: String,
    ) -> Result<()> {
        let mode = self.drain().await?;
        ensure!(
            mode.starts_with('c'),
            "Command-line focus was lost; composition retained for recovery"
        );
        let bytes = utf16_range(&expected, range)?;
        let mut replacement = expected.clone();
        replacement.replace_range(bytes.clone(), &text);
        let position = bytes.start + text.len() + 1;
        let result = self.lua("local expected,text,pos=...; if vim.fn.mode(1):sub(1,1)~='c' or vim.fn.getcmdline()~=expected then return -1 end; return vim.fn.setcmdline(text,pos)", vec![expected.into(), replacement.into(), (position as u64).into()]).await?;
        ensure!(
            result.as_i64() == Some(0),
            "Command-line focus/text changed; composition retained for recovery"
        );
        self.events.emit(NvimEvent::CompositionCommitted);
        Ok(())
    }
    async fn replace_if_revision(
        &mut self,
        key: DraftKey,
        revision: DraftRevision,
        text: String,
    ) -> Result<bool> {
        if revision.generation != self.generation {
            return Ok(false);
        }
        let current = self.snapshot(key.clone()).await?;
        if current.revision() != revision {
            return Ok(false);
        }
        let handle = self.drafts[&key].handle.clone();
        // The check and write occur in one Lua invocation as well as at the host
        // barrier, so timer/autocmd changes cannot clear a newer draft.
        let result = self.lua("local b,tick,lines=...; if vim.api.nvim_buf_get_changedtick(b)~=tick then return false end; vim.api.nvim_buf_set_lines(b,0,-1,true,lines); return true", vec![handle, revision.changedtick.into(), Value::Array(text.split('\n').map(Value::from).collect())]).await?;
        let replaced = result
            .as_bool()
            .context("Invalid conditional replacement result")?;
        if replaced {
            self.refresh_draft(&key).await?;
            if self.active.as_ref() == Some(&key) {
                self.call("nvim_command", vec!["startinsert".into()])
                    .await?;
            }
        }
        Ok(replaced)
    }
    async fn persist_barrier(&mut self) -> Result<Vec<DraftSnapshot>> {
        if self.active.is_some() {
            self.snapshot_active().await?;
        }
        self.frozen = true;
        let keys: Vec<_> = self.drafts.keys().cloned().collect();
        let mut snapshots = Vec::with_capacity(keys.len());
        for key in keys {
            snapshots.push(self.refresh_draft(&key).await?);
        }
        Ok(snapshots)
    }
    async fn restart(&mut self) -> Result<()> {
        let active = self.active.clone();
        let saved: Vec<_> = self
            .drafts
            .iter()
            .map(|(key, draft)| {
                (
                    key.clone(),
                    draft.snapshot.text.clone(),
                    draft.snapshot.cursor,
                    draft.insert,
                )
            })
            .collect();
        self.rpc.take(); // kills only the still-owned old generation; no key replay.
        self.generation = Uuid::new_v4();
        self.grid = GridReducer::new(self.generation);
        // Keep recovery snapshots if spawning/initialization fails, even across
        // repeated restart attempts. Old handles are filtered by generation.
        self.sequence = 0;
        self.marker = None;
        self.failed = None;
        self.frozen = false;
        self.composition = false;
        self.vim_entered = false;
        self.events.grid = None;
        self.events.drafts.clear();
        self.events.activated = None;
        self.events.ready = false;
        self.events.overflowed = false;
        self.events.control.clear();
        self.initialize().await?;
        for (key, text, cursor, insert) in saved {
            self.create_draft(key, text, cursor, insert).await?;
        }
        if let Some(key) = active {
            self.activate(key).await?;
        }
        let serial = self.grid.snapshot().serial;
        self.call("nvim_command", vec!["redraw!".into()]).await?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        while self.grid.snapshot().serial == serial {
            self.pump_until(deadline).await?;
        }
        self.events.emit(NvimEvent::Ready);
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<()> {
        self.frozen = true;
        if let Some(rpc) = &self.rpc {
            // A quit commonly closes stdout before replying. It is intentionally
            // sent without waiting; persistence is the caller's prerequisite.
            if !rpc.is_failed() {
                let _ = rpc.call("nvim_command", vec!["qa!".into()]).await;
            }
        }
        if let Some(mut rpc) = self.rpc.take() {
            rpc.stop().await?;
        }
        Ok(())
    }

    async fn run(&mut self, commands: Receiver<NvimCommand>) {
        enum Wake {
            Command(std::result::Result<NvimCommand, async_channel::RecvError>),
            Incoming(std::result::Result<Incoming, async_channel::RecvError>),
            Tick,
        }
        let mut deferred = None;
        loop {
            self.events.flush();
            let wake = if let Some(command) = deferred.take() {
                Wake::Command(Ok(command))
            } else {
                let incoming = self
                    .rpc
                    .as_ref()
                    .filter(|_| self.failed.is_none())
                    .map(|rpc| rpc.incoming.clone());
                smol::future::race(
                    async { Wake::Command(commands.recv().await) },
                    smol::future::race(
                        async {
                            match incoming {
                                Some(incoming) => Wake::Incoming(incoming.recv().await),
                                None => std::future::pending().await,
                            }
                        },
                        async {
                            smol::Timer::after(Duration::from_millis(8)).await;
                            Wake::Tick
                        },
                    ),
                )
                .await
            };
            match wake {
                Wake::Tick => continue,
                Wake::Incoming(Ok(incoming)) => {
                    let _ = self.incoming(incoming);
                    continue;
                }
                Wake::Incoming(Err(_)) => {
                    self.fail("Neovim transport closed".into());
                    continue;
                }
                Wake::Command(Err(_)) => {
                    let _ = self.shutdown().await;
                    break;
                }
                Wake::Command(Ok(command)) => {
                    // Adjacent resize requests collapse, without moving resize
                    // across an input/snapshot mutation boundary.
                    let command = if let NvimCommand::Resize { mut cols, mut rows } = command {
                        while let Ok(next) = commands.try_recv() {
                            match next {
                                NvimCommand::Resize { cols: c, rows: r } => {
                                    cols = c;
                                    rows = r;
                                }
                                other => {
                                    deferred = Some(other);
                                    break;
                                }
                            }
                        }
                        NvimCommand::Resize { cols, rows }
                    } else {
                        command
                    };
                    let stop = matches!(&command, NvimCommand::Shutdown { .. });
                    self.command(command).await;
                    if stop {
                        break;
                    }
                }
            }
        }
    }
    async fn command(&mut self, command: NvimCommand) {
        match command {
            NvimCommand::Shutdown { reply } => {
                let _ = reply.try_send(self.shutdown().await);
            }
            NvimCommand::Restart { reply } => {
                let result = self.restart().await;
                if let Err(error) = &result {
                    self.fail(format!("Editor restart failed: {error:#}"));
                }
                let _ = reply.try_send(result);
            }
            NvimCommand::SetTheme(theme) => {
                self.theme = Some(theme);
                if let Err(error) = self.apply_theme(theme).await {
                    self.fail(format!("Editor theme could not be applied: {error:#}"));
                }
            }
            NvimCommand::ResumeInput => self.frozen = false,
            NvimCommand::SetComposition(value) => self.composition = value,
            NvimCommand::OpenDraft { key, text, reply } => {
                let result = async {
                    self.available()?;
                    if !self.drafts.contains_key(&key) {
                        self.create_draft(key, text, (0, 0), false).await?;
                    }
                    Ok(())
                }
                .await;
                let _ = reply.try_send(result);
            }
            NvimCommand::FocusDraft { key, reply } => {
                let result = async {
                    self.available()?;
                    self.focus_draft(key).await
                }
                .await;
                let _ = reply.try_send(result);
            }
            NvimCommand::Snapshot { key, reply } => {
                let result = async {
                    self.available()?;
                    self.snapshot(key).await
                }
                .await;
                let _ = reply.try_send(result);
            }
            NvimCommand::ReplaceIfRevision {
                key,
                revision,
                text,
                reply,
            } => {
                let result = async {
                    self.available()?;
                    self.replace_if_revision(key, revision, text).await
                }
                .await;
                let _ = reply.try_send(result);
            }
            NvimCommand::PersistenceBarrier { reply } => {
                let result = async {
                    self.available()?;
                    self.persist_barrier().await
                }
                .await;
                let _ = reply.try_send(result);
            }
            NvimCommand::CancelCommand { reply } => {
                let result = async {
                    self.available()?;
                    self.composition = false;
                    self.raw_input("<Esc>").await?;
                    self.drain().await?;
                    Ok(())
                }
                .await;
                let _ = reply.try_send(result);
            }
            command => {
                let composition = matches!(
                    &command,
                    NvimCommand::Replace { .. } | NvimCommand::ReplaceCmdline { .. }
                );
                let result: Result<()> = async {
                    self.available()?;
                    match command {
                        NvimCommand::Input(keys) => {
                            ensure!(!self.composition, "Native composition is active");
                            self.raw_input(&keys).await?;
                        }
                        NvimCommand::Paste(text) => self.paste(text).await?,
                        NvimCommand::Replace {
                            key,
                            revision,
                            range,
                            text,
                        } => self.replace(key, revision, range, text).await?,
                        NvimCommand::ReplaceCmdline {
                            expected,
                            range,
                            text,
                        } => self.replace_cmdline(expected, range, text).await?,
                        NvimCommand::Mouse {
                            button,
                            action,
                            modifiers,
                            row,
                            col,
                        } => {
                            self.call(
                                "nvim_input_mouse",
                                vec![
                                    button.into(),
                                    action.into(),
                                    modifiers.into(),
                                    0.into(),
                                    row.into(),
                                    col.into(),
                                ],
                            )
                            .await?;
                        }
                        NvimCommand::Resize { cols, rows } => {
                            self.config.cols = cols.max(1);
                            self.config.rows = rows.max(2);
                            self.call(
                                "nvim_ui_try_resize",
                                vec![self.config.cols.into(), self.config.rows.into()],
                            )
                            .await?;
                        }
                        NvimCommand::Focus(focused) => {
                            if self.methods.contains("nvim_ui_set_focus") {
                                self.call("nvim_ui_set_focus", vec![focused.into()]).await?;
                            }
                        }
                        NvimCommand::Copy { cut } => {
                            let mode = self.drain().await?;
                            ensure!(
                                mode.starts_with('v')
                                    || mode.starts_with('V')
                                    || mode.starts_with('\u{16}'),
                                "Select text visually in Neovim before copying or cutting"
                            );
                            self.raw_input(if cut { "\"+d" } else { "\"+y" }).await?;
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    if self.rpc.as_ref().is_some_and(Rpc::is_failed) {
                        self.fail(format!("{error:#}"));
                    } else if self.failed.is_none() {
                        self.events.emit(if composition {
                            NvimEvent::CompositionRejected(format!("{error:#}"))
                        } else {
                            NvimEvent::InputRejected(format!("{error:#}"))
                        });
                    }
                }
            }
        }
    }
}

fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("Neovim lines must be an array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("Neovim line is not UTF-8 text")
        })
        .collect()
}
fn parse_cursor(value: &Value) -> Result<(usize, usize)> {
    let values = value.as_array().context("Neovim cursor must be an array")?;
    ensure!(values.len() >= 2, "Incomplete Neovim cursor");
    let row = values[0].as_u64().context("Invalid cursor row")?;
    ensure!(row > 0, "Neovim cursor row must be one-based");
    Ok((
        row as usize - 1,
        values[1].as_u64().context("Invalid cursor byte column")? as usize,
    ))
}
fn valid_regtype(value: &str) -> bool {
    value == "v"
        || value == "V"
        || value
            .strip_prefix('\u{16}')
            .is_some_and(|width| width.is_empty() || width.parse::<u32>().is_ok())
}
fn utf16_range(text: &str, range: Range<usize>) -> Result<Range<usize>> {
    fn offset(text: &str, wanted: usize) -> Result<usize> {
        let mut utf16 = 0;
        for (byte, ch) in text.char_indices() {
            if utf16 == wanted {
                return Ok(byte);
            }
            utf16 += ch.len_utf16();
            ensure!(
                utf16 <= wanted,
                "Native replacement range splits a UTF-16 surrogate pair"
            );
        }
        ensure!(
            utf16 == wanted,
            "Native replacement range exceeds the verified shadow"
        );
        Ok(text.len())
    }
    ensure!(
        range.start <= range.end,
        "Reversed native replacement range"
    );
    Ok(offset(text, range.start)?..offset(text, range.end)?)
}
fn byte_position(text: &str, byte: usize) -> (usize, usize) {
    let prefix = &text[..byte];
    let row = prefix.bytes().filter(|value| *value == b'\n').count();
    let column = prefix
        .rfind('\n')
        .map_or(byte, |newline| byte - newline - 1);
    (row, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_replacements_preserve_unicode_boundaries() {
        let text = "a😀\n日本語 e\u{301}";
        assert_eq!(&text[utf16_range(text, 1..3).unwrap()], "😀");
        assert!(utf16_range(text, 2..3).is_err());
        let range = utf16_range(text, 4..7).unwrap();
        assert_eq!(byte_position(text, range.start), (1, 0));
        assert_eq!(byte_position(text, range.end), (1, 9));
    }
}
