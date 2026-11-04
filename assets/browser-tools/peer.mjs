// Private native custom-message ingress. All output goes through the runtime's
// serialized extension UI channel, never process.stdout.
export const PEER_COMMAND = "__drostje_peer";
const STATUS = "drostje.peer.v1";
const TYPE = "drostje-peer";

export function registerPeerDelivery(api, dialect) {
  // A missing command is reported by the host's command/readiness handshake.
  if (typeof api.registerCommand !== "function" || typeof api.sendMessage !== "function" || typeof api.on !== "function") return;
  const seen = new Map();
  const cancelled = new Set();
  const accepted = new Set();
  let firstMessage = false;
  const status = (ctx, event) => ctx.ui.setStatus(STATUS, JSON.stringify({ v: 1, dialect, ...event }));
  const peer = (message) => message?.role === "custom" && message.customType === TYPE && message.details?.origin === "peer" && message.details?.v === 1;
  const uuid = (value) => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);

  // agent_start alone has no origin. Attribute a turn only when its first native
  // input is our custom message, including a busy-host/native-idle race. Pi may
  // first emit system-policy metadata; it is not the run's input. Never leave
  // an attempt armed to misattribute a later ordinary user's agent_start.
  api.on("agent_start", () => { firstMessage = true; });
  api.on("agent_end", () => { firstMessage = false; });
  api.on("message_start", (event, ctx) => {
    if (event.message?.role === "system") return;
    const startsRun = firstMessage;
    firstMessage = false;
    if (!peer(event.message)) return;
    const attempt = event.message.details.attempt;
    if (!seen.has(attempt) || accepted.has(attempt)) return;
    if (startsRun && seen.get(attempt)) {
      status(ctx, { event: "turn_started", attempt });
      if (cancelled.has(attempt)) ctx.abort();
    }
    accepted.add(attempt);
    status(ctx, { event: "accepted", attempt });
  });
  // The public APIs have no selective custom-message dequeue operation. Prevent
  // an interrupted, not-yet-observed peer message from entering later LLM context.
  // Already observed context is never recalled. Keep tombstones after late echoes.
  api.on("context", (event) => ({
    messages: event.messages.filter((message) => !peer(message) || !cancelled.has(message.details.attempt)),
  }));
  // Pi can be cancelled during asynchronous native run preparation, before any
  // agent_start. Its custom trigger carries the real attempt, unlike OMP's
  // ordinary-prompt-only before_agent_start event.
  if (dialect === "pi") api.on("before_agent_start", (event, ctx) => {
    if (event.messageType === "custom" && peer(event.message) && cancelled.has(event.message.details.attempt)) ctx.abort();
  });
  api.registerCommand(PEER_COMMAND, {
    description: "Private DROSTJE peer delivery transport",
    handler: async (args, ctx) => {
      const envelope = JSON.parse(Buffer.from(args.trim(), "base64").toString("utf8"));
      if (envelope.v !== 1 || typeof ctx.ui?.setStatus !== "function" || typeof ctx.isIdle !== "function" || typeof ctx.abort !== "function") throw new Error("Native peer delivery unavailable: requires serialized status, idle state, and cancellation APIs");
      if (envelope.op === "hello") {
        status(ctx, { event: "ready" });
        return;
      }
      if (envelope.op === "cancel") {
        if (!uuid(envelope.attempt)) throw new Error("Invalid peer attempt");
        if (!accepted.has(envelope.attempt)) {
          if (cancelled.size >= 4096 && !cancelled.has(envelope.attempt)) throw new Error("Peer cancellation lifetime limit reached; close runtime");
          cancelled.add(envelope.attempt);
        }
        return;
      }
      if (envelope.op !== "deliver" || !uuid(envelope.attempt)) throw new Error("Invalid peer delivery");
      const { attempt, messages, content } = envelope;
      if (seen.has(attempt) || cancelled.has(attempt)) {
        status(ctx, { event: "rejected", attempt, reason: "Attempt cancelled or already dispatched; never replay" });
        return;
      }
      if (!Array.isArray(messages) || !messages.length || messages.length > 128 || typeof content !== "string" || typeof envelope.trigger_turn !== "boolean" || messages.some((m) => !m || !uuid(m.id) || !uuid(m.sender) || !uuid(m.space) || typeof m.sender_name !== "string" || typeof m.body !== "string")) {
        status(ctx, { event: "rejected", attempt, reason: "Invalid peer identities or content" });
        return;
      }
      // Bound replay/tombstone memory by failing closed, not forgetting IDs.
      if (seen.size >= 4096 || cancelled.size >= 4096) {
        status(ctx, { event: "rejected", attempt, reason: "Peer delivery lifetime limit reached; reopen explicitly" });
        return;
      }
      const nativeIdle = ctx.isIdle();
      seen.set(attempt, envelope.trigger_turn);
      status(ctx, { event: "dispatching", attempt, idle: envelope.trigger_turn && nativeIdle });
      try {
        // Steering uses native tool-call boundaries (and interruptible waits),
        // rather than OMP's aside queue, which waits for a whole agent step.
        // Never abort a tool or implement our own wake/retry loop here.
        api.sendMessage({
          customType: TYPE, content, display: true,
          ...(dialect === "omp" ? { attribution: "agent" } : {}),
          details: { v: 1, origin: "peer", attempt, messages },
        }, { deliverAs: "steer", triggerTurn: envelope.trigger_turn });
      } catch (error) {
        // Even synchronous failure can follow native admission. The lifecycle
        // observer remains armed for a correlated late echo, never a retry.
        status(ctx, { event: "unknown", attempt, reason: String(error) });
      }
    },
  });
}
