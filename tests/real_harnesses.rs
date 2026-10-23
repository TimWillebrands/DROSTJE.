//! Opt-in checks against the real Codex and OpenCode executables. They are ignored by
//! default and need an explicit executable path, so `cargo test` never starts a harness:
//!
//! ```sh
//! DROSTJE_REAL_CODEX=$(command -v codex) cargo test --test real_harnesses codex -- --ignored --nocapture
//! DROSTJE_REAL_OPENCODE=$(command -v opencode) cargo test --test real_harnesses opencode -- --ignored --nocapture
//! ```
//!
//! Opening a conversation sends no prompt. `DROSTJE_REAL_TURN=1` also sends one short
//! prompt, which uses the logged-in account's quota. Both run in a fresh temp directory,
//! never in a working repository.
use anyhow::{Context, Result, bail};
use drostje::harness::{
    self, ConversationCommand, ConversationConfig, ConversationEvent, ConversationHandle,
    ConversationState, TurnOutcome,
};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

fn executable(variable: &str) -> Result<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .with_context(|| format!("Set {variable} to the executable to test"))
}

fn next(handle: &ConversationHandle, seconds: u64) -> Result<ConversationEvent> {
    smol::block_on(smol::future::race(
        async { handle.events.recv().await.ok() },
        async {
            smol::Timer::after(Duration::from_secs(seconds)).await;
            None
        },
    ))
    .context("Timed out waiting for a harness event")
}

fn exercise(harness_name: &str, executable: PathBuf) -> Result<()> {
    let root = std::env::temp_dir().join(format!("drostje-real-{harness_name}-{}", Uuid::new_v4()));
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd)?;
    let handle = smol::block_on(harness::open(ConversationConfig {
        harness: harness_name.into(),
        executable,
        cwd,
        session_dir: root.join("session"),
        session_ref: None,
        model: None,
        effort: None,
        permission_mode: None,
        browser_tools: None,
        allow_empty_session: true,
    }))?;
    let mut models = 0;
    let mut commands = None;
    loop {
        match next(&handle, 90)? {
            ConversationEvent::Ready {
                models: list,
                session_ref,
                ..
            } => {
                println!("Ready: {} models, session {session_ref}", list.len());
                models = list.len();
            }
            ConversationEvent::CommandsUpdated(list) => commands = Some(list.len()),
            ConversationEvent::CommandsUnavailable(reason) => {
                println!("Commands unavailable: {reason}")
            }
            ConversationEvent::Failed { message, .. } => bail!("{harness_name} failed: {message}"),
            ConversationEvent::Notice { level, text } if level != "diagnostic" => {
                println!("Notice ({level}): {text}")
            }
            ConversationEvent::StateChanged(ConversationState::Ready) if models > 0 => break,
            _ => {}
        }
    }
    println!("Commands: {commands:?}");
    if std::env::var_os("DROSTJE_REAL_TURN").is_some() {
        let request_id = Uuid::new_v4();
        handle.commands.send_blocking(ConversationCommand::Submit {
            request_id,
            text: "Reply with the single word: pong".into(),
            images: Vec::new(),
        })?;
        loop {
            match next(&handle, 120)? {
                ConversationEvent::MessageUpdated(message) if message.role == "assistant" => {
                    println!("Assistant: {:?}", message.content)
                }
                ConversationEvent::ContextUsageUpdated(usage) => println!("Context: {usage:?}"),
                ConversationEvent::TurnSettled {
                    request_id: id,
                    outcome,
                } if id == request_id => {
                    assert_eq!(outcome, TurnOutcome::Completed);
                    break;
                }
                ConversationEvent::Failed { message, .. } => {
                    bail!("{harness_name} failed: {message}")
                }
                _ => {}
            }
        }
    }
    handle.commands.send_blocking(ConversationCommand::Close)?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
#[ignore = "starts the real Codex app-server; set DROSTJE_REAL_CODEX"]
fn codex_opens_a_thread() -> Result<()> {
    exercise("codex", executable("DROSTJE_REAL_CODEX")?)
}

#[test]
#[ignore = "starts the real OpenCode ACP agent; set DROSTJE_REAL_OPENCODE"]
fn opencode_opens_a_session() -> Result<()> {
    exercise("opencode", executable("DROSTJE_REAL_OPENCODE")?)
}
