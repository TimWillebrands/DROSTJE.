use super::{
    commands::{RESTART_NEOVIM_LABEL, RestartTerminal},
    theme,
};
use crate::model::{ProjectId, SpaceId, WorktreeId};
use ghostty_terminal::{Terminal, TerminalOptions};
use gpui_kit::{
    component::{ActiveTheme, alert::Alert, button::Button, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

/// Tab identity. Saved layouts keep it so the tab reopens after a restart, always
/// with a new shell; it never names a Thread or a persisted shell session.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct TerminalId(uuid::Uuid);
impl TerminalId {
    pub(super) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}
impl std::fmt::Display for TerminalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What a Neovim tab opened: a file, at a 1-based position, or nothing. Saved with the
/// tab so a restart reopens the same file in a new Neovim; buffers, undo history and
/// unwritten changes belong to the process and are never restored.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NeovimTarget {
    #[serde(default)]
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

/// The user's own Neovim, with their configuration, in a native terminal surface.
/// Unlike the composer's isolated editor, this one reads and writes the file itself.
struct NeovimLaunch {
    target: NeovimTarget,
    /// The `nvim` executable setting; blank discovers it on PATH.
    setting: Option<String>,
    /// Resolved for the running process; the remote client uses the same binary.
    executable: Option<PathBuf>,
    /// The `--listen` address of the running process.
    socket: Option<PathBuf>,
    exit_reported: bool,
}

/// The Neovim in a tab exited on its own (`:q`), so the tab has nothing left to show.
pub struct NeovimExited;
impl EventEmitter<NeovimExited> for TerminalPane {}

/// Argv after the executable: listen for jumps, open the file at its position.
/// Every element is a separate argument; nothing is shell source.
pub fn neovim_args(target: &NeovimTarget, socket: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--listen".into(), socket.into()];
    if let Some(file) = &target.file {
        match (target.line, target.column) {
            (Some(line), Some(column)) => {
                args.push(format!("+call cursor({line}, {column})").into())
            }
            (Some(line), None) => args.push(format!("+{line}").into()),
            _ => {}
        }
        args.push("--".into());
        args.push(file.into());
    }
    args
}

/// A Vimscript expression that shows `file` in the running Neovim (splitting rather
/// than abandoning a modified buffer, per `:drop`) and moves the cursor. It is
/// evaluated, not typed, so it cannot land in the buffer whatever mode Neovim is in.
pub fn neovim_jump_expression(file: &Path, line: Option<u32>, column: Option<u32>) -> String {
    let quoted = file.display().to_string().replace('\'', "''");
    let mut commands = vec![format!("'drop ' .. fnameescape('{quoted}')")];
    if let Some(line) = line {
        commands.push(format!("'call cursor({line}, {})'", column.unwrap_or(1)));
        commands.push("'normal! zz'".into());
    }
    format!("execute([{}])", commands.join(", "))
}

/// Ask the Neovim listening on `socket` to evaluate `expression`. Blocking; run it on a
/// background executor. Neovim answers only between keys, so a pending prompt there
/// times out instead of hanging this call.
pub fn neovim_remote(executable: &Path, socket: &Path, expression: &str) -> Result<(), String> {
    let mut child = Command::new(executable)
        .args(["-u", "NONE", "-i", "NONE", "-n", "--headless", "--server"])
        .arg(socket)
        .arg("--remote-expr")
        .arg(expression)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Neovim could not be asked to open the file: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "Neovim did not respond. Finish its prompt or command, then open the file again."
                        .into(),
                );
            }
            Err(error) => return Err(format!("Neovim request failed: {error}")),
        }
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    // The client reports `Lua: Vim:E492: …` followed by its own stack trace.
    let stderr = stderr.lines().next().unwrap_or_default().trim();
    let stderr = stderr.strip_prefix("Lua: ").unwrap_or(stderr);
    let stderr = stderr.strip_prefix("Vim:").unwrap_or(stderr);
    if status.success() && stderr.is_empty() {
        Ok(())
    } else if stderr.is_empty() {
        Err(format!("Neovim rejected the request ({status})"))
    } else {
        Err(format!("Neovim rejected the request: {stderr}"))
    }
}

