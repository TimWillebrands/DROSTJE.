//! Opt-in, disposable real-process checks. No init, plugins, provider calls or
//! personal ShaDa are used. Run with NVIM=/opt/homebrew/bin/nvim cargo test
//! --test neovim_runtime -- --ignored --nocapture.
use drost::model::PaneDirection;
use drost::neovim::{
    ClipboardContent, DraftKey, NvimCommand, NvimConfig, NvimEditor, NvimEvent, NvimTheme,
    grid::GridSnapshot,
};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

struct Runtime {
    editor: NvimEditor,
    grid: Arc<Mutex<Option<Arc<GridSnapshot>>>>,
    clipboard: Arc<Mutex<ClipboardContent>>,
    diagnostics: Arc<Mutex<Vec<String>>>,
    focus: async_channel::Receiver<(DraftKey, Uuid, PaneDirection)>,
    directory: PathBuf,
}
impl Runtime {
    async fn start() -> Self {
        Self::start_with_theme(None).await
    }
    async fn start_with_theme(initial_theme: Option<NvimTheme>) -> Self {
        let directory = std::env::temp_dir().join(format!("drost-nvim-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let executable = std::env::var_os("NVIM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/nvim"));
        let mut config = NvimConfig::new(executable);
        config.initial_theme = initial_theme;
        let editor = NvimEditor::open(config)
            .await
            .expect("isolated Neovim startup");
        let grid = Arc::new(Mutex::new(None));
        let clipboard = Arc::new(Mutex::new(ClipboardContent {
            lines: vec![String::new()],
            regtype: "v".into(),
        }));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let (focus_tx, focus) = async_channel::unbounded();
        let events = editor.events.clone();
        let observed_grid = grid.clone();
        let native_clipboard = clipboard.clone();
        let observed_diagnostics = diagnostics.clone();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv_blocking() {
                match event {
                    NvimEvent::Grid(grid) => *observed_grid.lock() = Some(grid),
                    NvimEvent::ClipboardGet { reply } => {
                        let _ = reply.try_send(Ok(native_clipboard.lock().clone()));
                    }
                    NvimEvent::ClipboardSet(content) => *native_clipboard.lock() = content,
                    NvimEvent::FocusPane {
                        key,
                        generation,
                        direction,
                    } => {
                        focus_tx.try_send((key, generation, direction)).unwrap();
                    }
                    NvimEvent::Failed(error)
                    | NvimEvent::InputRejected(error)
                    | NvimEvent::CompositionRejected(error) => {
                        observed_diagnostics.lock().push(error)
                    }
                    _ => {}
                }
            }
        });
        Self {
            editor,
            grid,
            clipboard,
            diagnostics,
            focus,
            directory,
        }
    }
    async fn draft(&self, text: &str) -> DraftKey {
        let key = DraftKey::Thread(Uuid::new_v4());
        self.editor
            .open_draft(key.clone(), text.into())
            .await
            .unwrap();
        self.editor.focus_draft(key.clone()).await.unwrap();
        key
    }
    async fn keys(&self, keys: &str) {
        self.editor
            .commands
            .send(NvimCommand::Input(keys.into()))
            .await
            .unwrap();
    }
    async fn focus(&self) -> (DraftKey, Uuid, PaneDirection) {
        smol::future::race(
            async { self.focus.recv().await.expect("editor focus event stream") },
            async {
                smol::Timer::after(Duration::from_secs(2)).await;
                panic!("Neovim did not publish the ordered pane-focus request");
            },
        )
        .await
    }
    async fn wait_grid(&self, matches: impl Fn(&GridSnapshot) -> bool) -> Arc<GridSnapshot> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(grid) = self.grid.lock().clone().filter(|grid| matches(grid)) {
                return grid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected Neovim grid was not published: {:?}",
                self.diagnostics.lock()
            );
            smol::Timer::after(Duration::from_millis(5)).await;
        }
    }
    async fn close(self) {
        self.editor.persistence_barrier().await.unwrap();
        self.editor.shutdown().await.unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let (reply, _) = async_channel::bounded(1);
        let _ = self.editor.try_command(NvimCommand::Shutdown { reply });
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn immediately_snapshot_and_switch_after_modal_typing() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let first = runtime.draft("").await;
        runtime.keys("alpha beta<Esc>0wciwGAMMA<Esc>").await;
        let submitted = runtime.editor.snapshot(first.clone()).await.unwrap();
        assert_eq!(submitted.text, "alpha GAMMA");
        let second = DraftKey::Thread(Uuid::new_v4());
        runtime
            .editor
            .open_draft(second.clone(), "second draft".into())
            .await
            .unwrap();
        runtime.keys("A!<Esc>").await;
        runtime.editor.focus_draft(second.clone()).await.unwrap();
        assert_eq!(
            runtime.editor.snapshot(first.clone()).await.unwrap().text,
            "alpha GAMMA!"
        );
        assert_eq!(
            runtime.editor.snapshot(second).await.unwrap().text,
            "second draft"
        );
        assert!(
            !runtime
                .editor
                .replace_if_revision(first.clone(), submitted.revision(), String::new())
                .await
                .unwrap()
        );
        assert_eq!(
            runtime.editor.snapshot(first).await.unwrap().text,
            "alpha GAMMA!"
        );
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn ordered_paste_unicode_resize_and_revision_safe_clear() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("").await;
        let text = "first\n日本語 e\u{301} 😀";
        runtime
            .editor
            .commands
            .send(NvimCommand::Paste(text.into()))
            .await
            .unwrap();
        let snapshot = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_eq!(snapshot.text.as_bytes(), text.as_bytes());
        runtime
            .editor
            .commands
            .send(NvimCommand::Resize { cols: 18, rows: 6 })
            .await
            .unwrap();
        runtime.keys("gg<C-e>").await;
        runtime.editor.snapshot(key.clone()).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if runtime
                .grid
                .lock()
                .as_ref()
                .is_some_and(|grid| grid.cols == 18 && grid.rows.len() == 6)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "acknowledged resized flush was not published"
            );
            smol::Timer::after(Duration::from_millis(5)).await;
        }
        let grid = runtime.grid.lock().clone().unwrap();
        assert!(grid.rows.iter().all(|row| row.len() == grid.cols));
        assert!(grid.cursor.0 < grid.rows.len() && grid.cursor.1 < grid.cols);
        assert!(
            runtime
                .editor
                .replace_if_revision(key.clone(), snapshot.revision(), String::new())
                .await
                .unwrap()
        );
        runtime.keys("next<CR>line").await;
        assert_eq!(
            runtime.editor.snapshot(key).await.unwrap().text,
            "next\nline"
        );
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn real_operators_registers_undo_redo_search_and_commands() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("").await;
        runtime.keys("one two<CR>three<CR>four<Esc>ggdd").await;
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "three\nfour"
        );
        runtime.keys("u").await;
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "one two\nthree\nfour"
        );
        runtime.keys("<C-r>").await;
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "three\nfour"
        );
        let words = runtime.draft("alpha beta").await;
        runtime.keys("0v4l").await;
        runtime
            .editor
            .commands
            .send(NvimCommand::Copy { cut: false })
            .await
            .unwrap();
        runtime.keys("A <Esc>p").await;
        assert_eq!(
            runtime.editor.snapshot(words.clone()).await.unwrap().text,
            "alpha beta alpha"
        );
        assert_eq!(runtime.clipboard.lock().regtype, "v");
        runtime.keys("0/beta<CR>").await;
        assert_eq!(
            runtime.editor.snapshot(words.clone()).await.unwrap().cursor,
            (0, 6)
        );
        runtime.keys(":s/beta/GAMMA/<CR>").await;
        assert_eq!(
            runtime.editor.snapshot(words).await.unwrap().text,
            "alpha GAMMA alpha"
        );
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn command_line_and_composition_block_switch_until_explicit_cancel() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("draft").await;
        runtime.keys(":echo 'still editing'").await;
        assert!(runtime.editor.snapshot(key.clone()).await.is_err());
        runtime.editor.cancel_command().await.unwrap();
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "draft"
        );
        runtime
            .editor
            .commands
            .send(NvimCommand::SetComposition(true))
            .await
            .unwrap();
        assert!(runtime.editor.snapshot(key.clone()).await.is_err());
        runtime
            .editor
            .commands
            .send(NvimCommand::SetComposition(false))
            .await
            .unwrap();
        let snapshot = runtime.editor.snapshot(key.clone()).await.unwrap();
        runtime
            .editor
            .commands
            .send(NvimCommand::Replace {
                key: key.clone(),
                revision: snapshot.revision(),
                range: 0..5,
                text: "日本語😀".into(),
            })
            .await
            .unwrap();
        assert_eq!(runtime.editor.snapshot(key).await.unwrap().text, "日本語😀");
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; deliberately kills its own isolated child"]
fn exit_before_snapshot_callback_never_produces_submission_and_restart_drops_keys() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("retained draft").await;
        let durable = runtime.editor.snapshot(key.clone()).await.unwrap();
        runtime.keys(":lua os.exit(23)<CR>").await;
        // The process exits while earlier input is executing: the marker can
        // never acknowledge, so a caller cannot obtain a submission snapshot.
        assert!(runtime.editor.snapshot(key.clone()).await.is_err());
        runtime.keys("iDO NOT REPLAY<Esc>").await;
        runtime.editor.restart().await.unwrap();
        let restored = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_ne!(restored.generation, durable.generation);
        assert_eq!(restored.text, durable.text);
        assert_eq!(restored.cursor, durable.cursor);
        assert!(
            !runtime
                .editor
                .replace_if_revision(key, durable.revision(), String::new())
                .await
                .unwrap()
        );
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn pane_chords_only_leave_normal_mode_without_editing_text() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("alpha beta").await;
        let original = runtime.editor.snapshot(key.clone()).await.unwrap();
        for (keys, direction) in [
            ("<C-w>h", PaneDirection::Left),
            ("<C-w>j", PaneDirection::Down),
            ("<C-w>k", PaneDirection::Up),
            ("<C-w>l", PaneDirection::Right),
            ("<C-w><C-h>", PaneDirection::Left),
            ("<C-w><C-j>", PaneDirection::Down),
            ("<C-w><C-k>", PaneDirection::Up),
            ("<C-w><C-l>", PaneDirection::Right),
        ] {
            runtime.keys(keys).await;
            assert_eq!(
                runtime.focus().await,
                (key.clone(), original.generation, direction)
            );
            assert_eq!(
                runtime.editor.snapshot(key.clone()).await.unwrap().text,
                original.text
            );
        }

        // Insert and command-line Ctrl-W retain their native word deletion.
        // The following h must remain text, not the second key of a host chord.
        runtime.keys("A<C-w>h<Esc>").await;
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "alpha h"
        );
        runtime.keys(":echo alpha beta<C-w>h").await;
        runtime
            .wait_grid(|grid| {
                grid.cmdline.last_key_value().is_some_and(|(_, cmd)| {
                    cmd.content
                        .iter()
                        .map(|chunk| chunk.text.as_str())
                        .collect::<String>()
                        == "echo alpha h"
                })
            })
            .await;
        runtime.editor.cancel_command().await.unwrap();
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "alpha h"
        );

        // An ordered normal-mode event fences both negative cases: any
        // accidental insert/command-line event would arrive before this one.
        runtime.keys("<C-w>l").await;
        assert_eq!(
            runtime.focus().await,
            (key, original.generation, PaneDirection::Right)
        );
        assert!(runtime.focus.try_recv().is_err());
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn pane_focus_ignores_old_generations_and_unknown_buffers() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("retained 日本語 😀").await;
        let before = runtime.editor.snapshot(key.clone()).await.unwrap();
        runtime.editor.restart().await.unwrap();
        let after = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_ne!(before.generation, after.generation);
        runtime.keys(&format!(
            ":lua vim.rpcnotify(drost.channel,'drost_focus_pane','{}',vim.api.nvim_get_current_buf(),'left')<CR>",
            before.generation
        )).await;
        runtime.keys(
            ":lua vim.rpcnotify(drost.channel,'drost_focus_pane',drost.generation,2147483647,'up')<CR>"
        ).await;
        runtime.keys("<C-w>l").await;
        assert_eq!(
            runtime.focus().await,
            (key.clone(), after.generation, PaneDirection::Right)
        );
        assert!(runtime.focus.try_recv().is_err());
        assert_eq!(
            runtime.editor.snapshot(key).await.unwrap().text,
            before.text
        );
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn palette_changes_repaint_real_grid_and_survive_restart_without_editing() {
    smol::block_on(async {
        let light = NvimTheme {
            dark: false,
            background: 0xf1e2d3,
            foreground: 0x213243,
            popover: 0xe3d4c5,
            popover_foreground: 0x324354,
            selection: 0xc1d2e3,
            caret: 0x753159,
            primary: 0x2468ac,
            primary_foreground: 0xfedcba,
        };
        let dark = NvimTheme {
            dark: true,
            background: 0x172839,
            foreground: 0xd3e4f5,
            popover: 0x28394a,
            popover_foreground: 0xc2d3e4,
            selection: 0x394a5b,
            caret: 0xe1b273,
            primary: 0x8ac4e6,
            primary_foreground: 0x123456,
        };
        let runtime = Runtime::start_with_theme(Some(light)).await;
        let text = "alpha beta\n日本語 e\u{301} 😀";
        let key = runtime.draft(text).await;
        let before = runtime.editor.snapshot(key.clone()).await.unwrap();
        runtime
            .wait_grid(|grid| {
                grid.default_bg == light.background && grid.default_fg == light.foreground
            })
            .await;
        runtime.keys("gg0v4l").await;
        runtime
            .wait_grid(|grid| {
                grid.rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|cell| grid.highlights.get(&cell.hl_id))
                    .is_some_and(|highlight| highlight.background == Some(light.selection))
            })
            .await;
        runtime
            .editor
            .commands
            .send(NvimCommand::SetTheme(dark))
            .await
            .unwrap();
        runtime
            .wait_grid(|grid| {
                grid.default_bg == dark.background
                    && grid.default_fg == dark.foreground
                    && grid
                        .rows
                        .first()
                        .and_then(|row| row.first())
                        .and_then(|cell| grid.highlights.get(&cell.hl_id))
                        .is_some_and(|highlight| highlight.background == Some(dark.selection))
            })
            .await;
        runtime.keys("y<C-w>l").await;
        assert_eq!(
            runtime.focus().await,
            (key.clone(), before.generation, PaneDirection::Right)
        );
        assert_eq!(runtime.clipboard.lock().text(), "alpha");
        let themed = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_eq!(themed.text.as_bytes(), text.as_bytes());
        assert_eq!(themed.changedtick, before.changedtick);
        runtime.keys(":call matchadd('Search', 'beta')<CR>").await;
        runtime
            .wait_grid(|grid| {
                grid.rows
                    .first()
                    .and_then(|row| row.get(6))
                    .and_then(|cell| grid.highlights.get(&cell.hl_id))
                    .is_some_and(|highlight| {
                        highlight.foreground == Some(dark.primary_foreground)
                            && highlight.background == Some(dark.primary)
                    })
            })
            .await;

        runtime.editor.restart().await.unwrap();
        let restored = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_ne!(restored.generation, before.generation);
        assert_eq!(restored.text.as_bytes(), text.as_bytes());
        assert_eq!(runtime.clipboard.lock().text(), "alpha");
        runtime
            .wait_grid(|grid| {
                grid.generation == restored.generation
                    && grid.default_bg == dark.background
                    && grid.default_fg == dark.foreground
                    && grid
                        .mode_styles
                        .get(grid.mode_index)
                        .and_then(|style| grid.highlights.get(&style.attr_id))
                        .is_some_and(|highlight| {
                            highlight.background == Some(dark.caret)
                                && highlight.foreground == Some(dark.background)
                        })
            })
            .await;
        runtime.keys("gg0v4l").await;
        runtime
            .wait_grid(|grid| {
                grid.rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|cell| grid.highlights.get(&cell.hl_id))
                    .is_some_and(|highlight| highlight.background == Some(dark.selection))
            })
            .await;
        assert_eq!(
            runtime.editor.snapshot(key).await.unwrap().text.as_bytes(),
            text.as_bytes()
        );
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}
