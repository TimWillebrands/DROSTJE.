use super::*;

fn settle(cx: &mut TestAppContext, fixture: &Fixture) {
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
}

fn bounds(cx: &mut TestAppContext, fixture: &Fixture) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.find("composer-row").bounds()
    })
    .unwrap()
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; real prompt editor, drag, keyboard sizing and persisted per-thread heights"]
async fn prompt_resize_preserves_drafts_and_thread_sizes(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let first = new_conversation(cx, &fixture).await;
    cx.simulate_window_resize(fixture.window.into(), size(px(1440.), px(900.)));
    settle(cx, &fixture);
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_composer(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "k e e p secondary-escape");
    until(cx, "committed draft", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(first)].text == "keep"
    })
    .await;
    settle(cx, &fixture);
    let before = bounds(cx, &fixture);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        let from = gpui_kit::point(before.center().x, before.top());
        window.drag(from, gpui_kit::point(from.x, from.y - px(72.)), cx);
    })
    .unwrap();
    settle(cx, &fixture);
    let dragged = bounds(cx, &fixture);
    assert!(
        (dragged.size.height - before.size.height - px(72.)).abs() < px(3.),
        "drag resizes the prompt, not its sidebar: {before:?} -> {dragged:?}"
    );
    cx.dispatch_action(fixture.window.into(), commands::ShrinkComposer);
    settle(cx, &fixture);
    let smaller = bounds(cx, &fixture);
    assert!((dragged.size.height - smaller.size.height - px(48.)).abs() < px(2.));
    let first_height = cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .thread_composer_height(first)
            .unwrap()
    });
    let second = new_conversation(cx, &fixture).await;
    settle(cx, &fixture);
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .thread_composer_height(second)
            .is_none()
    }));
    cx.dispatch_action(fixture.window.into(), commands::GrowComposer);
    settle(cx, &fixture);
    let second_height = cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .thread_composer_height(second)
            .unwrap()
    });
    cx.dispatch_action(fixture.window.into(), commands::PreviousThread);
    settle(cx, &fixture);
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_thread),
        Some(first)
    );
    assert!((f32::from(bounds(cx, &fixture).size.height) - first_height).abs() < 2.);
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(first)]
            .text
            .clone()),
        "keep"
    );
    for mode in ["light", "dark"] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        cx.simulate_window_resize(fixture.window.into(), size(px(900.), px(600.)));
        settle(cx, &fixture);
        for _ in 0..20 {
            cx.dispatch_action(fixture.window.into(), commands::GrowComposer);
            settle(cx, &fixture);
        }
        let composer = bounds(cx, &fixture);
        let panel = cx
            .update_window(fixture.window.into(), |_, window, _| {
                window
                    .find(gpui_kit::SharedString::from(format!("Thread:{first}")))
                    .bounds()
            })
            .unwrap();
        assert!(
            composer.top() >= panel.top() + px(79.),
            "resizing must preserve usable history"
        );
        assert!(composer.bottom() <= panel.bottom() + px(1.));
        assert!(
            cx.read(|cx| fixture.state.read(cx).thread_composer_height(second))
                == Some(second_height)
        );
    }
    quit(cx, &fixture).await;
    let saved = fixture.store.snapshot().await.unwrap();
    assert_eq!(
        saved.settings["thread_composer_height"][second.to_string()],
        json!(second_height)
    );
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == first)
            .unwrap()
            .draft,
        "keep"
    );
}