pub struct TerminalPane {
    pub project_id: ProjectId,
    /// The worktree the shell started in, when it was opened from a Conversation in one.
    pub worktree: Option<WorktreeId>,
    pub space_id: SpaceId,
    pub root: PathBuf,
    neovim: Option<NeovimLaunch>,
    native: Option<Entity<Terminal>>,
    error: Option<String>,
    starting: bool,
    /// Bumped by every start and close, so a slow executable lookup cannot start a
    /// process for a tab that was closed or restarted meanwhile.
    generation: u64,
    visible: bool,
    focus: FocusHandle,
    native_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}
impl TerminalPane {
    pub fn new(
        project_id: ProjectId,
        worktree: Option<WorktreeId>,
        space_id: SpaceId,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_launch(project_id, worktree, space_id, root, None, window, cx)
    }
    /// A tab running the user's Neovim in `root`, opened on `target`.
    #[expect(clippy::too_many_arguments, reason = "a tab's full launch context")]
    pub fn neovim(
        project_id: ProjectId,
        worktree: Option<WorktreeId>,
        space_id: SpaceId,
        root: PathBuf,
        target: NeovimTarget,
        setting: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let launch = NeovimLaunch {
            target,
            setting,
            executable: None,
            socket: None,
            exit_reported: false,
        };
        Self::with_launch(
            project_id,
            worktree,
            space_id,
            root,
            Some(launch),
            window,
            cx,
        )
    }
    fn with_launch(
        project_id: ProjectId,
        worktree: Option<WorktreeId>,
        space_id: SpaceId,
        root: PathBuf,
        neovim: Option<NeovimLaunch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe_global::<gpui_kit::component::Theme>(|pane, cx| {
            if let Some(native) = &pane.native {
                let theme = theme::terminal_theme(cx);
                native.update(cx, |terminal, _| terminal.update_theme(theme));
            }
            cx.notify();
        })];
        cx.defer_in(window, |pane, window, cx| pane.start(window, cx));
        Self {
            project_id,
            worktree,
            space_id,
            root,
            neovim,
            native: None,
            error: None,
            starting: true,
            generation: 0,
            visible: false,
            focus: cx.focus_handle(),
            native_subscription: None,
            _subscriptions: subscriptions,
        }
    }
    /// What this tab's Neovim opened; `None` for a shell.
    pub fn neovim_target(&self) -> Option<&NeovimTarget> {
        self.neovim.as_ref().map(|launch| &launch.target)
    }
    /// The executable and address a remote request to this tab's running Neovim uses.
    pub fn neovim_server(&self, cx: &App) -> Option<(PathBuf, PathBuf)> {
        let launch = self.neovim.as_ref()?;
        if !self.is_alive(cx) {
            return None;
        }
        Some((launch.executable.clone()?, launch.socket.clone()?))
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.starting {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let Some(launch) = &mut self.neovim else {
            self.spawn_native(None, window, cx);
            return;
        };
        launch.exit_reported = false;
        let setting = launch.setting.clone();
        // PATH discovery and canonicalisation touch the file system: not on this thread.
        cx.spawn_in(window, async move |pane, cx| {
            let resolved = smol::unblock(move || {
                crate::platform::resolve_executable("nvim", setting.as_deref())
            })
            .await;
            let _ = pane.update_in(cx, |pane, window, cx| {
                if pane.generation != generation || !pane.starting {
                    return;
                }
                match resolved {
                    Ok(executable) => {
                        let id = uuid::Uuid::new_v4().simple().to_string();
                        // Unix socket paths are short (104 bytes on macOS): keep the name brief.
                        let socket =
                            std::env::temp_dir().join(format!("drostje-nvim-{}.sock", &id[..8]));
                        let Some(launch) = &mut pane.neovim else {
                            return;
                        };
                        let args = neovim_args(&launch.target, &socket);
                        launch.executable = Some(executable.clone());
                        launch.socket = Some(socket);
                        pane.spawn_native(Some((executable, args)), window, cx);
                    }
                    Err(error) => {
                        pane.error = Some(format!("{error:#}"));
                        pane.starting = false;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn spawn_native(
        &mut self,
        program: Option<(PathBuf, Vec<OsString>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restore_focus = self.focus.contains_focused(window, cx);
        let mut options = TerminalOptions::new(self.root.clone());
        options.focus_on_spawn = false;
        options.theme = Some(theme::terminal_theme(cx));
        if let Some((executable, args)) = program {
            options.program = Some(executable);
            options.args = args;
        }
        match Terminal::spawn(options, window, cx) {
            Ok(native) => {
                native.update(cx, |terminal, _| terminal.set_visible(self.visible));
                self.native_subscription =
                    Some(cx.observe_in(&native, window, |pane, native, window, cx| {
                        let terminal = native.read(cx);
                        let exited = !terminal.is_alive();
                        if (exited || terminal.backend_error().is_some())
                            && terminal.focus_handle(cx).is_focused(window)
                        {
                            // The native input leaves the tree; retain the app's keyboard path.
                            pane.focus.focus(window, cx);
                        }
                        if exited
                            && let Some(launch) = &mut pane.neovim
                            && !std::mem::replace(&mut launch.exit_reported, true)
                        {
                            cx.emit(NeovimExited);
                        }
                        cx.notify();
                    }));
                self.native = Some(native);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.starting = false;
        if restore_focus {
            self.focus(window, cx);
        }
        cx.notify();
    }
    pub fn is_alive(&self, cx: &App) -> bool {
        self.native
            .as_ref()
            .is_some_and(|native| native.read(cx).is_alive())
    }
    #[cfg(all(test, feature = "runtime-qa"))]
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn native(&self) -> Option<Entity<Terminal>> {
        self.native.clone()
    }
    pub fn snapshot(&self, cx: &mut App) -> Result<Arc<RenderImage>, String> {
        self.native
            .as_ref()
            .ok_or_else(|| "Terminal has no native surface".to_owned())?
            .update(cx, |terminal, _| terminal.snapshot())
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(native) = &self.native
            && native.read(cx).is_alive()
        {
            native.update(cx, |terminal, cx| terminal.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
    }
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if let Some(native) = &self.native {
            native.update(cx, |terminal, _| terminal.set_visible(visible));
        }
        cx.notify();
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.starting = false;
        self.generation += 1;
        self.visible = false;
        self.native_subscription.take();
        if let Some(native) = self.native.take() {
            native.update(cx, |terminal, _| terminal.close());
        }
        cx.notify();
    }
    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible;
        self.close(cx);
        self.visible = visible;
        self.starting = true;
        self.error = None;
        cx.defer_in(window, |pane, window, cx| pane.start(window, cx));
        cx.notify();
    }
}
impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let backend_error = self
            .native
            .as_ref()
            .and_then(|native| native.read(cx).backend_error());
        let neovim = self.neovim.is_some();
        let content = if self.starting {
            let message = if neovim {
                "Starting Neovim…"
            } else {
                "Starting terminal…"
            };
            Alert::info("terminal-starting", message).into_any_element()
        } else if let Some(error) = &self.error {
            let message = if neovim {
                format!(
                    "Neovim could not start: {error}. Set the Neovim executable in Settings, then restart Neovim."
                )
            } else {
                format!("Terminal could not start: {error}")
            };
            Alert::error("terminal-start-error", message).into_any_element()
        } else if let Some(error) = backend_error {
            let restart = if neovim {
                "Restart Neovim"
            } else {
                "Restart terminal"
            };
            Alert::error(
                "terminal-render-error",
                format!("Terminal rendering stopped: {error}. {restart} to try again."),
            )
            .into_any_element()
        } else if let Some(native) = &self.native
            && native.read(cx).is_alive()
        {
            native.clone().into_any_element()
        } else {
            // A Neovim tab closes when Neovim exits; this shows only until it does.
            let message = if neovim {
                "Neovim exited."
            } else {
                "The shell exited. Restart terminal to open a new shell in this project."
            };
            Alert::info("terminal-exited", message).into_any_element()
        };
        let stopped = !self.starting && (!self.is_alive(cx) || backend_error.is_some());
        v_flex()
            .id("terminal-pane")
            .test_support()
            .key_context("TerminalPane")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .when(self.starting || stopped, |view| view.p_3().gap_2())
            .child(content)
            .children(stopped.then(|| {
                h_flex().child(
                    Button::new("restart-terminal")
                        .label(if neovim {
                            RESTART_NEOVIM_LABEL
                        } else {
                            super::commands::Command::RestartTerminal.label()
                        })
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(RestartTerminal), cx)
                        }),
                )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{NeovimTarget, neovim_args, neovim_jump_expression, neovim_remote};
    use std::{
        ffi::OsString,
        path::Path,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn neovim_argv_opens_the_file_at_its_position_after_the_option_terminator() {
        let socket = Path::new("/tmp/drostje-nvim-1.sock");
        let target = |line, column| NeovimTarget {
            file: Some("/work/-odd name.rs".into()),
            line,
            column,
        };
        assert_eq!(
            strings(neovim_args(&target(Some(149), None), socket)),
            [
                "--listen",
                "/tmp/drostje-nvim-1.sock",
                "+149",
                "--",
                "/work/-odd name.rs"
            ]
        );
        assert_eq!(
            strings(neovim_args(&target(Some(3), Some(7)), socket)),
            [
                "--listen",
                "/tmp/drostje-nvim-1.sock",
                "+call cursor(3, 7)",
                "--",
                "/work/-odd name.rs"
            ]
        );
        assert_eq!(
            strings(neovim_args(&NeovimTarget::default(), socket)),
            ["--listen", "/tmp/drostje-nvim-1.sock"]
        );
    }

    #[test]
    fn jump_expression_quotes_the_path_for_vimscript() {
        assert_eq!(
            neovim_jump_expression(Path::new("/w/it's.rs"), Some(12), None),
            "execute(['drop ' .. fnameescape('/w/it''s.rs'), 'call cursor(12, 1)', 'normal! zz'])"
        );
        assert_eq!(
            neovim_jump_expression(Path::new("/w/a.rs"), None, None),
            "execute(['drop ' .. fnameescape('/w/a.rs')])"
        );
    }

    /// Real Neovim: a listening instance evaluates the jump and reports failures.
    #[test]
    fn remote_jump_moves_a_listening_neovim_and_reports_errors() {
        let Ok(executable) = crate::platform::resolve_executable("nvim", None) else {
            eprintln!("nvim not installed; skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "drostje-nvim-remote-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let socket = dir.join("s.sock");
        let mut server = Command::new(&executable)
            .args(["--headless", "-u", "NONE", "-i", "NONE", "-n", "--listen"])
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let jumped = neovim_remote(
            &executable,
            &socket,
            &neovim_jump_expression(&file, Some(3), None),
        );
        let position = Command::new(&executable)
            .args(["-u", "NONE", "-i", "NONE", "-n", "--headless", "--server"])
            .arg(&socket)
            .args(["--remote-expr", "expand('%:t') .. ':' .. line('.')"])
            .output()
            .unwrap();
        let failed = neovim_remote(&executable, &socket, "execute('DrostjeMissing')");
        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(jumped, Ok(()));
        assert_eq!(String::from_utf8_lossy(&position.stdout).trim(), "a.txt:3");
        assert_eq!(
            failed,
            Err("Neovim rejected the request: E492: Not an editor command: DrostjeMissing".into())
        );
    }
}
