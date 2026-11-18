use crate::{
    app::{AppState, Bootstrap},
    store::Store,
    ui::{
        commands,
        workspace::{Workspace, WorkspaceRoot},
    },
};
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, TestAppContext,
    component::{Root, WindowExt, dialog::Dialog, notification::Notification},
    test::TestWindowExt,
};
use uuid::Uuid;

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<AppState>) {
    cx.dispatcher.allow_parking();
    let directory = std::env::temp_dir().join(format!("drostje-shutdown-ui-{}", Uuid::new_v4()));
    let store = Store::open(directory.join("ui")).unwrap();
    let snapshot = smol::block_on(store.snapshot()).unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::theme::initialize(None, Some(&directory), cx);
        commands::bind_keys(cx);
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
    let window = cx.add_window(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let content = cx.new(|_| WorkspaceRoot::new(workspace));
        Root::new(content, window, cx)
    });
    (window.into(), state)
}

#[gpui_kit::test]
fn shutting_down_closes_dialogs_sheets_and_notifications_above_the_saving_screen(
    cx: &mut TestAppContext,
) {
    let (window, state) = open(cx);
    cx.update_window(window, |_, window, cx| {
        window.open_dialog(cx, |dialog: Dialog, _, _| dialog.title("Rename Thread"));
        window.push_notification(Notification::new().message("Saved"), cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(window.notifications(cx).len(), 1);
    })
    .unwrap();
    state.update(cx, |state, cx| {
        state.shutting_down = true;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            !window.has_active_dialog(cx),
            "no dialog above the saving screen"
        );
        assert!(!window.has_active_sheet(cx));
        assert!(window.notifications(cx).is_empty());
    })
    .unwrap();
    cx.run_until_parked();
}
