//! Opt-in real workspace, HTTP tools, Servo sessions and resizable sidebar. No provider turns.
fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(not(target_os = "macos"))]
    println!("servo_workspace: GPUI offscreen renderer unavailable on this platform");
}
#[cfg(target_os = "macos")]
mod macos {
    use drostje::{
        app::{AppState, Bootstrap, SidebarTab},
        browser_tools::BrowserToolServer,
        harness::{ContentBlock, Message, ToolActivity},
        model::{Project, ThreadKind},
        platform,
        store::Store,
        ui::{
            commands::{self, Command},
            theme,
            workspace::{Workspace, WorkspaceRoot},
        },
    };
    use gpui_kit::{assets::AllAssets, component::Root, test::TestWindowExt, *};
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpStream,
        path::Path,
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    use uuid::Uuid;
    fn pump(cx: &mut HeadlessAppContext, window: WindowHandle<Root>) {
        cx.advance_clock(Duration::from_millis(40));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    fn request(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        endpoint: &str,
        thread: Uuid,
        name: &str,
        args: Value,
    ) -> Value {
        let endpoint = url::Url::parse(endpoint).unwrap();
        let body = json!({"thread":thread,"name":name,"arguments":args}).to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", endpoint.port().unwrap())).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(35)))
                .unwrap();
            write!(stream,"POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",endpoint.path(),body.len(),body).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            sender
                .send(
                    serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1)
                        .unwrap(),
                )
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Ok(response) = receiver.try_recv() {
                return response;
            }
            assert!(Instant::now() < deadline, "Tool timed out");
            pump(cx, window);
        }
    }
    fn tool(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        endpoint: &str,
        thread: Uuid,
        name: &str,
        args: Value,
    ) -> Value {
        let response = request(cx, window, endpoint, thread, name, args);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    fn assert_tab_closed(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        state: &Entity<AppState>,
        endpoint: &str,
        thread: Uuid,
        browser: &str,
    ) {
        pump(cx, window);
        let response = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"inspect","id":browser}),
        );
        assert!(response["error"].is_string(), "{response}");
        assert!(response.get("result").is_none(), "{response}");
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.snapshot.settings["browser_sessions"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert!(
                state.snapshot.settings["browser_sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|session| session["id"] != browser)
            );
            assert!(
                state.open_threads.contains(&thread),
                "Close tab must retain Conversation"
            );
            assert_eq!(state.selected_thread, Some(thread));
        });
        cx.update_window(window.into(), |_, window, _| {
            assert!(
                window
                    .try_find(SharedString::from(format!("close-browser-{browser}")))
                    .is_none()
            );
            assert_eq!(
                window
                    .find(SharedString::from(format!("Thread:{thread}")))
                    .focused(),
                Some(true),
                "Closing the active browser must restore keyboard focus to Conversation"
            );
        })
        .unwrap();
    }
    fn screenshot(cx: &mut HeadlessAppContext, window: WindowHandle<Root>, name: &str) {
        if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
            std::fs::create_dir_all(&output).unwrap();
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(Path::new(&output).join(name))
                .unwrap();
        }
    }
    fn assert_browser_pane_navigation(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        endpoint: &str,
        thread: Uuid,
        browser: &str,
        in_sidebar: bool,
    ) {
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate", "id":browser,
                "script":"document.body.innerHTML = '<input id=\"pane-input\" value=\"keep\">'; window.chordKeys = []; for (const type of ['keydown', 'keyup']) document.addEventListener(type, e => window.chordKeys.push(e.key)); true"
            }),
        );
        let panel = SharedString::from(if in_sidebar {
            format!("Thread:{thread}")
        } else {
            format!("Browser:{browser}")
        });
        for page_focused in [false, true] {
            for [left, down, up, right] in [["h", "j", "k", "l"], ["left", "down", "up", "right"]] {
                cx.update_window(window.into(), |_, window, cx| {
                    window.click("servo-surface", cx);
                    window.press("secondary-l", cx);
                    if page_focused {
                        window.click("servo-surface", cx);
                    }
                    assert_eq!(window.find("servo-surface").focused(), Some(page_focused));
                })
                .unwrap();
                tool(
                    cx,
                    window,
                    endpoint,
                    thread,
                    "browser",
                    json!({
                        "action":"evaluate", "id":browser,
                        "script":"{ const input = document.getElementById('pane-input'); input.focus(); input.setSelectionRange(input.value.length, input.value.length); window.chordKeys = []; } true"
                    }),
                );
                cx.update_window(window.into(), |_, window, cx| {
                    // There are no panes above, below or to the right of this browser.
                    for direction in [down, up, right] {
                        window.press(&format!("secondary-alt-{direction}"), cx);
                        assert_eq!(window.find(panel.clone()).focused(), Some(true));
                        assert_eq!(window.find("servo-surface").focused(), Some(page_focused));
                    }
                    window.press(&format!("secondary-alt-{left}"), cx);
                    if in_sidebar {
                        assert_eq!(
                            window.find(panel.clone()).focused(),
                            Some(true),
                            "Browser sidebar must move to Conversation before Projects"
                        );
                        assert_eq!(window.find("servo-surface").focused(), Some(false));
                        window.press(&format!("secondary-alt-{left}"), cx);
                    }
                    assert_eq!(
                        window.find("Projects").focused(),
                        Some(true),
                        "Pane shortcuts must leave both browser address and page focus"
                    );
                    window.press(&format!("secondary-alt-{right}"), cx);
                    if in_sidebar {
                        assert_eq!(
                            window.find(panel.clone()).focused(),
                            Some(true),
                            "Projects must move to Conversation before its browser sidebar"
                        );
                        assert_eq!(window.find("servo-surface").focused(), Some(false));
                        window.press(&format!("secondary-alt-{right}"), cx);
                    }
                    assert_eq!(window.find(panel.clone()).focused(), Some(true));
                    assert_eq!(window.find("servo-surface").focused(), Some(true));
                })
                .unwrap();
                let page = tool(
                    cx,
                    window,
                    endpoint,
                    thread,
                    "browser",
                    json!({
                        "action":"evaluate", "id":browser,
                        "script":"({value:document.getElementById('pane-input').value, keys:window.chordKeys})"
                    }),
                );
                assert_eq!(
                    page,
                    json!({"value":"keep","keys":[]}),
                    "Pane shortcuts must not type into or dispatch key events to the page"
                );
            }
        }
        // Raw Ctrl-W belongs to the native page, not an app prefix. Subsequent bare
        // motion letters must still edit the input rather than move to another pane.
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate", "id":browser,
                "script":"{ const input = document.getElementById('pane-input'); input.value = ''; input.focus(); window.chordKeys = []; } true"
            }),
        );
        cx.update_window(window.into(), |_, window, cx| {
            for key in ["ctrl-w", "h", "j", "k", "l", "backspace"] {
                window.press(key, cx);
                assert_eq!(window.find(panel.clone()).focused(), Some(true));
                assert_eq!(
                    window.find("servo-surface").focused(),
                    Some(true),
                    "Raw Ctrl-W and bare motion letters must not leave the page"
                );
            }
        })
        .unwrap();
        let text = tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate", "id":browser,
                "script":"({value:document.getElementById('pane-input').value, keys:window.chordKeys})"
            }),
        );
        assert_eq!(text["value"], "hjk");
        assert!(
            text["keys"].as_array().unwrap().contains(&json!("w")),
            "Raw Ctrl-W must reach the native page: {text}"
        );
        screenshot(
            cx,
            window,
            if in_sidebar {
                "workspace-browser-sidebar-pane-navigation.png"
            } else {
                "workspace-browser-pane-navigation.png"
            },
        );
    }
    fn artifact_height(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        expected: f32,
    ) -> Bounds<Pixels> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            pump(cx, window);
            let bounds = cx
                .update_window(window.into(), |_, window, _| {
                    window
                        .try_find("artifact-surface")
                        .map(|surface| surface.bounds())
                })
                .unwrap();
            if let Some(bounds) = bounds
                && (f32::from(bounds.size.height) - expected).abs() <= 2.
            {
                return bounds;
            }
            assert!(
                Instant::now() < deadline,
                "Artifact height should be {expected}, got {bounds:?}"
            );
        }
    }

    fn docked_sessions(cx: &mut HeadlessAppContext, state: &Entity<AppState>) -> Vec<String> {
        state.read_with(cx, |state, _| {
            state.snapshot.settings["browser_sessions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|session| session["placement"] == "composer")
                .map(|session| session["id"].as_str().unwrap().to_owned())
                .collect()
        })
    }

    /// Input artifacts dock above the composer through the real tool, one at a time, with a
    /// capped height; the transcript keeps a pointer, and the commands move and focus them.
    fn assert_docked_artifact(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        workspace: &Entity<Workspace>,
        state: &Entity<AppState>,
        endpoint: &str,
        thread: Uuid,
        directory: &Path,
    ) {
        use gpui_kit::component::{Theme, ThemeMode};
        let page = directory.join("ask.html");
        std::fs::write(&page, "<!doctype html><title>Ask</title><main data-drostje-artifact><form data-drostje=\"answer\"><p>Pick a scope</p><label><input type=radio name=scope value=crate checked> crate</label><button class=primary>Send</button></form></main>").unwrap();
        let first = tool(
            cx,
            window,
            endpoint,
            thread,
            "show_artifact",
            json!({"path":page,"title":"Pick a scope","placement":"composer"}),
        );
        assert_eq!(first["placement"], "composer");
        let first_id = first["id"].as_str().unwrap().to_owned();
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session.messages.push(Arc::new(Message {
                id: "docked-qa-message".into(),
                role: "assistant".into(),
                content: vec![ContentBlock::ToolCall {
                    id: "docked-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({"title":"Pick a scope","placement":"composer"}),
                }],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            }));
            session.tools.insert(
                "docked-qa-call".into(),
                Arc::new(ToolActivity {
                    id: "docked-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({}),
                    result: vec![ContentBlock::Text(first.to_string())],
                    error: false,
                    exit_code: None,
                    complete: true,
                    raw: None,
                }),
            );
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        pump(cx, window);
        assert_eq!(docked_sessions(cx, state), [first_id.clone()]);
        // A tall page stays at most 45% of the 900 px window and scrolls inside.
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"evaluate","id":first_id,
                "script":"document.querySelector('main').style.height='2000px'; true"}),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            pump(cx, window);
            let measured = cx
                .update_window(window.into(), |_, window, _| {
                    let surface = window.try_find("docked-artifact-surface")?.bounds();
                    let band = window.find("composer-artifact").bounds();
                    let composer = window.find("composer-row").bounds();
                    Some((surface, band, composer))
                })
                .unwrap();
            if let Some((surface, band, composer)) = measured
                && (f32::from(surface.size.height) - 405.).abs() <= 2.
            {
                assert!(
                    band.bottom() <= composer.top() + px(1.),
                    "the docked page sits above the composer: {band:?} vs {composer:?}"
                );
                break;
            }
            assert!(Instant::now() < deadline, "docked height cap: {measured:?}");
        }
        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                window.find("docked-artifact-stub-docked-qa-call").visible(),
                "transcript keeps a pointer"
            );
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::FocusComposerArtifact, window, cx)
            });
        })
        .unwrap();
        pump(cx, window);
        cx.update_window(window.into(), |_, window, _| {
            assert_eq!(window.find("docked-artifact-surface").focused(), Some(true));
        })
        .unwrap();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update_window(window.into(), |_, window, cx| {
                Theme::change(mode, Some(window), cx);
            })
            .unwrap();
            pump(cx, window);
            screenshot(
                cx,
                window,
                &format!(
                    "workspace-docked-artifact-{}.png",
                    if mode == ThemeMode::Light {
                        "light"
                    } else {
                        "dark"
                    }
                ),
            );
        }

        // Docking another moves the first into the transcript; it stays live.
        let second = tool(
            cx,
            window,
            endpoint,
            thread,
            "show_artifact",
            json!({"path":page,"title":"Follow-up","placement":"composer"}),
        );
        let second_id = second["id"].as_str().unwrap().to_owned();
        pump(cx, window);
        assert_eq!(docked_sessions(cx, state), [second_id.clone()]);
        let alive = tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"evaluate","id":first_id,"script":"document.title"}),
        );
        assert_eq!(alive, "Ask", "the undocked page keeps its document");

        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::MoveArtifactIntoConversation, window, cx)
            });
        })
        .unwrap();
        pump(cx, window);
        assert!(docked_sessions(cx, state).is_empty());
        cx.update_window(window.into(), |_, window, _| {
            assert!(window.try_find("composer-artifact").is_none());
            assert!(
                window
                    .try_find("docked-artifact-stub-docked-qa-call")
                    .is_none()
            );
        })
        .unwrap();
        for id in [first_id, second_id] {
            tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({"action":"close","id":id}),
            );
        }
        pump(cx, window);
    }

    fn sessions(cx: &mut HeadlessAppContext, state: &Entity<AppState>) -> Vec<Value> {
        state.read_with(cx, |state, _| {
            state.snapshot.settings["browser_sessions"]
                .as_array()
                .unwrap()
                .clone()
        })
    }

    /// A docked form's answer travels the real path (kit, Servo, app outbox, native command)
    /// and the page stays open for corrections. Only the agent's complete_artifact call,
    /// through the real tool endpoint, collapses it, marks it answered, frees the dock and
    /// hands focus back to the composer.
    fn assert_agent_completes_artifact(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        workspace: &Entity<Workspace>,
        state: &Entity<AppState>,
        endpoint: &str,
        thread: Uuid,
        directory: &Path,
    ) {
        use drostje::harness::{
            ConversationCommand, ConversationEvent, ConversationState, PeerDeliveryOutcome,
        };
        // A stand-in native runtime: commands land here, outcomes go back as harness events.
        let (commands, native) = async_channel::bounded(8);
        let generation = state.update(cx, |state, _| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session.commands = Some(commands);
            session.state = ConversationState::Ready;
            session.persisted = true;
            session.peer_delivery = true;
            session.requests.clear();
            // As a runtime that reported Ready and is idle.
            session.preparing = false;
            let generation = session.generation;
            state.active_threads.remove(&thread);
            generation
        });
        // Accept the answer, then act out the short turn it woke: start, settle, idle.
        let accept = |cx: &mut HeadlessAppContext, attempt| {
            let events = state.read_with(cx, |state, _| state.events.clone());
            for event in [
                ConversationEvent::PeerDelivery {
                    attempt,
                    outcome: PeerDeliveryOutcome::Accepted,
                },
                ConversationEvent::PeerTurnStarted {
                    request_id: attempt,
                },
                ConversationEvent::TurnSettled {
                    request_id: attempt,
                    outcome: drostje::harness::TurnOutcome::Completed,
                },
                ConversationEvent::StateChanged(ConversationState::Ready),
            ] {
                events
                    .try_send(drostje::app::AppEvent::Harness(thread, generation, event))
                    .unwrap();
            }
        };
        let submit = |cx: &mut HeadlessAppContext, id: &str| {
            tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({"action":"evaluate","id":id,"script":"document.querySelector('button').click(); true"}),
            );
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                pump(cx, window);
                if let Ok(ConversationCommand::DeliverPeer(delivery)) = native.try_recv() {
                    return delivery;
                }
                if Instant::now() >= deadline {
                    let page = tool(
                        cx,
                        window,
                        endpoint,
                        thread,
                        "browser",
                        json!({"action":"evaluate","id":id,
                            "script":"(document.querySelector('[data-drostje-status]') || {}).textContent || 'no status'"}),
                    );
                    let session = state.read_with(cx, |state, _| {
                        let session = &state.conversations[&thread];
                        format!(
                            "state {:?}, commands {}, preparing {}, requests {}",
                            session.state,
                            session.commands.is_some(),
                            session.preparing,
                            session.requests.len()
                        )
                    });
                    panic!("the answer never reached the runtime: page {page}, {session}");
                }
            }
        };

        // The user answers; the agent decides when it has what it needs.
        let page = directory.join("ask.html");
        let shown = tool(
            cx,
            window,
            endpoint,
            thread,
            "show_artifact",
            json!({"path":page,"title":"Pick a scope","placement":"composer"}),
        );
        let id = shown["id"].as_str().unwrap().to_owned();
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session.messages.push(Arc::new(Message {
                id: "answered-qa-message".into(),
                role: "assistant".into(),
                content: vec![ContentBlock::ToolCall {
                    id: "answered-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({"title":"Pick a scope","placement":"composer"}),
                }],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            }));
            session.tools.insert(
                "answered-qa-call".into(),
                Arc::new(ToolActivity {
                    id: "answered-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({}),
                    result: vec![ContentBlock::Text(shown.to_string())],
                    error: false,
                    exit_code: None,
                    complete: true,
                    raw: None,
                }),
            );
            cx.notify();
        });
        pump(cx, window);
        assert_eq!(docked_sessions(cx, state), [id.clone()]);
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::FocusComposerArtifact, window, cx)
            });
        })
        .unwrap();
        pump(cx, window);
        let delivery = submit(cx, &id);
        assert!(delivery.messages[0].body.contains(r#""type":"answer""#));
        accept(cx, delivery.attempt);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            pump(cx, window);
            let page = tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({"action":"evaluate","id":id,"script":"document.querySelector('form').dataset.drostjeState"}),
            );
            if page == "sent" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the page hears its answer arrived: {page}"
            );
        }
        assert_eq!(
            docked_sessions(cx, state),
            [id.clone()],
            "a delivered answer leaves the page open for corrections"
        );

        // Only this Conversation's own artifacts can be completed.
        let stranger = request(
            cx,
            window,
            endpoint,
            thread,
            "complete_artifact",
            json!({"id":Uuid::new_v4()}),
        );
        assert!(
            stranger
                .to_string()
                .contains("not an open artifact of this Conversation"),
            "{stranger}"
        );
        let completed = tool(
            cx,
            window,
            endpoint,
            thread,
            "complete_artifact",
            json!({"id":id}),
        );
        assert_eq!(completed["completed"], true);
        assert!(docked_sessions(cx, state).is_empty());
        pump(cx, window);
        let record = sessions(cx, state)
            .into_iter()
            .find(|session| session["id"] == id.as_str())
            .unwrap();
        assert_eq!(record["answered"], true, "completion survives a restart");
        assert_eq!(record["placement"], "conversation");
        cx.update_window(window.into(), |_, window, _| {
            assert!(window.try_find("composer-artifact").is_none());
            assert!(window.find("artifact-answered").visible());
            assert!(
                window.try_find("docked-artifact-surface").is_none(),
                "the answered page is collapsed, not docked"
            );
            assert_eq!(
                window.find("prompt-editor").focused(),
                Some(true),
                "focus returns to the composer"
            );
        })
        .unwrap();
        screenshot(cx, window, "workspace-answered-artifact.png");
        let page_state = tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"evaluate","id":id,"script":"document.querySelector('form').dataset.drostjeState"}),
        );
        assert_eq!(page_state, "sent", "the page was told its answer arrived");
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"close","id":id}),
        );
        pump(cx, window);
    }

    fn assert_inline_artifact(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        workspace: &Entity<Workspace>,
        state: &Entity<AppState>,
        endpoint: &str,
        thread: Uuid,
        artifact: &Value,
    ) {
        use gpui_kit::component::{Theme, ThemeMode};
        let id = artifact["id"].as_str().unwrap();
        // The result is from the real HTTP tool and real Servo session. Populate the
        // component's input directly; no harness process or fabricated provider turn.
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session.messages.push(Arc::new(Message {
                id: "artifact-qa-message".into(),
                role: "assistant".into(),
                content: vec![ContentBlock::ToolCall {
                    id: "artifact-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({"title":"HTML artifact"}),
                }],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            }));
            session.tools.insert(
                "artifact-qa-call".into(),
                Arc::new(ToolActivity {
                    id: "artifact-qa-call".into(),
                    name: "show_artifact".into(),
                    intent: None,
                    input: json!({}),
                    result: vec![ContentBlock::Text(artifact.to_string())],
                    error: false,
                    exit_code: None,
                    complete: true,
                    raw: None,
                }),
            );
            // A generation change makes the production transcript consume fixture rows.
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        pump(cx, window);
        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                window.find("browser-address-toolbar").visible(),
                "Sidebar keeps its address bar"
            );
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::ToggleBrowser, window, cx)
            });
        })
        .unwrap();
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate","id":id,
                "script":"document.body.style.margin='0'; document.body.innerHTML='<main data-drostje-artifact style=\"height:220px;width:100%\">Retained artifact</main>'; window.artifactState=41; true"
            }),
        );
        artifact_height(cx, window, 220.);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update_window(window.into(), |_, window, cx| {
                Theme::change(mode, Some(window), cx);
            })
            .unwrap();
            pump(cx, window);
            cx.update_window(window.into(), |_, window, cx| {
                assert!(
                    window.try_find("browser-address-toolbar").is_none(),
                    "Inline artifacts have no URL bar"
                );
                assert!(
                    window.try_find("browser-back").is_none(),
                    "Inline navigation starts hidden"
                );
                assert!(window.find("artifact-header").bounds().size.height < px(40.));
                window.click("artifact-disclosure", cx);
            })
            .unwrap();
            pump(cx, window);
            cx.update_window(window.into(), |_, window, cx| {
                assert!(
                    window.try_find("artifact-surface").is_none(),
                    "Collapsed surfaces have no hit/input handlers"
                );
                assert_eq!(
                    window.find("artifact-disclosure-focus").focused(),
                    Some(true)
                );
                window.press("enter", cx);
            })
            .unwrap();
            artifact_height(cx, window, 220.);
            let retained = tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({
                    "action":"evaluate","id":id,"script":"++window.artifactState"
                }),
            );
            assert_eq!(
                retained.as_f64(),
                if mode == ThemeMode::Light {
                    Some(42.)
                } else {
                    Some(43.)
                }
            );
            cx.update_window(window.into(), |_, window, cx| {
                // Leave-editor focuses the disclosure; Tab traverses the native buttons.
                window.click("artifact-surface", cx);
                window.press("secondary-escape", cx);
                for _ in 0..30 {
                    if window.find("artifact-navigation").focused() == Some(true) {
                        break;
                    }
                    window.press("tab", cx);
                }
                assert_eq!(window.find("artifact-navigation").focused(), Some(true));
                window.press("enter", cx);
            })
            .unwrap();
            pump(cx, window);
            cx.update_window(window.into(), |_, window, cx| {
                // Escape dismisses a keyboard-opened native navigation menu.
                assert!(
                    window.find("popup-menu").visible(),
                    "Navigation is reachable using native keyboard traversal"
                );
                window.press("escape", cx);
            })
            .unwrap();
            screenshot(
                cx,
                window,
                if mode == ThemeMode::Light {
                    "artifact-inline-light.png"
                } else {
                    "artifact-inline-dark.png"
                },
            );
        }
        for height in [410., 150.] {
            tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({
                    "action":"evaluate","id":id,
                    "script":format!("document.querySelector('main').style.height='{height}px'; true")
                }),
            );
            artifact_height(cx, window, height);
        }
        // Width changes must reflow the page and invalidate the actual transcript row.
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate","id":id,
                "script":"document.querySelector('main').style.height='calc(800px - 50vw)'; true"
            }),
        );
        let dimensions = |cx: &mut HeadlessAppContext| {
            tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({
                    "action":"evaluate","id":id,
                    "script":"({width:innerWidth,height:document.querySelector('main').getBoundingClientRect().height})"
                }),
            )
        };
        let wide = dimensions(cx);
        artifact_height(cx, window, wide["height"].as_f64().unwrap() as f32);
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::ToggleBrowser, window, cx)
            });
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            pump(cx, window);
            let narrow = dimensions(cx);
            if narrow["width"].as_f64() < wide["width"].as_f64() {
                artifact_height(cx, window, narrow["height"].as_f64().unwrap() as f32);
                assert!(narrow["height"].as_f64() > wide["height"].as_f64());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Sidebar must reflow inline artifact"
            );
        }
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::ToggleBrowser, window, cx)
            });
        })
        .unwrap();
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate","id":id,
                "script":"document.querySelector('main').style.height='5000px'; true"
            }),
        );
        artifact_height(cx, window, 1600.);
        let long_extent = workspace.read_with(cx, |workspace, cx| {
            workspace.artifact_transcript_scroll_extent(thread, cx)
        });
        assert!(
            long_extent > 500.,
            "Virtualized list must remeasure the growing artifact row: {long_extent}"
        );
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"scroll","id":id,"x":30,"y":30,"dx":0,"dy":10000
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let bottom = tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({
                    "action":"evaluate","id":id,"script":"scrollY + innerHeight >= document.documentElement.scrollHeight - 2"
                }),
            );
            if bottom == true {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Capped artifact content must remain reachable by scrolling"
            );
            pump(cx, window);
        }
        cx.update_window(window.into(), |_, window, cx| {
            // Tail following clips the center of this 1600px row above the transcript.
            // Click its visible lower edge through native hit testing, not that center.
            let surface = window.find("artifact-surface").bounds();
            window.click_at(
                "artifact-surface",
                point(surface.size.width / 2., surface.size.height - px(20.)),
                cx,
            );
            assert_eq!(window.find("artifact-surface").focused(), Some(true));
            window.press("secondary-shift-a", cx);
        })
        .unwrap();
        pump(cx, window);
        let collapsed_extent = workspace.read_with(cx, |workspace, cx| {
            workspace.artifact_transcript_scroll_extent(thread, cx)
        });
        cx.update_window(window.into(), |_, window, _| {
            assert!(window.try_find("artifact-surface").is_none());
            assert_eq!(
                window.find("artifact-disclosure-focus").focused(),
                Some(true)
            );
        })
        .unwrap();
        assert_eq!(
            collapsed_extent, 0.,
            "Collapsed single-row artifact must leave no cached transcript scroll extent"
        );
        cx.update_window(window.into(), |_, window, cx| window.press("space", cx))
            .unwrap();
        artifact_height(cx, window, 1600.);
        tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({
                "action":"evaluate","id":id,"script":"document.querySelector('main').style.height='180px'; true"
            }),
        );
        artifact_height(cx, window, 180.);
        let short_extent = workspace.read_with(cx, |workspace, cx| {
            workspace.artifact_transcript_scroll_extent(thread, cx)
        });
        assert_eq!(
            short_extent, 0.,
            "Shrinking to a fitted single-row artifact must clear the cached scroll extent"
        );
        // Evict a collapsed row from the real virtualized transcript and reattach it.
        cx.update_window(window.into(), |_, window, cx| {
            window.click("artifact-disclosure", cx)
        })
        .unwrap();
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            for index in 0..80 {
                session.messages.push(Arc::new(Message {
                    id: format!("artifact-qa-filler-{index}"),
                    role: "user".into(),
                    content: vec![ContentBlock::Text(format!(
                        "Transcript filler {index}\n\n{}",
                        "A real virtualized row. ".repeat(40)
                    ))],
                    raw: None,
                    tool_call_id: None,
                    error: false,
                    exit_code: None,
                }));
            }
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        pump(cx, window);
        cx.update_window(window.into(), |_, window, _| {
            assert!(
                window.try_find("artifact-header").is_none(),
                "The artifact row must actually leave the render window"
            );
        })
        .unwrap();
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session
                .messages
                .retain(|message| !message.id.starts_with("artifact-qa-filler-"));
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        pump(cx, window);
        cx.update_window(window.into(), |_, window, cx| {
            assert!(window.find("artifact-header").visible());
            assert!(
                window.try_find("artifact-surface").is_none(),
                "Row virtualization must not reset collapse state"
            );
            window.click("artifact-disclosure", cx);
        })
        .unwrap();
        artifact_height(cx, window, 180.);
        assert_eq!(
            tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({
                    "action":"evaluate","id":id,"script":"window.artifactState"
                })
            )
            .as_f64(),
            Some(43.),
            "Row virtualization must not recreate the WebView"
        );
        // The hidden navigation menu uses the same real reload Action as browser tabs.
        cx.update_window(window.into(), |_, window, cx| {
            window.click("artifact-navigation", cx);
            window.press("up", cx); // Select the last menu item, Reload.
            window.press("enter", cx);
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let snapshot = tool(
                cx,
                window,
                endpoint,
                thread,
                "browser",
                json!({"action":"inspect", "id":id}),
            );
            let restored = if snapshot["loading"] == false {
                tool(
                    cx,
                    window,
                    endpoint,
                    thread,
                    "browser",
                    json!({
                        "action":"evaluate", "id":id,
                        "script":"document.querySelector('h1')?.textContent === 'Real Servo workspace' && typeof window.artifactState === 'undefined'"
                    }),
                )
            } else {
                Value::Bool(false)
            };
            if restored == true {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Artifact menu must reload its own real page"
            );
            pump(cx, window);
        }
        state.update(cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session
                .messages
                .retain(|message| message.id != "artifact-qa-message");
            session.tools.remove("artifact-qa-call");
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::ToggleBrowser, window, cx)
            });
        })
        .unwrap();
        pump(cx, window);
    }
    fn assert_computer_use(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        endpoint: &str,
        thread: Uuid,
        directory: &Path,
    ) {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use std::sync::atomic::{AtomicBool, Ordering};
        const PAGE: &str = r#"<!doctype html><title>Automation workflow</title>
        <style>body{margin:0;padding:20px;font:16px sans-serif;background:#fff}
        input,button{display:block;margin:8px 0;height:32px}#drag{width:180px;height:50px;background:#ddd}
        #footer{margin-top:1200px}</style>
        <form><label for="name">Project name</label><input id="name"><button id="save">Save project</button></form>
        <p id="status" role="status">Not saved</p><button id="disabled" disabled>Unavailable</button>
        <input type="password" aria-label="Password" value="secret-not-observed">
        <div id="drag" role="slider" tabindex="0" aria-label="Drag control">Drag here</div>
        <div id="footer">Bottom of page</div>
        <script>
        window.submissions=0;window.hovered=false;window.dragged=false;window.trusted=[];
        document.querySelector('form').onsubmit=e=>{e.preventDefault();submissions++;trusted.push(e.isTrusted);setTimeout(()=>{document.querySelector('#status').textContent='Saved '+document.querySelector('#name').value},150)};
        const drag=document.querySelector('#drag');let down=false;
        drag.onmouseenter=()=>window.hovered=true;
        drag.onmousedown=e=>{down=true;trusted.push(e.isTrusted)};
        document.addEventListener('mousemove',e=>{if(down&&e.clientX>150){window.dragged=true;trusted.push(e.isTrusted)}});
        document.addEventListener('mouseup',()=>down=false);
        console.error('automation console evidence');
        fetch('/failed-fetch').then(()=>window.fetchFinished=true);
        const xhr=new XMLHttpRequest();xhr.open('GET','/failed-xhr');xhr.onload=()=>window.xhrFinished=true;xhr.send();
        </script><img src="/missing-image" alt=""></html>"#;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 4096];
                        while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                            match stream.read(&mut buffer) {
                                Ok(0) | Err(_) => break,
                                Ok(length) => bytes.extend_from_slice(&buffer[..length]),
                            }
                        }
                        let request = String::from_utf8_lossy(&bytes);
                        let path = request.split_whitespace().nth(1).unwrap_or("");
                        let (status, body) = if path == "/" {
                            ("200 OK", PAGE)
                        } else {
                            ("503 Service Unavailable", "Intentional diagnostic response")
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("Local browser fixture failed: {error}"),
                }
            }
        });
        let opened = tool(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"open","url":url,"placement":"tab"}),
        );
        let id = opened["id"].as_str().unwrap();
        let call = |cx: &mut HeadlessAppContext, mut args: Value| {
            args["id"] = json!(id);
            tool(cx, window, endpoint, thread, "browser", args)
        };
        call(cx, json!({"action":"wait","timeout_ms":20000}));
        let fixed = call(
            cx,
            json!({"action":"viewport","width":640,"height":480,"scale":2}),
        );
        assert_eq!(fixed["viewport"]["width"].as_f64(), Some(640.));
        let observed = call(cx, json!({"action":"observe"}));
        assert!(!observed.to_string().contains("secret-not-observed"));
        let target = |observation: &Value, label: &str| {
            observation["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["label"] == label)
                .unwrap()["target"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let name = target(&observed, "Project name");
        let drag = target(&observed, "Drag control");
        let disabled = target(&observed, "Unavailable");
        let rejected = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"click","id":id,"target":disabled}),
        );
        assert!(rejected["error"].is_string(), "{rejected}");
        call(
            cx,
            json!({"action":"evaluate","script":"const cover=document.createElement('div');cover.id='cover';cover.style='position:fixed;inset:0;z-index:999;background:white';document.body.append(cover);true"}),
        );
        let covered = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"click","id":id,"target":name}),
        );
        assert!(covered["error"].is_string(), "{covered}");
        call(
            cx,
            json!({"action":"evaluate","script":"document.querySelector('#cover').remove();true"}),
        );
        let delivered = call(cx, json!({"action":"click","target":name}));
        assert!(delivered.get("queued").is_none(), "{delivered}");
        call(cx, json!({"action":"type","text":"Verified UI"}));
        call(cx, json!({"action":"key","key":"Enter","code":"Enter"}));
        call(
            cx,
            json!({"action":"wait","script":"document.querySelector('#status').textContent === 'Saved Verified UI'"}),
        );
        call(cx, json!({"action":"hover","target":drag}));
        call(
            cx,
            json!({"action":"wait","script":"window.hovered === true"}),
        );
        let bounds = call(
            cx,
            json!({"action":"evaluate","script":"(()=>{const r=document.querySelector('#drag').getBoundingClientRect();return {x:r.x,y:r.y}})()"}),
        );
        call(
            cx,
            json!({"action":"drag","x":bounds["x"].as_f64().unwrap()+10.,
            "y":bounds["y"].as_f64().unwrap()+20.,"to_x":190.,
            "to_y":bounds["y"].as_f64().unwrap()+20.}),
        );
        call(
            cx,
            json!({"action":"wait","script":"window.dragged === true && window.trusted.every(Boolean)"}),
        );
        let before = call(cx, json!({"action":"inspect"}));
        call(
            cx,
            json!({"action":"evaluate","script":"document.body.style.background='rgb(12,34,56)'; true"}),
        );
        let png = directory.join("computer-use.png");
        let shot = call(
            cx,
            json!({"action":"screenshot","path":png,"after_frame":before["frame"]["revision"]}),
        );
        let bytes = STANDARD
            .decode(shot["image"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(bytes, std::fs::read(&png).unwrap());
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (1280, 960));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let shot = call(cx, json!({"action":"screenshot"}));
            let bytes = STANDARD
                .decode(shot["image"]["data"].as_str().unwrap())
                .unwrap();
            let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
            if image.get_pixel(1200, 900).0 == [12, 34, 56, 255] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Screenshot must observe actual updated pixels"
            );
        }
        screenshot(cx, window, "computer-use-fixed.png");
        let stale_frame = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"screenshot","id":id,"after_frame":u64::MAX,"timeout_ms":100}),
        );
        assert!(stale_frame["error"].is_string(), "{stale_frame}");
        let diagnostics = call(cx, json!({"action":"diagnostics"}));
        let encoded = diagnostics.to_string();
        assert!(
            encoded.contains("automation console evidence"),
            "{diagnostics}"
        );
        assert!(encoded.contains("failed-fetch"), "{diagnostics}");
        assert!(encoded.contains("failed-xhr"), "{diagnostics}");
        assert!(encoded.contains("503"), "{diagnostics}");
        let refreshed = call(cx, json!({"action":"observe","limit":1}));
        assert_eq!(refreshed["truncated"], true);
        let stale = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"click","id":id,"target":name}),
        );
        assert!(stale["error"].is_string(), "{stale}");
        let timeout = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"wait","id":id,"script":"false","timeout_ms":100}),
        );
        assert!(timeout["error"].is_string(), "{timeout}");
        let count = call(
            cx,
            json!({"action":"evaluate","script":"window.submissions"}),
        );
        assert_eq!(
            count.as_f64(),
            Some(1.),
            "A condition timeout must not replay submission"
        );
        call(
            cx,
            json!({"action":"scroll","x":400,"y":400,"dx":0,"dy":500}),
        );
        call(cx, json!({"action":"wait","script":"scrollY > 0"}));
        let pane = call(cx, json!({"action":"viewport","auto":true}));
        assert_eq!(pane["viewport_mode"], "pane");
        let document = call(cx, json!({"action":"inspect"}))["document_revision"].clone();
        call(cx, json!({"action":"reload"}));
        call(cx, json!({"action":"wait","timeout_ms":20000}));
        let reloaded = call(cx, json!({"action":"inspect"}));
        assert!(reloaded["document_revision"].as_u64().unwrap() > document.as_u64().unwrap());
        let old_document = request(
            cx,
            window,
            endpoint,
            thread,
            "browser",
            json!({"action":"click","id":id,"target":name}),
        );
        assert!(old_document["error"].is_string(), "{old_document}");
        call(cx, json!({"action":"close"}));
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        println!(
            "Computer-use proof: real HTTP, named targets, trusted form/hover/drag input, condition waits, fixed 2x viewport, image pixels, console and HTTP failures, stale targets, timeout without replay, scroll and pane restoration"
        );
    }

    pub fn run() {
        let directory =
            std::env::temp_dir().join(format!("drostje-servo-workspace-{}", Uuid::new_v4()));
        let repo = directory.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(&repo)
                .status()
                .unwrap()
                .success()
        );
        let html = directory.join("artifact.html");
        std::fs::write(&html,"<!doctype html><title>Workspace Servo</title><style>body{background:#173629;color:white}</style><h1>Real Servo workspace</h1><button onclick='document.title=\"Clicked from tool\"'>Click</button>").unwrap();
        let data = directory.join("data");
        let store = Store::open(data.clone()).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Browser QA".into(),
            root: repo.clone(),
            archived: false,
            defaults: Default::default(),
        };
        smol::block_on(store.save_project(project.clone())).unwrap();
        smol::block_on(
            store.set_setting("factory_data_root".into(), json!(directory.join("factory"))),
        )
        .unwrap();
        // No real harness may start here: a live runtime could take an injected artifact
        // answer and start a provider turn. The open fails visibly instead.
        smol::block_on(store.set_setting(
            "executables".into(),
            json!({"omp": directory.join("no-harness/omp")}),
        ))
        .unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let server = BrowserToolServer::start().unwrap();
        let endpoint = server.endpoint();
        let bootstrap = Bootstrap {
            data_dir: data,
            store: store.clone(),
            snapshot,
            diagnostics: BTreeMap::new(),
            repositories: BTreeMap::from([(
                project.id,
                Ok(platform::inspect_repository(&repo).unwrap()),
            )]),
            browser_tools: Some(server),
        };
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(AllAssets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::initialize(None, None, cx);
            commands::bind_keys(cx);
        });
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let thread = state.update(&mut cx, |state, _| {
            state.select_project(project.id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            state.selected_thread.unwrap()
        });
        let mut workspace = None;
        let window = cx
            .open_window(size(px(1440.), px(900.)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(state.clone(), window, cx));
                workspace = Some(view.clone());
                let root = cx.new(|_| WorkspaceRoot { workspace: view });
                cx.new(|cx| Root::new(root, window, cx))
            })
            .unwrap();
        let workspace = workspace.unwrap();
        pump(&mut cx, window);
        let artifact = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "show_artifact",
            json!({"path":html,"title":"HTML artifact"}),
        );
        let artifact_id = artifact["id"].as_str().unwrap().to_owned();
        assert_eq!(
            Path::new(artifact["path"].as_str().unwrap()),
            std::fs::canonicalize(&html).unwrap()
        );
        let opened = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"open","url":url::Url::from_file_path(&html).unwrap().as_str(),"placement":"sidebar"}),
        );
        let sidebar = opened["id"].as_str().unwrap().to_owned();
        assert_ne!(sidebar, artifact_id);
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let state = tool(
                &mut cx,
                window,
                &endpoint,
                thread,
                "browser",
                json!({"action":"inspect","id":sidebar}),
            );
            assert!(state["error"].is_null(), "{state}");
            if state["loading"] == false && state["title"] == "Workspace Servo" {
                break;
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(
            state.read_with(&cx, |state, _| state.thread_sidebar_tab(thread)),
            SidebarTab::Browser
        );
        pump(&mut cx, window);
        assert_inline_artifact(
            &mut cx, window, &workspace, &state, &endpoint, thread, &artifact,
        );
        screenshot(&mut cx, window, "workspace-sidebar.png");
        let before = state.read_with(&cx, |state, _| state.thread_sidebar_width(thread));
        let before_bounds = cx
            .update_window(window.into(), |_, window, _| {
                window.find("servo-surface").bounds()
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::WidenSidebar, window, cx)
            })
        })
        .unwrap();
        pump(&mut cx, window);
        assert!(
            state.read_with(&cx, |state, _| state.thread_sidebar_width(thread)) > before,
            "Keyboard sidebar resize must persist the actual width"
        );
        let after_bounds = cx
            .update_window(window.into(), |_, window, _| {
                window.find("servo-surface").bounds()
            })
            .unwrap();
        assert!(
            after_bounds.size.width > before_bounds.size.width,
            "Keyboard resize must change the visible surface"
        );
        screenshot(&mut cx, window, "workspace-sidebar-wide.png");
        let result = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"evaluate","id":sidebar,"script":"document.querySelector('button').click(); document.title"}),
        );
        assert_eq!(result, "Clicked from tool");
        let artifact_title = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"evaluate","id":artifact_id,"script":"document.title"}),
        );
        assert_eq!(
            artifact_title, "Workspace Servo",
            "Artifact and sidebar retain distinct pages"
        );
        let repeated = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "show_artifact",
            json!({"path":html}),
        );
        assert_ne!(
            repeated["id"], artifact["id"],
            "Separate tool cards must not render the same retained GPUI entity"
        );
        tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"close","id":repeated["id"]}),
        );
        assert_eq!(
            state.read_with(&cx, |state, _| state.thread_sidebar_tab(thread)),
            SidebarTab::Browser,
            "Closing an artifact must not hide a different browser session"
        );
        assert_docked_artifact(
            &mut cx, window, &workspace, &state, &endpoint, thread, &directory,
        );
        assert_agent_completes_artifact(
            &mut cx, window, &workspace, &state, &endpoint, thread, &directory,
        );
        let png = directory.join("tool.png");
        let saved = tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"screenshot","id":sidebar,"path":png}),
        );
        let image = image::open(&png).unwrap();
        assert_eq!(image.width(), saved["width"].as_u64().unwrap() as u32);
        assert_browser_pane_navigation(&mut cx, window, &endpoint, thread, &sidebar, true);
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::NewBrowser, window, cx)
            })
        })
        .unwrap();
        pump(&mut cx, window);
        screenshot(&mut cx, window, "workspace-tab.png");
        let browser = state.read_with(&cx, |state, _| {
            let sessions = state.snapshot.settings["browser_sessions"]
                .as_array()
                .unwrap();
            assert_eq!(sessions.len(), 3);
            sessions
                .iter()
                .find(|session| session["thread"].is_null())
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        });
        assert_browser_pane_navigation(&mut cx, window, &endpoint, thread, &browser, false);
        cx.update_window(window.into(), |_, window, cx| {
            let close = SharedString::from(format!("close-browser-{browser}"));
            assert!(
                window.find(close.clone()).visible(),
                "Browser close must not require hover"
            );
            window.click(close, cx);
        })
        .unwrap();
        assert_tab_closed(&mut cx, window, &state, &endpoint, thread, &browser);
        screenshot(&mut cx, window, "workspace-tab-closed.png");

        // The same Close tab action must route through both nested native-input
        // focus scopes, not just a direct call to Workspace::execute.
        for page_focused in [false, true] {
            cx.update_window(window.into(), |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.execute(Command::NewBrowser, window, cx)
                })
            })
            .unwrap();
            pump(&mut cx, window);
            let browser = state.read_with(&cx, |state, _| {
                state.snapshot.settings["browser_sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|session| session["thread"].is_null())
                    .unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
            cx.update_window(window.into(), |_, window, cx| {
                if page_focused {
                    window.click("servo-surface", cx);
                }
                assert_eq!(window.find("servo-surface").focused(), Some(page_focused));
                assert_eq!(
                    window
                        .find(SharedString::from(format!("Browser:{browser}")))
                        .focused(),
                    Some(true)
                );
                window.press(commands::CLOSE_THREAD_KEY, cx);
            })
            .unwrap();
            assert_tab_closed(&mut cx, window, &state, &endpoint, thread, &browser);
        }
        assert_computer_use(&mut cx, window, &endpoint, thread, &directory);
        tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"close","id":sidebar}),
        );
        assert_eq!(
            state.read_with(&cx, |state, _| state.thread_sidebar_tab(thread)),
            SidebarTab::Changes
        );
        tool(
            &mut cx,
            window,
            &endpoint,
            thread,
            "browser",
            json!({"action":"close","id":artifact_id}),
        );
        state.update(&mut cx, |state, cx| state.begin_shutdown(false, cx));
        for _ in 0..10 {
            pump(&mut cx, window);
        }
        drop(workspace);
        drop(state);
        drop(cx);
        smol::block_on(store.flush()).unwrap();
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
        println!(
            "Servo workspace proof: HTML tool, separate artifact/sidebar sessions, real evaluate and PNG, keyboard sidebar resize, direct address/page pane navigation with Option-Command letters and arrows, browser-sidebar/Conversation/Projects focus order, preserved page input and native Ctrl-W, rendered browser tab close, address/page keyboard close, runtime removal and Conversation focus restoration"
        );
    }
}
