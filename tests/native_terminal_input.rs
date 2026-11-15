//! Opt-in real-window regression: no providers, user configuration, or project files.
//! Run with DROSTJE_NATIVE_INPUT_QA=1; requires a GUI session and Neovim on PATH.
use ghostty_terminal::{Terminal, TerminalOptions};
use gpui_kit::{component::Root, *};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

struct Surface {
    terminal: Entity<Terminal>,
    chrome: FocusHandle,
    neighbours: [FocusHandle; 4],
    navigation: Vec<&'static str>,
    left: bool,
    palette: bool,
}
impl Render for Surface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Workspace Thread List TerminalPane")
            .track_focus(&self.chrome)
            .size_full()
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::LeaveEditor, window, cx| {
                    this.left = true;
                    this.chrome.focus(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::FocusLeft, window, cx| {
                    this.navigation.push("left");
                    this.neighbours[0].focus(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::FocusDown, window, cx| {
                    this.navigation.push("down");
                    this.neighbours[1].focus(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::FocusUp, window, cx| {
                    this.navigation.push("up");
                    this.neighbours[2].focus(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::FocusRight, window, cx| {
                    this.navigation.push("right");
                    this.neighbours[3].focus(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &drostje::ui::commands::OpenPalette, _, _| {
                    this.palette = true;
                }),
            )
            .flex()
            .flex_col()
            .child(div().h_8().track_focus(&self.neighbours[2]))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().w_8().track_focus(&self.neighbours[0]))
                    .child(div().flex_1().min_w_0().child(self.terminal.clone()))
                    .child(div().w_8().track_focus(&self.neighbours[3])),
            )
            .child(div().h_8().track_focus(&self.neighbours[1]))
    }
}

async fn remote(nvim: &Path, socket: &Path, expression: &str) -> anyhow::Result<String> {
    let (nvim, socket, expression) = (nvim.to_owned(), socket.to_owned(), expression.to_owned());
    smol::unblock(move || {
        let mut child = std::process::Command::new(nvim)
            .args(["--clean", "-n", "-i", "NONE", "--headless", "--server"])
            .arg(socket)
            .arg("--remote-expr")
            .arg(expression)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Neovim RPC timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output()?;
        anyhow::ensure!(
            output.status.success(),
            "Neovim RPC: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?
            .trim_end_matches(['\r', '\n'])
            .to_owned())
    })
    .await
}

async fn wait_for(
    nvim: &Path,
    socket: &Path,
    expression: &str,
    expected: &str,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Ok(value) = remote(nvim, socket, expression).await {
            if value == expected {
                return Ok(());
            }
            last = value;
        }
        smol::Timer::after(Duration::from_millis(20)).await;
    }
    anyhow::bail!("{expression}: expected {expected:?}, observed {last:?}")
}

async fn navigate_from_native(
    window: WindowHandle<Root>,
    surface: &Entity<Surface>,
    nvim: &Path,
    socket: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let state = "json_encode([mode(), getline(1, '$'), getcmdline(), getcmdpos(), getpos('.')])";
    let before = remote(nvim, socket, state).await?;
    for (key, direction, neighbour) in [
        ("secondary-alt-h", "left", 0),
        ("secondary-alt-j", "down", 1),
        ("secondary-alt-k", "up", 2),
        ("secondary-alt-l", "right", 3),
        ("secondary-alt-left", "left", 0),
        ("secondary-alt-down", "down", 1),
        ("secondary-alt-up", "up", 2),
        ("secondary-alt-right", "right", 3),
    ] {
        cx.update_window(window.into(), |_, window, cx| {
            let fixture = surface.read(cx);
            let terminal = fixture.terminal.clone();
            let native_focus = terminal.read(cx).focus_handle(cx);
            let destination = fixture.neighbours[neighbour].clone();
            let dispatched = fixture.navigation.len();
            assert!(!fixture.left, "{key} required the explicit escape hatch");
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
            assert!(
                native_focus.is_focused(window),
                "{key}: terminal not focused"
            );
            window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx);
            assert!(
                !window.has_pending_keystrokes(),
                "{key} started an app chord"
            );
            assert!(
                destination.is_focused(window),
                "{key} did not focus {direction}"
            );
            assert!(
                !native_focus.is_focused(window),
                "{key} left native focus behind"
            );
            let surface = surface.read(cx);
            assert_eq!(&surface.navigation[dispatched..], &[direction], "{key}");
        })?;
        // Allow native PTY input to settle before observing the real Neovim state.
        smol::Timer::after(Duration::from_millis(50)).await;
        anyhow::ensure!(
            remote(nvim, socket, state).await? == before,
            "{key} changed Neovim's mode, buffer, cursor or command line"
        );
    }
    cx.update_window(window.into(), |_, window, cx| {
        let terminal = surface.read(cx).terminal.clone();
        terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
    })?;
    Ok(())
}

async fn scenario(
    window: WindowHandle<Root>,
    surface: Entity<Surface>,
    nvim: PathBuf,
    socket: PathBuf,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    wait_for(&nvim, &socket, "mode()", "n").await?;
    smol::Timer::after(Duration::from_millis(150)).await;
    let terminal = surface.read_with(cx, |surface, _| surface.terminal.clone());
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| terminal.focus(window, cx))
    })?;
    // Reproduce the old commit API's actual clipboard-paste semantics against
    // the same Neovim process. Its bracketed paste must insert ':' into the
    // buffer, not enter command mode. No mock PTY/echo or source inspection.
    let clipboard = cx.update(|cx| cx.read_from_clipboard());
    cx.update_window(window.into(), |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(":".into()));
        let paste = if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-shift-v"
        };
        window.dispatch_keystroke(Keystroke::parse(paste).unwrap(), cx);
    })?;
    let pasted = wait_for(&nvim, &socket, "getline(1)", ":").await;
    cx.update(|cx| {
        cx.write_to_clipboard(
            clipboard.unwrap_or_else(|| ClipboardItem::new_string(String::new())),
        );
    });
    pasted?;
    let pasted_mode = remote(&nvim, &socket, "mode()").await?;
    anyhow::ensure!(
        pasted_mode != "c",
        "paste unexpectedly entered command mode"
    );
    println!(
        "reproduction: clipboard-paste ':' inserted ':' into Neovim's buffer in mode {pasted_mode:?}, not its command line"
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
    })?;
    wait_for(&nvim, &socket, "mode()", "n").await?;
    remote(&nvim, &socket, "execute('enew!')").await?;
    wait_for(&nvim, &socket, "getline(1)", "").await?;
    // These are native text commits, the exact seam used by OS text/IME input.
    // With the old paste API ':' enters pasted text instead of command mode.
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.replace_text_in_range(None, ":", window, cx);
        })
    })?;
    wait_for(&nvim, &socket, "mode()", "c").await?;
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.replace_text_in_range(None, "echo alpha", window, cx);
        })
    })?;
    wait_for(&nvim, &socket, "getcmdline()", "echo alpha").await?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(Keystroke::parse("ctrl-w").unwrap(), cx);
        assert!(
            !window.has_pending_keystrokes(),
            "workspace intercepted native Ctrl-W"
        );
        assert!(
            terminal.read(cx).focus_handle(cx).is_focused(window),
            "Ctrl-W moved focus out of native Neovim"
        );
        assert!(
            surface.read(cx).navigation.is_empty(),
            "Ctrl-W dispatched pane navigation"
        );
        assert!(
            !surface.read(cx).left,
            "Ctrl-W invoked the explicit escape hatch"
        );
    })?;
    wait_for(&nvim, &socket, "getcmdline()", "echo ").await?;
    navigate_from_native(window, &surface, &nvim, &socket, cx).await?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
    })?;
    wait_for(&nvim, &socket, "mode()", "n").await?;
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.replace_text_in_range(None, "i", window, cx);
        })
    })?;
    wait_for(&nvim, &socket, "mode()", "i").await?;
    for (key, text, modifiers) in [
        (":", ":", Modifiers::default()),
        (
            "g",
            "G",
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        ),
        (
            "e",
            "é",
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
    ] {
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(
                Keystroke {
                    key: key.into(),
                    key_char: Some(text.into()),
                    modifiers,
                },
                cx,
            );
        })?;
    }
    wait_for(&nvim, &socket, "getline(1)", ":Gé").await?;
    // Preedit stays local until committed, then reaches Neovim exactly once.
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.replace_and_mark_text_in_range(None, "界", Some(1..1), window, cx);
        })
    })?;
    anyhow::ensure!(remote(&nvim, &socket, "getline(1)").await? == ":Gé");
    cx.update_window(window.into(), |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.replace_text_in_range(None, "界", window, cx);
        })
    })?;
    wait_for(&nvim, &socket, "getline(1)", ":Gé界").await?;
    navigate_from_native(window, &surface, &nvim, &socket, cx).await?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(Keystroke::parse("secondary-shift-p").unwrap(), cx);
        assert!(!window.has_pending_keystrokes());
        assert!(
            surface
                .read(cx)
                .terminal
                .read(cx)
                .focus_handle(cx)
                .is_focused(window),
            "app shortcut unexpectedly left the terminal"
        );
        window.dispatch_keystroke(Keystroke::parse("secondary-escape").unwrap(), cx);
        assert!(!window.has_pending_keystrokes());
        assert!(surface.read(cx).chrome.is_focused(window));
        assert!(
            !surface
                .read(cx)
                .terminal
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        );
    })?;
    surface.read_with(cx, |surface, _| {
        anyhow::ensure!(surface.palette, "explicit app shortcut did not dispatch");
        anyhow::ensure!(surface.left, "Cmd-Escape did not leave the native input");
        Ok(())
    })?;
    println!(
        "real Ghostty/Neovim: normal-mode colon, command-mode Ctrl-W, all eight direct pane shortcuts in command/insert mode, shifted/Option text, IME commit, app shortcuts and escape passed"
    );
    Ok(())
}

