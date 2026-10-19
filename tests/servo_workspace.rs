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
    fn tool(
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
                assert!(response.get("error").is_none(), "{response}");
                return response["result"].clone();
            }
            assert!(Instant::now() < deadline, "Tool timed out");
            pump(cx, window);
        }
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
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::NewBrowser, window, cx)
            })
        })
        .unwrap();
        pump(&mut cx, window);
        screenshot(&mut cx, window, "workspace-tab.png");
        let sessions = state.read_with(&cx, |state, _| {
            state.snapshot.settings["browser_sessions"]
                .as_array()
                .unwrap()
                .len()
        });
        assert_eq!(sessions, 3);
        cx.update_window(window.into(), |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(Command::CloseThread, window, cx)
            })
        })
        .unwrap();
        pump(&mut cx, window);
        assert_eq!(
            state.read_with(&cx, |state, _| state.snapshot.settings["browser_sessions"]
                .as_array()
                .unwrap()
                .len()),
            2,
            "Closing a browser tab must retain the conversation and sidebar"
        );
        assert!(state.read_with(&cx, |state, _| state.open_threads.contains(&thread)));
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
            "Servo workspace proof: HTML tool, separate artifact/sidebar sessions, real evaluate and PNG, keyboard sidebar resize, manual browser tab, close without closing thread"
        );
    }
}
