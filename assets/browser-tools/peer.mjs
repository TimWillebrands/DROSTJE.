// Private native custom-message ingress. All output goes through the runtime's
// serialized extension UI channel, never process.stdout.
export const PEER_COMMAND = "__drostje_peer";
const STATUS = "drostje.peer.v1";
const TYPE = "drostje-peer";

export function registerPeerDelivery(api, dialect) {
  if (typeof api.registerCommand !== "function" || typeof api.sendMessage !== "function" || typeof api.on !== "function") return;
  const seen = new Set();
  const cancelled = new Set();
  const accepted = new Set();
  let idleAttempt;
  const status = (ctx, event) => ctx.ui.setStatus(STATUS, JSON.stringify({ v: 1, dialect, ...event }));
  const peer = (message) => message?.role === "custom" && message.customType === TYPE && message.details?.origin === "peer" && message.details?.v === 1;
  const uuid = (value) => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);

  api.on("message_start", (event, ctx) => {
    if (!peer(event.message)) return;
    const attempt = event.message.details.attempt;
    if (!seen.has(attempt) || accepted.has(attempt)) return;
    accepted.add(attempt);
    status(ctx, { event: "accepted", attempt });
  });
  api.on("agent_start", (_event, ctx) => {
    if (!idleAttempt) return;
    const attempt = idleAttempt;
    idleAttempt = undefined;
    status(ctx, { event: "turn_started", attempt });
  });
  // The public APIs have no selective custom-message dequeue operation. Prevent
  // an interrupted, not-yet-observed peer message from entering later LLM context.
  // Already observed context is never recalled.
  api.on("context", (event) => ({
    messages: event.messages.filter((message) => !peer(message) || !cancelled.has(message.details.attempt)),
  }));
  api.registerCommand(PEER_COMMAND, {
    description: "Private DROSTJE peer delivery transport",
    handler: async (args, ctx) => {
      const envelope = JSON.parse(Buffer.from(args.trim(), "base64").toString("utf8"));
      if (envelope.v !== 1 || typeof ctx.ui?.setStatus !== "function" || typeof ctx.isIdle !== "function") throw new Error("Native peer delivery unavailable");
      if (envelope.op === "hello") {
        status(ctx, { event: "ready" });
        return;
      }
      if (envelope.op === "cancel") {
        if (!uuid(envelope.attempt)) throw new Error("Invalid peer attempt");
        if (!accepted.has(envelope.attempt)) cancelled.add(envelope.attempt);
        return;
      }
      if (envelope.op !== "deliver" || !uuid(envelope.attempt)) throw new Error("Invalid peer delivery");
      const { attempt, messages, content } = envelope;
      if (seen.has(attempt) || cancelled.has(attempt)) {
        status(ctx, { event: "rejected", attempt, reason: "Attempt cancelled or already dispatched; never replay" });
        return;
      }
      if (!Array.isArray(messages) || !messages.length || messages.length > 128 || typeof content !== "string" || messages.some((m) => !uuid(m.id) || !uuid(m.sender) || !uuid(m.space) || typeof m.sender_name !== "string" || typeof m.body !== "string")) {
        status(ctx, { event: "rejected", attempt, reason: "Invalid peer identities or content" });
        return;
      }
      // Bound replay/tombstone memory by failing closed, not forgetting IDs.
      if (seen.size >= 4096 || cancelled.size >= 4096) {
        status(ctx, { event: "rejected", attempt, reason: "Peer delivery lifetime limit reached; reopen explicitly" });
        return;
      }
      const nativeIdle = ctx.isIdle();
      const idle = envelope.trigger_turn === true && nativeIdle;
      seen.add(attempt);
      if (idle) idleAttempt = attempt;
      status(ctx, { event: "dispatching", attempt, idle });
      try {
        api.sendMessage({
          customType: TYPE, content, display: true,
          details: { v: 1, origin: "peer", attempt, messages },
        }, { deliverAs: dialect === "omp" && !nativeIdle ? "aside" : "steer", triggerTurn: idle });
      } catch (error) {
        if (idleAttempt === attempt) idleAttempt = undefined;
        // A void public API cannot prove whether an exception preceded admission.
        status(ctx, { event: "unknown", attempt, reason: String(error) });
      }
    },
  });
}
