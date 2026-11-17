use super::{NetworkPanel, View, delivery_label};
use crate::{
    app::{AppState, Bootstrap},
    model::{Project, ProjectDefaults, ThreadKind},
    network::{Action, DeliveryState, InboundPolicy, Network, NetworkSnapshot, Peer, Priority},
    store::Store,
    ui::workspace::Workspace,
};
use gpui_kit::{
    AppContext, Focusable, TestAppContext,
    component::{IndexPath, Root, Theme, ThemeMode},
    test::TestWindowExt,
};
use std::sync::Arc;
use uuid::Uuid;

#[gpui_kit::test]
fn coordination_states_render_and_messages_remain_keyboard_accessible(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let directory =
        std::env::temp_dir().join(format!("drostje-coordination-ui-{}", Uuid::new_v4()));
    let store = Store::open(directory.join("ui")).unwrap();
    let project = Project {
        id: Uuid::new_v4(),
        name: "Coordination QA".into(),
        root: directory.join("repo"),
        archived: false,
        defaults: ProjectDefaults::default(),
    };
    let project_id = project.id;
    smol::block_on(store.save_project(project)).unwrap();
    let snapshot = smol::block_on(store.snapshot()).unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::theme::initialize(None, Some(&directory), cx);
        crate::ui::commands::bind_keys(cx);
    });
    let state = cx.new(|cx| {
        AppState::new(
            Bootstrap {
                data_dir: directory.clone(),
                store,
                snapshot,
                diagnostics: Default::default(),
                repositories: Default::default(),
                browser_tools: None,
            },
            cx,
        )
    });
    let mut workspace = None;
    let mut panel = None;
    let mut ids = None;
    let window = cx.add_window(|window, cx| {
        let owner = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let (a, b, space) = state.update(cx, |state, _| {
            state.select_project(project_id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            let a = state.selected_thread.unwrap();
            state.new_thread(ThreadKind::Conversation).unwrap();
            let b = state.selected_thread.unwrap();
            state.selected_thread = None;
            state.open_threads.clear();
            (a, b, state.active_space)
        });
        ids = Some((a, b, space));
        let view = cx.new(|cx| NetworkPanel::new(a, state.clone(), owner.downgrade(), window, cx));
        panel = Some(view.clone());
        workspace = Some(owner);
        Root::new(view, window, cx)
    });
    let panel = panel.unwrap();
    let (a, b, space) = ids.unwrap();
    let peer = |id, title: &str| Peer {
        thread: id,
        space,
        title: title.into(),
        project: "Coordination QA".into(),
        worktree: directory.join("repo").to_string_lossy().into_owned(),
        harness: "omp".into(),
        status: "Idle".into(),
        token: Some(Uuid::new_v4()),
        eligible: true,
        delivery_enabled: true,
        can_receive: true,
        dormant: false,
    };
    let a = peer(a, "Author");
    let b = peer(b, "Reviewer");
    let mailbox_store = Store::open(directory.join("mailbox")).unwrap();
    let (mailbox, updates) = Network::start(mailbox_store, None).unwrap();
    let call =
        |peer: &Peer, action| smol::block_on(mailbox.call(peer.token.unwrap(), action)).unwrap();
    smol::block_on(async {
        mailbox.sync(vec![a.clone(), b.clone()]).await.unwrap();
        mailbox.set_joined(a.thread, true).await.unwrap();
        mailbox.set_joined(b.thread, true).await.unwrap();
    });
    call(
        &b,
        Action::SetIdentity {
            activity: Some("Reviewing network coordination".into()),
            tags: Some(vec!["role:reviewer".into()]),
        },
    );
    let normal = call(
        &a,
        Action::Send {
            recipient: b.thread,
            body: "Review this change".into(),
            priority: Priority::Normal,
        },
    );
    let id = serde_json::from_value(normal["message"]["id"].clone()).unwrap();
    call(
        &b,
        Action::Reply {
            message: id,
            body: "Review complete".into(),
            priority: Priority::Fyi,
        },
    );
    call(&a, Action::Inbox);
    call(
        &b,
        Action::SetInbound {
            policy: InboundPolicy::Hold,
        },
    );
    let holding = updates.try_recv().unwrap();
    call(
        &b,
        Action::SetInbound {
            policy: InboundPolicy::Refuse,
        },
    );
    call(
        &a,
        Action::Send {
            recipient: b.thread,
            body: "Refused message".into(),
            priority: Priority::Urgent,
        },
    );
    let refusing = updates.try_recv().unwrap();
    cx.run_until_parked();
    let states = [Arc::new(NetworkSnapshot::default()), holding, refusing];
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for snapshot in &states {
            cx.update_window(window.into(), |_, window, cx| {
                Theme::change(mode, Some(window), cx);
                state.update(cx, |state, _| {
                    state.network.snapshot = snapshot.clone();
                    state.network.revision += 1;
                    state.network.pending.insert(a.thread);
                    state.network.error = Some("Storage temporarily unavailable".into());
                });
                panel.update(cx, |panel, cx| panel.show_view(View::Members, window, cx));
                window.render_frame(cx);
                let rows = panel.read(cx).list.read(cx).delegate().rows.len();
                assert_eq!(rows, snapshot.members.len());
                window.press("g", cx);
                window.press("g", cx);
                window.press("j", cx);
                window.render_frame(cx);
                if rows > 1 {
                    assert_eq!(
                        panel.read(cx).list.read(cx).delegate().selected,
                        Some(IndexPath::new(1))
                    );
                }
                state.update(cx, |state, _| {
                    state.network.pending.clear();
                    state.network.error = None;
                });
                panel.update(cx, |panel, cx| panel.show_view(View::Messages, window, cx));
                window.render_frame(cx);
                if !snapshot.messages.is_empty() {
                    window.press("enter", cx);
                    assert!(panel.read(cx).inspection_focus.is_focused(window));
                    window.press("j", cx);
                    window.press("escape", cx);
                    assert!(panel.read(cx).focus_handle(cx).is_focused(window));
                    for _ in 0..snapshot.messages.len() {
                        window.press("j", cx);
                        window.render_frame(cx);
                        assert!(window.find("network-selection-detail").visible());
                    }
                }
            })
            .unwrap();
        }
    }
    assert_eq!(delivery_label(&DeliveryState::Read), "Retrieved");
    assert_eq!(
        delivery_label(&DeliveryState::Refused {
            reason: "Recipient refuses messages".into()
        }),
        "Refused"
    );
    drop(workspace);
}
