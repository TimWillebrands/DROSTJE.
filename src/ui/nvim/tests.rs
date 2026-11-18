use super::NvimView;
use crate::model::DraftKey;
use crate::neovim::{NvimCommand, NvimConfig, NvimEditor};
use gpui_kit::component::Root;
use gpui_kit::{AppContext, Entity, EntityInputHandler, TestAppContext};
use std::time::{Duration, Instant};

async fn until(
    cx: &mut TestAppContext,
    view: &Entity<NvimView>,
    predicate: impl Fn(&NvimView) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| predicate(view.read(cx))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Neovim view did not reach expected state: {:?}",
            cx.read(|cx| {
                let view = view.read(cx);
                (
                    view.ready,
                    view.writable,
                    view.error.clone(),
                    view.draft.clone(),
                    view.grid.generation,
                    view.grid.rows.len(),
                )
            })
        );
        smol::Timer::after(Duration::from_millis(10)).await;
    }
}
async fn service<T>(cx: &mut TestAppContext, future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if let Some(result) = smol::future::or(async { Some(future.as_mut().await) }, async {
            smol::Timer::after(Duration::from_millis(10)).await;
            None
        })
        .await
        {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "Native Neovim operation timed out"
        );
    }
}

#[gpui_kit::test]
#[ignore = "requires installed Neovim; isolated native typing and flushed grids"]
async fn native_typing_bursts_reach_the_flushed_grid(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let path = crate::platform::resolve_executable("nvim", None).unwrap();
    let editor = NvimEditor::open(NvimConfig::new(path)).await.unwrap();
    let key = DraftKey::Thread(uuid::Uuid::new_v4());
    editor.open_draft(key.clone(), String::new()).await.unwrap();
    editor.focus_draft(key).await.unwrap();
    editor
        .try_command(NvimCommand::Input("<Esc>i".into()))
        .unwrap();
    cx.update(gpui_kit::init);
    cx.update(crate::ui::commands::bind_keys);
    let mut view = None;
    let window = cx.add_window(|window, cx| {
        let entity = cx.new(|cx| NvimView::new(editor.clone(), window, cx));
        entity.update(cx, |view, cx| view.focus(window, cx));
        view = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let view = view.unwrap();
    until(cx, &view, |view| view.enabled()).await;
    let mut expected = String::new();
    for burst in ["gghjkl", "burst", "visible", ":?{}~", "é"] {
        for character in burst.chars() {
            cx.update_window(window.into(), |_, window, cx| {
                assert!(
                    window.dispatch_keystroke(
                        gpui_kit::Keystroke::parse_with_os(
                            &character.to_string(),
                            cx.platform_info().os,
                        )
                        .unwrap(),
                        cx,
                    )
                );
                assert!(!window.has_pending_keystrokes());
            })
            .unwrap();
        }
        expected.push_str(burst);
        until(cx, &view, |view| {
            view.draft
                .as_ref()
                .is_some_and(|draft| draft.text == expected)
                && view.grid.rows.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains(&expected)
                })
        })
        .await;
        cx.read(|cx| {
            let view = view.read(cx);
            assert!(view.error.is_none());
            assert!(!view.has_composition());
            assert!(view.recovery_text().is_empty());
        });
    }
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
    })
    .unwrap();
    until(cx, &view, |view| view.mode() == "normal").await;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse(":").unwrap(), cx);
    })
    .unwrap();
    until(cx, &view, |view| !view.grid.cmdline.is_empty()).await;
    for (key, text, modifiers) in [
        (
            "g",
            "G",
            gpui_kit::Modifiers {
                shift: true,
                ..Default::default()
            },
        ),
        (
            "e",
            "é",
            gpui_kit::Modifiers {
                alt: true,
                ..Default::default()
            },
        ),
    ] {
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(
                gpui_kit::Keystroke {
                    key: key.into(),
                    key_char: Some(text.into()),
                    modifiers,
                },
                cx,
            );
            assert!(!window.has_pending_keystrokes());
        })
        .unwrap();
    }
    until(cx, &view, |view| {
        view.grid
            .cmdline
            .last_key_value()
            .is_some_and(|(_, command)| {
                command
                    .content
                    .iter()
                    .map(|chunk| chunk.text.as_str())
                    .collect::<String>()
                    == "Gé"
            })
    })
    .await;
    cx.read(|cx| {
        assert_eq!(view.read(cx).draft.as_ref().unwrap().text, expected);
        assert!(!view.read(cx).has_composition());
    });
    editor.shutdown().await.unwrap();
}

#[gpui_kit::test]
#[ignore = "requires installed Neovim; isolated native input and real editing"]
async fn native_composition_is_recovery_only_until_committed(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let path = crate::platform::resolve_executable("nvim", None).unwrap();
    let editor = NvimEditor::open(NvimConfig::new(path)).await.unwrap();
    let key = DraftKey::Thread(uuid::Uuid::new_v4());
    editor
        .open_draft(key.clone(), "before".into())
        .await
        .unwrap();
    editor.focus_draft(key.clone()).await.unwrap();
    let initial = editor.snapshot(key.clone()).await.unwrap();
    editor
        .try_command(NvimCommand::Input("<Esc>A".into()))
        .unwrap();
    cx.update(gpui_kit::init);
    let mut view = None;
    let window = cx.add_window(|window, cx| {
        let entity = cx.new(|cx| NvimView::new(editor.clone(), window, cx));
        entity.update(cx, |view, cx| view.focus(window, cx));
        view = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let view = view.unwrap();
    until(cx, &view, |view| {
        view.enabled() && view.draft.as_ref().is_some_and(|draft| draft.cursor.1 == 6)
    })
    .await;
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(Some(6..6), "仮😀", Some(3..3), window, cx);
            assert_eq!(view.recovery_text(), "仮😀");
            assert!(view.has_composition());
        })
    })
    .unwrap();
    assert!(editor.snapshot(key.clone()).await.is_err());
    assert!(
        editor
            .replace_if_revision(key.clone(), initial.revision(), "unsafe".into())
            .await
            .is_err()
    );
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(None, "確定😀", window, cx);
        })
    })
    .unwrap();
    let committed = service(cx, editor.snapshot(key.clone())).await.unwrap();
    assert_eq!(committed.text, "before確定😀");
    until(cx, &view, |view| !view.has_composition()).await;
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_writable(false, cx);
            view.replace_text_in_range(None, "discard", window, cx);
            view.replace_and_mark_text_in_range(None, "discard", None, window, cx);
            assert!(view.recovery_text().is_empty());
            view.set_writable(true, cx);
        })
    })
    .unwrap();
    assert_eq!(
        editor.snapshot(key.clone()).await.unwrap().text,
        committed.text
    );
    until(cx, &view, |view| {
        view.draft
            .as_ref()
            .is_some_and(|draft| draft.text == committed.text)
    })
    .await;
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "retained", None, window, cx);
        })
    })
    .unwrap();
    assert_eq!(cx.read(|cx| view.read(cx).recovery_text()), "retained");
    editor.restart().await.unwrap();
    until(cx, &view, |view| view.ready && view.error.is_none()).await;
    assert_eq!(cx.read(|cx| view.read(cx).recovery_text()), "retained");
    assert!(editor.snapshot(key.clone()).await.is_err());
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.cancel_composition(window, cx);
        })
    })
    .unwrap();
    assert_eq!(editor.snapshot(key).await.unwrap().text, committed.text);
    editor.persistence_barrier().await.unwrap();
    editor.shutdown().await.unwrap();
}
