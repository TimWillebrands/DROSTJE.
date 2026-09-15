use super::*;

pub(super) struct Shutdown {
    editor: Option<NvimEditor>,
    sessions: Vec<(bool, async_channel::Sender<ConversationCommand>)>,
    drains: Vec<async_channel::Receiver<()>>,
    writes: async_channel::Sender<Write>,
}

impl Shutdown {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            editor: state.editor.clone(),
            sessions: state
                .conversations
                .iter()
                .filter_map(|(id, session)| {
                    session
                        .commands
                        .clone()
                        .map(|commands| (state.active_threads.contains(id), commands))
                })
                .collect(),
            drains: state
                .conversations
                .values()
                .filter_map(|session| session.finished.clone())
                .collect(),
            writes: state.writes.clone(),
        }
    }

    pub(super) async fn run(
        self,
        state: Entity<AppState>,
        cx: &mut AsyncApp,
        allow_data_loss: bool,
    ) -> Result<()> {
        if let Some(editor) = &self.editor {
            match editor.persistence_barrier().await {
                Ok(drafts) => cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        for draft in drafts {
                            state.record_draft(draft);
                        }
                        cx.notify();
                    })
                }),
                Err(error) => {
                    if !allow_data_loss {
                        let _ = editor.resume_input().await;
                        return Err(error.context("Final editor snapshot failed; cancel the editor command or copy draft recovery before quitting"));
                    }
                    eprintln!(
                        "Quitting with retained cached drafts after explicit confirmation: {error:#}"
                    );
                }
            }
        }
        for (active, commands) in &self.sessions {
            if *active {
                let _ = commands.send(ConversationCommand::Interrupt).await;
            }
            let _ = commands.send(ConversationCommand::Close).await;
        }
        // The adapter enforces its owned-process five-second grace period. Its
        // pump acknowledges only after all terminal state writes were enqueued.
        for drain in self.drains {
            let _ = drain.recv().await;
        }
        let (reply, result) = async_channel::bounded(1);
        let flushed = match self.writes.send(Write::Flush(reply)).await {
            Ok(()) => result
                .recv()
                .await
                .context("Storage writer stopped during quit")
                .and_then(|result| result.map_err(anyhow::Error::msg)),
            Err(error) => Err(anyhow::anyhow!("Storage writer unavailable: {error}")),
        };
        if let Err(error) = flushed {
            if !allow_data_loss {
                if let Some(editor) = &self.editor {
                    let _ = editor.resume_input().await;
                }
                return Err(
                    error.context("Final persistence failed; copy draft recovery before quitting")
                );
            }
            eprintln!(
                "Quitting without a successful final flush after explicit confirmation: {error:#}"
            );
        }
        if let Some(editor) = self.editor {
            if let Err(error) = editor.shutdown().await {
                eprintln!("Neovim shutdown: {error:#}");
            }
        }
        Ok(())
    }
}

impl AppState {
    /// Run while the foreground event loop and windows still exist. GPUI's final
    /// platform shutdown hook allows only 200ms, not enough for durable I/O or RPC.
    pub fn begin_shutdown(&mut self, allow_data_loss: bool, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.shutdown_error = None;
        let shutdown = Shutdown::new(self);
        let state = cx.entity();
        cx.spawn(async move |this, cx| {
            let result = shutdown.run(state, cx, allow_data_loss).await;
            let _ = this.update(cx, |state, cx| match result {
                Ok(()) => {
                    state.factory = None;
                    state.shutdown_complete = true;
                    cx.quit();
                }
                Err(error) => {
                    state.shutting_down = false;
                    let error = format!("Quit cancelled: {error:#}");
                    state.shutdown_error = Some(error.clone());
                    state.notice(error);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}
