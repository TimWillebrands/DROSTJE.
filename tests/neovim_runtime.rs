//! Opt-in, disposable real-process checks. No init, plugins, provider calls or
//! personal ShaDa are used. Run with NVIM=/opt/homebrew/bin/nvim cargo test
//! --test neovim_runtime -- --ignored --nocapture.
use drostje::model::DraftKey;
use drostje::neovim::{
    ClipboardContent, NvimCommand, NvimConfig, NvimEditor, NvimEvent, NvimTheme, grid::GridSnapshot,
};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

/// Every role distinct, so a group drawn from the wrong role is visible.
const LIGHT: NvimTheme = NvimTheme {
    dark: false,
    foreground: 0x172135,
    background: 0xe7edf5,
    muted: 0x6b7385,
    surface: 0xdde4ee,
    border: 0xc3ccd9,
    selection: 0xb2c6df,
    caret: 0x285daa,
    accent: 0x1f4f99,
    popover: 0xf4f7fb,
    popover_foreground: 0x101827,
    success: 0x2e7d4f,
    warning: 0x9a6a00,
    danger: 0xb3261e,
    info: 0x0b6e99,
};

struct Runtime {
    editor: NvimEditor,
    grid: Arc<Mutex<Option<Arc<GridSnapshot>>>>,
    leaves: Arc<Mutex<usize>>,
    clipboard: Arc<Mutex<ClipboardContent>>,
    diagnostics: Arc<Mutex<Vec<String>>>,
    directory: PathBuf,
}
impl Runtime {
    async fn start() -> Self {
        let directory = std::env::temp_dir().join(format!("drostje-nvim-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let executable = std::env::var_os("NVIM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/nvim"));
        let editor = NvimEditor::open(NvimConfig::new(executable))
            .await
            .expect("isolated Neovim startup");
        let grid = Arc::new(Mutex::new(None));
        let clipboard = Arc::new(Mutex::new(ClipboardContent {
            lines: vec![String::new()],
            regtype: "v".into(),
        }));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let leaves = Arc::new(Mutex::new(0));
        let observed_leaves = leaves.clone();
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
                    NvimEvent::Leave => *observed_leaves.lock() += 1,
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
            leaves,
            clipboard,
            diagnostics,
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
    /// Waits until the latest grid satisfies `check`.
    async fn grid_until(&self, what: &str, check: impl Fn(&GridSnapshot) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !self.grid.lock().as_deref().is_some_and(&check) {
            assert!(std::time::Instant::now() < deadline, "{what}");
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
#[ignore = "requires a real Neovim executable; measures isolated input-to-grid delivery"]
fn queued_typing_burst_reaches_grid_without_snapshot_or_redraw() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("").await;
        let line = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKL";
        let sentinel = "BURST-COMPLETED";
        let expected = format!("{}{sentinel}", format!("{line}\n").repeat(8));
        let started = std::time::Instant::now();
        for character in expected.chars() {
            if character == '\n' {
                runtime.keys("<CR>").await;
            } else {
                runtime.keys(&character.to_string()).await;
            }
        }
        let queued = std::time::Instant::now();
        // Observe only published flushes. A snapshot sends modal input markers,
        // which could hide a redraw that stalls until another command arrives.
        // The generous bound tolerates loaded CI while catching human-visible
        // stalls; this is not a microbenchmark of each RPC round trip.
        let deadline = queued + Duration::from_millis(750);
        let expected_lines: Vec<_> = expected.lines().collect();
        loop {
            let caught_up = runtime.grid.lock().as_ref().is_some_and(|grid| {
                grid.rows.len() >= expected_lines.len()
                    && grid
                        .rows
                        .iter()
                        .zip(&expected_lines)
                        .all(|(row, expected)| {
                            let text: String = row.iter().map(|cell| cell.text.as_str()).collect();
                            text.trim_end() == *expected
                        })
            });
            if caught_up {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queued burst did not reach a flushed grid within 750ms; diagnostics: {:?}",
                runtime.diagnostics.lock()
            );
            smol::Timer::after(Duration::from_millis(1)).await;
        }
        let published = std::time::Instant::now();
        eprintln!(
            "{} individual input commands: queued in {:?}, grid caught up in {:?}, total {:?}",
            expected.chars().count(),
            queued.duration_since(started),
            published.duration_since(queued),
            published.duration_since(started)
        );
        assert_eq!(runtime.editor.snapshot(key).await.unwrap().text, expected);
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
fn theme_updates_follow_serialized_input_and_survive_restart() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("theme keeps draft").await;
        runtime
            .editor
            .commands
            .send(NvimCommand::Theme(LIGHT))
            .await
            .unwrap();
        let saved = runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_eq!(saved.text, "theme keeps draft");
        runtime.editor.restart().await.unwrap();
        let restored = runtime.editor.snapshot(key).await.unwrap();
        assert_eq!(restored.text, saved.text);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let matches = runtime.grid.lock().as_ref().is_some_and(|grid| {
                grid.generation == restored.generation
                    && grid.default_fg == 0x172135
                    && grid.default_bg == 0xe7edf5
            });
            if matches {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "restarted editor did not publish the retained theme"
            );
            smol::Timer::after(Duration::from_millis(5)).await;
        }
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn focus_resize_and_theme_wait_out_an_unfinished_key_sequence() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("first\nsecond").await;
        // Visual `g` waits for its second key; Neovim defers non-fast requests
        // until it arrives, which only the user can send.
        runtime.keys("<Esc>vg").await;
        let started = std::time::Instant::now();
        for command in [
            NvimCommand::Focus(false),
            NvimCommand::Resize { cols: 40, rows: 6 },
            NvimCommand::Theme(LIGHT),
            NvimCommand::Focus(true),
        ] {
            runtime.editor.commands.send(command).await.unwrap();
        }
        runtime.keys("g<Esc>").await;
        let saved = runtime.editor.snapshot(key).await.unwrap();
        assert_eq!(saved.text, "first\nsecond");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "advisory requests held later input for {:?}",
            started.elapsed()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !runtime
            .grid
            .lock()
            .as_ref()
            .is_some_and(|grid| grid.default_fg == 0x172135 && grid.default_bg == 0xe7edf5)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "deferred theme was never applied"
            );
            smol::Timer::after(Duration::from_millis(5)).await;
        }
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn persistence_barrier_freezes_every_draft_until_explicit_resume() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let first = runtime.draft("first").await;
        let second = runtime.draft("second").await;
        runtime.keys("A!<Esc>").await;
        let snapshots = runtime.editor.persistence_barrier().await.unwrap();
        assert_eq!(
            snapshots
                .iter()
                .find(|draft| draft.key == first)
                .unwrap()
                .text,
            "first"
        );
        let second_snapshot = snapshots.iter().find(|draft| draft.key == second).unwrap();
        assert_eq!(second_snapshot.text, "second!");
        runtime.keys("Aunpersisted<Esc>").await;
        assert!(
            runtime
                .editor
                .replace_if_revision(second.clone(), second_snapshot.revision(), String::new())
                .await
                .is_err()
        );
        runtime.editor.resume_input().await.unwrap();
        assert_eq!(
            runtime.editor.snapshot(second.clone()).await.unwrap().text,
            "second!"
        );
        runtime.keys("A resumed<Esc>").await;
        assert_eq!(
            runtime.editor.snapshot(second).await.unwrap().text,
            "second! resumed"
        );
        runtime.close().await;
    });
}

