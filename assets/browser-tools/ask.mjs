// DROSTJE asks the user through HTML artifacts whose events reach the agent, so the
// native ask tool is deactivated. A session that still calls it is refused with that
// guidance, never answered on the user's behalf.
export const ASK_TOOL = "ask";
export const ASK_GUIDANCE = "The ask tool is not available in DROSTJE. To ask the user, write an HTML page with a <form data-drostje=\"answer\"> (or data-drostje-send buttons) and call show_artifact; the user's answers arrive as artifact events. For a simple question, ask in plain text instead.";

export function registerArtifactQuestions(api) {
  if (typeof api.on !== "function") return;
  // Tool state exists only once a session runs; extension factories cannot read it.
  const deactivate = () => {
    try {
      if (typeof api.getActiveTools !== "function" || typeof api.setActiveTools !== "function") return;
      const active = api.getActiveTools();
      if (!Array.isArray(active) || !active.includes(ASK_TOOL)) return;
      const result = api.setActiveTools(active.filter((name) => name !== ASK_TOOL));
      if (result && typeof result.catch === "function") result.catch(() => {});
    } catch {
      // The tool_call guard below still refuses the tool.
    }
  };
  api.on("session_start", deactivate);
  api.on("before_agent_start", deactivate);
  api.on("tool_call", (event) =>
    event?.toolName === ASK_TOOL ? { block: true, reason: ASK_GUIDANCE } : undefined);
}