fn main() {
    if std::env::var_os("DROSTJE_NATIVE_INPUT_QA").as_deref() != Some(std::ffi::OsStr::new("1")) {
        println!("native_terminal_input: skipped; opt in with DROSTJE_NATIVE_INPUT_QA=1");
        return;
    }
    let temporary = std::process::Command::new("mktemp")
        .args(["-d", "-t", "drost-native-terminal.XXXXXXXX"])
        .output()
        .unwrap();
    assert!(temporary.status.success());
    let scratch = PathBuf::from(String::from_utf8(temporary.stdout).unwrap().trim_end());
    let nvim = std::env::var_os("DROSTJE_NVIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| "nvim".into());
    let socket = scratch.join("nvim.sock");
    let outcome = Rc::new(RefCell::new(None));
    let result = outcome.clone();
    let cwd = scratch.clone();
    gpui_kit::application()
        .with_assets(assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            drostje::ui::theme::initialize(None, None, cx);
            drostje::ui::commands::bind_keys(cx);
            let mut options = TerminalOptions::new(cwd);
            options.program = Some(nvim.clone());
            options.args = ["--clean", "-n", "-i", "NONE", "--listen"]
                .map(Into::into)
                .to_vec();
            options.args.push(socket.clone().into_os_string());
            let mut surface = None;
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::centered(size(px(800.), px(500.)), cx)),
                        ..WindowOptions::default()
                    },
                    |window, cx| {
                        let view = cx.new(|cx| Surface {
                            terminal: Terminal::spawn(options, window, cx)
                                .expect("spawn real terminal"),
                            chrome: cx.focus_handle(),
                            neighbours: std::array::from_fn(|_| cx.focus_handle()),
                            navigation: Vec::new(),
                            left: false,
                            palette: false,
                        });
                        surface = Some(view.clone());
                        cx.new(|cx| Root::new(view, window, cx))
                    },
                )
                .expect("open native regression window");
            cx.activate(true);
            let surface = surface.unwrap();
            cx.spawn(async move |cx| {
                let report = scenario(window, surface.clone(), nvim, socket, cx).await;
                surface.update(cx, |surface, cx| {
                    surface.terminal.update(cx, |terminal, _| terminal.close())
                });
                *result.borrow_mut() = Some(report);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let _ = std::fs::remove_dir_all(scratch);
    outcome
        .borrow_mut()
        .take()
        .expect("native regression completed")
        .expect("native terminal input regression");
}
