import assert from "node:assert/strict";
import test from "node:test";
import { ASK_GUIDANCE, ASK_TOOL, registerArtifactQuestions } from "./ask.mjs";

// The real extension entrypoint against the extension API surface it relies on.
function fixture(active) {
  const handlers = new Map();
  const api = {
    on: (name, handler) => handlers.set(name, [...(handlers.get(name) ?? []), handler]),
    getActiveTools: () => [...active],
    setActiveTools: (names) => { active = names; },
  };
  registerArtifactQuestions(api);
  const emit = (name, event = {}) => (handlers.get(name) ?? []).map((handler) => handler(event, {}));
  return { emit, active: () => active };
}

test("session start deactivates only the native ask tool", () => {
  const f = fixture(["read", "bash", ASK_TOOL, "show_artifact"]);
  f.emit("session_start");
  assert.deepEqual(f.active(), ["read", "bash", "show_artifact"]);
  f.emit("before_agent_start");
  assert.deepEqual(f.active(), ["read", "bash", "show_artifact"], "idempotent");
});

test("a call that still reaches ask is refused toward artifacts", () => {
  const f = fixture(["read"]);
  const [refused] = f.emit("tool_call", { toolName: ASK_TOOL, input: { questions: [] } });
  assert.deepEqual(refused, { block: true, reason: ASK_GUIDANCE });
  assert.match(ASK_GUIDANCE, /show_artifact/);
  const [allowed] = f.emit("tool_call", { toolName: "read", input: {} });
  assert.equal(allowed, undefined);
});

test("runtimes without tool control keep the refusal guard", () => {
  const handlers = new Map();
  registerArtifactQuestions({
    on: (name, handler) => handlers.set(name, handler),
    getActiveTools: () => { throw new Error("runtime not bound"); },
    setActiveTools: () => {},
  });
  assert.doesNotThrow(() => handlers.get("session_start")({}, {}));
  assert.equal(handlers.get("tool_call")({ toolName: ASK_TOOL }, {}).block, true);
});