/// Foreground of the first cell showing `text`, through its highlight.
fn foreground_of(grid: &GridSnapshot, text: &str) -> Option<u32> {
    grid.rows
        .iter()
        .flat_map(|row| row.iter())
        .find_map(|cell| {
            (cell.text == text).then(|| {
                grid.highlights
                    .get(&cell.hl_id)
                    .and_then(|highlight| highlight.foreground)
                    .unwrap_or(grid.default_fg)
            })
        })
}

fn row_text(grid: &GridSnapshot, row: usize) -> String {
    grid.rows[row]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect()
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn vim_write_and_quit_commands_keep_neovim_and_the_draft() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("keep me").await;
        for (ix, keys) in [
            ":q<CR>", ":wq<CR>", ":x<CR>", ":qa!<CR>", "ZZ", "ZQ", "<C-w>q",
        ]
        .into_iter()
        .enumerate()
        {
            runtime.keys(keys).await;
            // The snapshot round trip orders every earlier key and notification.
            let snapshot = runtime.editor.snapshot(key.clone()).await.unwrap();
            assert_eq!(snapshot.text, "keep me", "{keys} changed the draft");
            assert_eq!(*runtime.leaves.lock(), ix + 1, "{keys} did not leave");
        }
        // A split closes like Vim's would; only the last window leaves the editor.
        runtime.keys(":split<CR>:wq<CR>").await;
        runtime.editor.snapshot(key.clone()).await.unwrap();
        assert_eq!(*runtime.leaves.lock(), 7);
        runtime.keys(":w<CR>").await;
        runtime.editor.snapshot(key.clone()).await.unwrap();
        runtime
            .grid_until("`:w` did not explain draft saving", |grid| {
                row_text(grid, grid.rows.len() - 1).starts_with("Drafts save automatically")
            })
            .await;
        runtime.keys(":bd<CR>").await;
        assert_eq!(
            runtime.editor.snapshot(key.clone()).await.unwrap().text,
            "keep me"
        );
        assert_eq!(*runtime.leaves.lock(), 7);
        // Other commands still run.
        runtime.keys(":s/keep/kept/<CR>").await;
        assert_eq!(runtime.editor.snapshot(key).await.unwrap().text, "kept me");
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}

#[test]
#[ignore = "requires a real Neovim executable; spawns disposable isolated processes"]
fn theme_colors_every_visible_group() {
    smol::block_on(async {
        let runtime = Runtime::start().await;
        let key = runtime.draft("themed").await;
        runtime
            .editor
            .commands
            .send(NvimCommand::Theme(LIGHT))
            .await
            .unwrap();
        runtime
            .grid_until("end-of-buffer filler was not themed", |grid| {
                grid.default_bg == LIGHT.background
                    && foreground_of(grid, "~") == Some(LIGHT.muted)
            })
            .await;
        // Typed only after the theme's redraw, which would clear the message.
        runtime.keys(":bd<CR>").await;
        runtime.editor.snapshot(key).await.unwrap();
        runtime
            .grid_until("the error message was not themed", |grid| {
                foreground_of(grid, "A") == Some(LIGHT.danger)
            })
            .await;
        assert!(runtime.diagnostics.lock().is_empty());
        runtime.close().await;
    });
}
