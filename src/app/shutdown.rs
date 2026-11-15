use super::*;

pub(super) struct Shutdown {
    editor: Option<crate::neovim::NvimEditor>,
    sessions: Vec<(bool, async_channel::Sender<ConversationCommand>)>,
    drains: Vec<async_channel::Receiver<()>>,
    writes: async_channel::Sender<Write>,
    network: Option<super::network::NetworkDrain>,
}

impl Shutdown {
    pub(super) fn new(state: &mut AppState) -> Self {
        state.commit_all_thread_edits();
        state.sync_network();
        Self {
            editor: state.editor.clone(),
            sessions: state
                .conversations
                .iter()
                .filter_map(|(id, session)| {
                    session.commands.clone().map(|commands| {
                        (
                            interrupt_on_shutdown(
                                state.active_threads.contains(id),
                                state
                                    .snapshot
                                    .threads
                                    .iter()
                                    .any(|thread| thread.id == *id && thread.remote.is_some()),
                            ),
                            commands,
                        )
                    })
                })
                .collect(),
            drains: state
                .conversations
                .values()
                .filter_map(|session| session.finished.clone())
                .collect(),
            writes: state.writes.clone(),
            network: state.network.drain(),
        }
    }

    pub(super) async fn run(self, allow_data_loss: bool) -> Result<()> {
        for (active, commands) in &self.sessions {
            if *active {
                let _ = commands.send(ConversationCommand::Interrupt).await;
            }
            let _ = commands.send(ConversationCommand::Close).await;
        }
        for drain in self.drains {
            let _ = drain.recv().await;
        }
        let network_drained = match self.network {
            Some(network) => network.wait().await,
            None => Ok(()),
        };
        let (reply, result) = async_channel::bounded(1);
        let flushed = match self.writes.send(Write::Flush(reply)).await {
            Ok(()) => result
                .recv()
                .await
                .context("Storage writer stopped during quit")
                .and_then(|result| result.map_err(anyhow::Error::msg)),
            Err(error) => Err(anyhow::anyhow!("Storage writer unavailable: {error}")),
        };
        let flushed = network_drained.and(flushed);
        if let Err(error) = flushed {
            if !allow_data_loss {
                if let Some(editor) = &self.editor
                    && let Err(resume_error) = editor.resume_input().await
                {
                    return Err(error.context(format!(
                        "Final persistence failed and Neovim input could not resume: {resume_error:#}; copy draft recovery before quitting"
                    )));
                }
                return Err(
                    error.context("Final persistence failed; copy draft recovery before quitting")
                );
            }
            eprintln!(
                "Quitting without a successful final flush after explicit confirmation: {error:#}"
            );
        }
        if let Some(editor) = self.editor
            && let Err(error) = editor.shutdown().await
        {
            eprintln!("Neovim shutdown: {error:#}");
        }
        Ok(())
    }
}

impl AppState {
    pub fn begin_shutdown(&mut self, allow_data_loss: bool, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.shutdown_error = None;
        let shutdown = Shutdown::new(self);
        cx.spawn(async move |this, cx| {
            let result = shutdown.run(allow_data_loss).await;
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
                    state.notice(NoticeLevel::Error, error);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

fn interrupt_on_shutdown(active: bool, remote: bool) -> bool {
    active && !remote
}

#[cfg(test)]
mod tests {
    use super::interrupt_on_shutdown;

    #[test]
    fn remote_shutdown_detaches_without_interrupting_the_active_turn() {
        assert!(!interrupt_on_shutdown(true, true));
        assert!(!interrupt_on_shutdown(false, true));
        assert!(interrupt_on_shutdown(true, false));
        assert!(!interrupt_on_shutdown(false, false));
    }
}
