# WIP: automatic Space-network delivery

This is a requested move-to-another-machine checkpoint, **not a completed feature**. Background implementation agents were stopped after atomic edits. Do not treat the previous pull-network test results as verification of this push implementation.

## User-approved behavior

Replace manual `network inbox` / `network wait` with Claude-like automatic delivery:

- Busy live agents receive peer-origin context at native safe boundaries, without interrupting running tools.
- Idle live agents admit a peer-origin turn automatically.
- Joining/Resume explicitly authorizes receiving messages that may start paid turns. Legacy opt-ins require fresh consent.
- Never mutate/submit Drafts or attachments, grant permissions from peers, restart a stopped/closed runtime, or retry uncertain delivery.
- Keep only `members`, `send`, `broadcast` as model tools; no shared todo board, file claims, or agent spawning.
- Restored/reconnected runtimes start paused. A new explicit user prompt may reactivate previously recorded automatic consent; Resume is the no-prompt activation path.

## Current implementation

- `src/network.rs`: migrated durable delivery states (`Queued`, `Delivering`, `Delivered`, `Failed`, `Unknown`, `Closed`), explicit automatic consent, claim/finish leases, restart uncertainty, scope/capacity/store handling. Runtime capabilities are never serialized. `claim(token)` persists the lease before returning a `DeliveryBatch`; `finish(attempt, outcome)` is host-only. 64KiB is a batch target, with a bounded singleton exception rather than a new message limit.
- `src/app/network.rs`: new controller for per-generation runtime authorization, activation nonce, native admission cancellation lease, background claims/receipts, peer-turn bookkeeping, Join/Resume/Leave. `src/app.rs` and `src/app/conversation.rs` connect lifecycle and read-only/interrupt/close/reconnect guards. These are implemented but not behavior-verified.
- `src/harness/mod.rs`: `PeerMessage`, `PeerDelivery`, `PeerDeliveryOutcome`; `DeliverPeer`; `Ready.peer_delivery`; `PeerDeliveryAvailable`, `PeerDelivery`, `PeerTurnStarted`; `ContentBlock::PeerMessage`. `peer_context()` escapes JSON `<`, `>`, `&` and frames peer content as untrusted, not user consent.
- `src/browser_tools.rs`, `assets/browser-tools/client.mjs`, `tools.json`: three-action tool cutover; obsolete wait SSE/cancellation machinery removed; normal deadlines retained. Preserve the earlier bug fix: flat optional string `i`/`recipient`/`body` are accepted, unused fields ignored, unknown/wrong-type fields rejected. The generated extension copy list includes `peer.mjs`.
- `src/ui/network.rs`, `commands.rs`, `workspace.rs`, `conversation.rs`: Resume action/palette entry; explicit paid-turn consent; native status versus network readiness; truthful per-recipient dispositions; distinct peer-authored transcript content. No incoming-message focus changes.

## Native implementation state

### OMP / Pi

Files: `src/harness/omp.rs`, new `src/harness/omp/peer.rs`, `assets/browser-tools/{omp.ts,pi.ts,peer.mjs}`.

Native custom-message injection uses a private registered extension command, not a human Submit:

```text
prompt('/__drostje_peer ' + base64(JSON envelope))
```

Envelope: `{v:1,op:'hello'|'deliver'|'cancel',attempt,messages,content,trigger_turn}`. Serialized native status: `extension_ui_request`, method `setStatus`, key `drostje.peer.v1`, statusText `{v:1,dialect,event,attempt,...}`. Receipt must come from actual custom message lifecycle/status correlation, never the void `sendMessage` return or ordinary prompt response.

Production requests always use `trigger_turn:true`; native `ctx.isIdle()` decides busy versus idle. OMP `aside` is used only when actually busy: its idle `aside` path can start a turn even with false. Pi uses `steer`. Native admission IDs are tracked separately from ordinary user turns. Cancellation tombstones unobserved peer context; already accepted context cannot be recalled.

Still needs review/tests: busy-host/native-idle race, replay rejection, missing readiness handshake, cancellation/abort behavior, synchronous sendMessage exception correlation, idle missing-model/auth settlement. Nothing has been exercised against real processes for this implementation yet.

Known researched versions: OMP18.12.2 matching upstream tag `can1357/oh-my-pi:v18.12.2`; Pi0.85.1 at `@earendil-works/pi-coding-agent`. Exact installed Pi API is `sendMessage(custom, {triggerTurn?,deliverAs:'steer'|'followUp'|'nextTurn'}) : void`. OMP additionally supports `aside` and agent attribution.

### Claude Code

Files: `src/harness/claude.rs`, new `src/harness/claude/peer.rs`, `peer/tests.rs`, **`peer/status.rs` (not wired yet)**.

