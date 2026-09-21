use super::{commands::RestartTerminal, theme};
use crate::model::{ProjectId, SpaceId};
use ghostty_terminal::{Terminal, TerminalOptions};
use gpui_kit::{
    component::{ActiveTheme, alert::Alert, button::Button, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use std::{path::PathBuf, sync::Arc};

/// Runtime-only identity. It never names a Thread or a persisted shell session.
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

pub struct TerminalPane {
    pub project_id: ProjectId,
    pub space_id: SpaceId,
    pub root: PathBuf,
    native: Option<Entity<Terminal>>,
    error: Option<String>,
    starting: bool,
    visible: bool,
    focus: FocusHandle,
    native_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}
impl TerminalPane {
    pub fn new(
        project_id: ProjectId,
        space_id: SpaceId,
        root: PathBuf,
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
            space_id,
            root,
            native: None,
            error: None,
            starting: true,
            visible: false,
            focus: cx.focus_handle(),
            native_subscription: None,
            _subscriptions: subscriptions,
        }
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.starting {
            return;
        }
        let restore_focus = self.focus.contains_focused(window, cx);
        let mut options = TerminalOptions::new(self.root.clone());
        options.focus_on_spawn = false;
        options.theme = Some(theme::terminal_theme(cx));
        match Terminal::spawn(options, window, cx) {
            Ok(native) => {
                native.update(cx, |terminal, _| terminal.set_visible(self.visible));
                self.native_subscription =
                    Some(cx.observe_in(&native, window, |pane, native, window, cx| {
                        let terminal = native.read(cx);
                        if (!terminal.is_alive() || terminal.backend_error().is_some())
                            && terminal.focus_handle(cx).is_focused(window)
                        {
                            // The native input leaves the tree; retain the app's keyboard path.
                            pane.focus.focus(window, cx);
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
        let content = if self.starting {
            Alert::info("terminal-starting", "Starting terminal…").into_any_element()
        } else if let Some(error) = &self.error {
            Alert::error(
                "terminal-start-error",
                format!("Terminal could not start: {error}"),
            )
            .into_any_element()
        } else if let Some(error) = backend_error {
            Alert::error(
                "terminal-render-error",
                format!("Terminal rendering stopped: {error}. Restart terminal to try again."),
            )
            .into_any_element()
        } else if let Some(native) = &self.native
            && native.read(cx).is_alive()
        {
            native.clone().into_any_element()
        } else {
            Alert::info(
                "terminal-exited",
                "The shell exited. Restart terminal to open a new shell in this project.",
            )
            .into_any_element()
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
                        .label("Restart terminal")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(RestartTerminal), cx)
                        }),
                )
            }))
    }
}
