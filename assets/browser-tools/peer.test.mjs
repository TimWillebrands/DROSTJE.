import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";
import { PEER_COMMAND, registerPeerDelivery } from "./peer.mjs";

// The real extension entrypoint and lifecycle handlers, not a provider or an
// acceptance stand-in. Native process evidence is a separate sandboxed probe.
function fixture(dialect, idle) {
  const handlers = new Map(), statuses = [], sends = [];
  let command, aborts = 0, fail;
  const ctx = {
    isIdle: () => idle,
    abort: () => { aborts++; },
    ui: { setStatus: (_key, text) => statuses.push(JSON.parse(text)) },
  };
  registerPeerDelivery({
    on: (name, handler) => handlers.set(name, handler),
    registerCommand: (name, entry) => { assert.equal(name, PEER_COMMAND); command = entry.handler; },
    sendMessage: (message, options) => { sends.push({ message, options }); if (fail) throw fail; },
  }, dialect);
  const envelope = () => ({
    v: 1, op: "deliver", attempt: randomUUID(), trigger_turn: true, content: "untrusted peer context",
    messages: [{ id: randomUUID(), sender: randomUUID(), sender_name: "Peer", space: randomUUID(), body: "context" }],
  });
  const control = (value) => command(Buffer.from(JSON.stringify(value)).toString("base64"), ctx);
  const emit = (event, value = {}) => handlers.get(event)?.(value, ctx);
  const native = (message) => ({ ...message, role: "custom" });
  return { statuses, sends, envelope, control, emit, native, aborts: () => aborts, fail: (error) => { fail = error; } };
}

for (const dialect of ["omp", "pi"]) {
  test(`${dialect}: busy delivery requests the native earliest safe steering boundary`, async () => {
    const f = fixture(dialect, false), e = f.envelope();
    f.emit("agent_start");
    f.emit("message_start", { message: { role: "user", content: "ongoing work" } });
    await f.control(e);
    assert.equal(f.sends.length, 1, "dispatch cannot wait for agent_end");
    assert.deepEqual(f.sends[0].options, { deliverAs: "steer", triggerTurn: true });
    assert.equal(f.statuses[0].idle, false, "busy context does not claim a new peer turn");
    assert.equal(f.statuses.some(s => s.event === "accepted"), false);
    f.emit("message_start", { message: f.native(f.sends[0].message) });
    assert.deepEqual(f.statuses.map(s => s.event), ["dispatching", "accepted"]);
    assert.equal(f.statuses.some(s => s.event === "turn_started"), false);
  });

  test(`${dialect}: authorized idle delivery can wake once without replaying uncertain admission`, async () => {
    const f = fixture(dialect, true), e = f.envelope();
    f.fail(new Error("dispatch outcome unknown"));
    await f.control(e);
    assert.deepEqual(f.sends[0].options, { deliverAs: "steer", triggerTurn: true });
    assert.equal(f.statuses[0].idle, true);
    assert.equal(f.statuses.at(-1).event, "unknown");
    await f.control(e);
    assert.equal(f.sends.length, 1, "an uncertain dispatch must not be resent");
    assert.equal(f.statuses.at(-1).event, "rejected");
  });

  test(`${dialect}: void dispatch never acknowledges admission; actual first custom lifecycle does`, async () => {
    const f = fixture(dialect, true), e = f.envelope();
    await f.control(e);
    assert.deepEqual(f.statuses.map(s => s.event), ["dispatching"]);
    f.emit("agent_start");
    f.emit("message_start", { message: { role: "system", content: "real native startup policy metadata" } });
    assert.equal(f.statuses.some(s => s.event === "turn_started"), false);
    f.emit("message_start", { message: f.native(f.sends[0].message) });
    assert.deepEqual(f.statuses.map(s => s.event), ["dispatching", "turn_started", "accepted"]);
    await f.control(e);
    assert.equal(f.sends.length, 1);
    assert.equal(f.statuses.at(-1).event, "rejected");
  });

  test(`${dialect}: busy context and native-idle race retain distinct turn attribution`, async () => {
    const f = fixture(dialect, false), e = f.envelope();
    f.emit("agent_start");
    f.emit("message_start", { message: { role: "system", content: "policy is not user input" } });
    f.emit("message_start", { message: { role: "user", content: "ordinary user" } });
    await f.control(e);
    f.emit("message_start", { message: f.native(f.sends[0].message) });
    assert.equal(f.statuses.some(s => s.event === "turn_started"), false);
    const raced = f.envelope();
    await f.control(raced);
    f.emit("agent_start");
    f.emit("message_start", { message: f.native(f.sends[1].message) });
    assert.equal(f.statuses.find(s => s.event === "turn_started").attempt, raced.attempt);
  });

  test(`${dialect}: exceptions retain correlation without attributing unrelated user starts`, async () => {
    const f = fixture(dialect, true), e = f.envelope();
    f.fail(new Error("native admission uncertain"));
    await f.control(e);
    assert.equal(f.statuses.at(-1).event, "unknown");
    f.emit("agent_start");
    f.emit("message_start", { message: { role: "user", content: "not a peer" } });
    assert.equal(f.statuses.some(s => s.event === "turn_started"), false);
    f.emit("agent_start");
    f.emit("message_start", { message: f.native(f.sends[0].message) });
    assert.equal(f.statuses.at(-1).event, "accepted");
    assert.equal(f.statuses.at(-2).attempt, e.attempt);
  });

  test(`${dialect}: cancellation tombstones survive late message echoes and replay`, async () => {
    const f = fixture(dialect, true), e = f.envelope();
    await f.control(e);
    await f.control({ v: 1, op: "cancel", attempt: e.attempt });
    const message = f.native(f.sends[0].message);
    f.emit("agent_start");
    f.emit("message_start", { message });
    assert.equal(f.aborts(), 1);
    assert.deepEqual(f.emit("context", { messages: [message, { role: "user" }] }).messages, [{ role: "user" }]);
    await f.control(e);
    assert.equal(f.sends.length, 1);
    assert.equal(f.statuses.at(-1).event, "rejected");
  });

  test(`${dialect}: actual context-only false never requests an idle turn`, async () => {
    const f = fixture(dialect, true), e = { ...f.envelope(), trigger_turn: false };
    await f.control(e);
    f.emit("message_start", { message: f.native(f.sends[0].message) });
    assert.equal(f.statuses.some(s => s.event === "turn_started"), false);
  });

  test(`${dialect}: accepted context is not recalled by later cancellation`, async () => {
    const f = fixture(dialect, false), e = f.envelope();
    await f.control(e);
    const message = f.native(f.sends[0].message);
    f.emit("message_start", { message });
    await f.control({ v: 1, op: "cancel", attempt: e.attempt });
    assert.deepEqual(f.emit("context", { messages: [message] }).messages, [message]);
  });
}