Implemented but unverified: child PID/session UUID/protocol1 registry validation, native UDS send, `--replay-user-messages`, UUID/origin correlation, peer history, ordinary replay deduplication, separate native peer-turn accounting, cancellation and hold/release/drop handling. Installed CLI was2.1.306. No permission-mode/inbound policy setting was changed and no stdin fallback is allowed.

**Must finish status-socket integration:**

1. Wire `mod status`, listener state, and bounded polling into `peer.rs`.
2. Bind an owned status socket beside the validated target socket before send; include its real escaped `uds:` address in outer and envelope `from`.
3. Correlate `peer_message_status.orig_msg_id`: held -> Unknown/blocked; delivered -> Accepted; denied/refused/expired/dropped -> Rejected. Preserve raw diagnostics.
4. Explicitly deny actual native user replies to this status-only address with guidance to use `network send`; never silently drop/fake success.
5. Update tests that currently assert omitted `from`; test actual receipt/refusal/denial and cleanup behavior.
6. Reset `self.turn.interrupting` in `settle_peer()` once no ordinary turn remains.

Without this integration, native refusal before queue creation has no stdout event and remains unconfirmed until cancellation/loss. Socket write alone is **not** acceptance. The new status file contains uncompiled transport/credential code; audit it before wiring.

Native discovery: same-user Claude config `sessions/<child-pid>.json`; require matching PID, expected session UUID, peerProtocol1 and absolute socket path. Handle `CLAUDE_CONFIG_DIR`. No need to read/log messaging bearer tokens. Protocol1 frame is native user queue input with `msgV`, `msg_id`, `priority:'next'`, target session_id, uuid=delivery attempt, peer envelope, and owned `from`; native stdout `origin.kind='peer'`, UUIDs and kernel-verified PID are the acceptance evidence. Never spoof `from-mode` or own-child identity.

## Integration work remaining

1. Complete Claude status socket above.
2. **Revalidate temporary readiness before dispatching an already-claimed batch.** `dispatch_network_claim` currently rechecks authorization/generation but not `network_can_receive` after a permission/input/preparing race. `DeliveryOutcome::Deferred` already restores an unwritten Delivering lease to Queued and never requeues Unknown. Wire this only before any native write, then wait for a real readiness transition; no polling/retry loop.
3. Update `src/ui/network_runtime_qa.rs`: it still uses the removed inbox/wait/retrieved contract. Test automatic native receipts, idle admission, busy boundaries, Resume/stop/reconnect, legacy consent, and unchanged private Draft/attachments/focus.
4. Review all new native tests and extension lifecycle behavior. Verify unsupported/held/refused runtimes have actionable visible states.
5. Update README network section: it still describes pull retrieval/wait. Current tool instructions/UI describe push, so documentation is temporarily inconsistent.
6. Run the complete checks and actual UI/runtime smokes. Do not mark native acceptance or completed work from a socket write, a void API, or guessed lifecycle.

## Checkpoint verification

The first `cargo check --all-targets` found only two stale caller errors in `src/ui/workspace_browser.rs`: removed `cancelled` field/argument. Those were fixed. A frozen-tree check is being run; its final result is recorded below before handoff.

No push-feature tests, clippy, UI smoke, or provider calls have been executed. Prior pull-version tests are not push-feature proof.

Run next:

```sh
cargo check --all-targets
cargo check --all-targets --features runtime-qa
cargo test
cargo clippy --all-targets
```

Default tests must make no provider calls. For genuine OMP/Pi scratch-process probes, OS-deny networking (for example macOS `sandbox-exec` with deny network), use isolated HOME/config, register only the peer extension, and first prove context-only `trigger_turn:false` causes no `agent_start`. Then separately observe genuine idle admission/error/settlement with true; don't simulate a provider or claim successful generation. Claude's new ignored installed-CLI smoke is unexercised and must be reviewed for safe provider blocking before running.

## Moving machines

- Include all new files under `src/harness/{claude,omp}/` and `assets/browser-tools/peer.mjs` in the commit.
- Other dirty Ghostty, Neovim, editor, palette and modal changes are concurrent user work. Preserve them; do not reset or revert them to simplify network integration.
- Working branch was `main`; prefer a WIP branch for a partial checkpoint rather than shipping it as complete.
- Git carries source and this handoff, **not** local DROSTJE conversations/app data, provider credentials, runtime processes, or managed tool installs.
- Resume instruction: “Read NETWORK_PUSH_HANDOFF.md and finish automatic Space-network delivery without reverting concurrent work.”

Primary references:
- https://code.claude.com/docs/en/cross-session-messaging
- https://code.claude.com/docs/en/agent-teams
- https://code.claude.com/docs/en/channels-reference
- https://code.claude.com/docs/en/agent-sdk/typescript

Custom development MCP channels are not a drop-in solution: the development-channel flag is ignored in embedded `-p` mode. Use the native peer socket.
