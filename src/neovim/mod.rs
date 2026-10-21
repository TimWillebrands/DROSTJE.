//! One real, isolated Neovim instance per workspace window.
pub mod grid;
mod rpc;
mod session;

use crate::model::{DraftKey, DraftRevision, DraftSnapshot};
use anyhow::{Result, anyhow};
use async_channel::{Receiver, Sender};
use std::{ops::Range, path::PathBuf, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardContent {
    pub lines: Vec<String>,
    /// Neovim characterwise `v`, linewise `V`, or blockwise `\u{16}<width>`.
    pub regtype: String,
}
impl ClipboardContent {
    pub fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.regtype == "V" {
            text.push('\n');
        }
        text
    }
}

/// The kit theme as Neovim sees it: packed 24-bit RGB, already flattened onto the
/// editor background, plus whether the theme is dark. `bridge.lua` maps every
/// highlight group the editor can show onto these roles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NvimTheme {
    pub dark: bool,
    pub foreground: u32,
    pub background: u32,
    pub muted: u32,
    pub surface: u32,
    pub border: u32,
    pub selection: u32,
    pub caret: u32,
    pub accent: u32,
    pub popover: u32,
    pub popover_foreground: u32,
    pub success: u32,
    pub warning: u32,
    pub danger: u32,
    pub info: u32,
}
impl NvimTheme {
    pub fn colors(&self) -> [(&'static str, u32); 14] {
        [
            ("foreground", self.foreground),
            ("background", self.background),
            ("muted", self.muted),
            ("surface", self.surface),
            ("border", self.border),
            ("selection", self.selection),
            ("caret", self.caret),
            ("accent", self.accent),
            ("popover", self.popover),
            ("popover_foreground", self.popover_foreground),
            ("success", self.success),
            ("warning", self.warning),
            ("danger", self.danger),
            ("info", self.info),
        ]
    }
}

#[derive(Clone, Debug)]
pub struct NvimConfig {
    pub executable: PathBuf,
    pub cols: u32,
    pub rows: u32,
}
impl NvimConfig {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            cols: 80,
            rows: 12,
        }
    }
}

pub enum NvimEvent {
    Grid(Arc<grid::GridSnapshot>),
    DraftChanged(DraftSnapshot),
    DraftActivated(DraftSnapshot),
    ClipboardGet {
        reply: Sender<std::result::Result<ClipboardContent, String>>,
    },
    ClipboardSet(ClipboardContent),
    CompositionCommitted,
    CompositionRejected(String),
    InputRejected(String),
    /// The user quit the way Vim quits (`:q`, `:wq`, `ZZ`…). Neovim and the draft stay;
    /// the editor gives up focus instead.
    Leave,
    Failed(String),
    Ready,
}

/// All mutations are serialized by the owned background actor. UI callbacks never wait.
pub enum NvimCommand {
    Input(String),
    Paste(String),
    Mouse {
        button: String,
        action: String,
        modifiers: String,
        row: u32,
        col: u32,
    },
    Resize {
        cols: u32,
        rows: u32,
    },
    Theme(NvimTheme),
    Focus(bool),
    SetComposition(bool),
    Replace {
        key: DraftKey,
        revision: DraftRevision,
        range: Range<usize>,
        text: String,
    },
    ReplaceCmdline {
        expected: String,
        range: Range<usize>,
        text: String,
    },
    Copy {
        cut: bool,
    },
    OpenDraft {
        key: DraftKey,
        text: String,
        reply: Sender<Result<()>>,
    },
    FocusDraft {
        key: DraftKey,
        reply: Sender<Result<()>>,
    },
    Snapshot {
        key: DraftKey,
        reply: Sender<Result<DraftSnapshot>>,
    },
    ReplaceIfRevision {
        key: DraftKey,
        revision: DraftRevision,
        text: String,
        reply: Sender<Result<bool>>,
    },
    /// Writes `/name ` over the leading command the active draft starts with (or before
    /// its text) and leaves the cursor after it in insert mode. The edit is computed from
    /// the buffer as it is when the command runs, after every earlier keystroke.
    InsertSlashCommand {
        key: DraftKey,
        name: String,
        reply: Sender<Result<()>>,
    },
    PersistenceBarrier {
        reply: Sender<Result<Vec<DraftSnapshot>>>,
    },
    ResumeInput,
    CancelCommand {
        reply: Sender<Result<()>>,
    },
    Restart {
        reply: Sender<Result<()>>,
    },
    Shutdown {
        reply: Sender<Result<()>>,
    },
}

#[derive(Clone)]
pub struct NvimEditor {
    pub commands: Sender<NvimCommand>,
    /// Exactly one consumer: normally NvimView forwards semantic events to Workspace.
    pub events: Receiver<NvimEvent>,
}
impl NvimEditor {
    pub async fn open(config: NvimConfig) -> Result<Self> {
        session::open(config).await
    }

    pub fn try_command(&self, command: NvimCommand) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| anyhow!("Neovim command queue unavailable: {error}"))
    }

    pub async fn open_draft(&self, key: DraftKey, text: String) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::OpenDraft { key, text, reply })
            .await?;
        result.recv().await?
    }
    pub async fn focus_draft(&self, key: DraftKey) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::FocusDraft { key, reply })
            .await?;
        result.recv().await?
    }
    pub async fn snapshot(&self, key: DraftKey) -> Result<DraftSnapshot> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::Snapshot { key, reply })
            .await?;
        result.recv().await?
    }
    pub async fn replace_if_revision(
        &self,
        key: DraftKey,
        revision: DraftRevision,
        text: String,
    ) -> Result<bool> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::ReplaceIfRevision {
                key,
                revision,
                text,
                reply,
            })
            .await?;
        result.recv().await?
    }
    pub async fn insert_slash_command(&self, key: DraftKey, name: String) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::InsertSlashCommand { key, name, reply })
            .await?;
        result.recv().await?
    }
    /// Quiesce and snapshot before persistence. This deliberately does not quit Neovim.
    /// After the caller durably writes every result it may call shutdown; on failure resume_input.
    pub async fn persistence_barrier(&self) -> Result<Vec<DraftSnapshot>> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::PersistenceBarrier { reply })
            .await?;
        result.recv().await?
    }
    pub async fn resume_input(&self) -> Result<()> {
        self.commands.send(NvimCommand::ResumeInput).await?;
        Ok(())
    }
    pub async fn cancel_command(&self) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands
            .send(NvimCommand::CancelCommand { reply })
            .await?;
        result.recv().await?
    }
    pub async fn restart(&self) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(NvimCommand::Restart { reply }).await?;
        result.recv().await?
    }
    pub async fn shutdown(&self) -> Result<()> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(NvimCommand::Shutdown { reply }).await?;
        result.recv().await?
    }
}
