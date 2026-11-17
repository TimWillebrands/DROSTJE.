export const ASK_TOOL = "ask";
export const ASK_GUIDANCE = "ask is unavailable. For choices or forms: write HTML with <form data-drostje=\"answer\"> or data-drostje-send buttons; call show_artifact with placement=\"composer\"; end your turn. Answers arrive as artifact events. For a single yes/no or short free-form question, use plain chat.";

export function registerArtifactQuestions(api) {
  if (typeof api.on !== "function") return;
  const deactivate = () => {
    try {
      if (typeof api.getActiveTools !== "function" || typeof api.setActiveTools !== "function") return;
      const active = api.getActiveTools();
      if (!Array.isArray(active) || !active.includes(ASK_TOOL)) return;
      const result = api.setActiveTools(active.filter((name) => name !== ASK_TOOL));
      if (result && typeof result.catch === "function") result.catch(() => {});
    } catch {
    }
  };
  api.on("session_start", deactivate);
  api.on("before_agent_start", deactivate);
  api.on("tool_call", (event) =>
    event?.toolName === ASK_TOOL ? { block: true, reason: ASK_GUIDANCE } : undefined);
}
