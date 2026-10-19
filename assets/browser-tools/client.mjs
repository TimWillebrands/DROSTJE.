import { readFileSync } from "node:fs";

const tools = JSON.parse(readFileSync(new URL("./tools.json", import.meta.url), "utf8"));

export function registerBrowserTools(api, schema) {
  const endpoint = process.env.DROSTJE_BROWSER_ENDPOINT;
  const thread = process.env.DROSTJE_BROWSER_THREAD;
  if (!endpoint || !thread) {
    throw new Error("DROSTJE browser tools require a configured endpoint and thread ID");
  }
  for (const tool of tools) {
    api.registerTool({
      name: tool.name,
      label: tool.name === "browser" ? "DROSTJE Browser" : "Show Artifact",
      description: tool.description,
      parameters: schema(tool.inputSchema),
      // OMP otherwise defers discovery of new names until a tool search.
      loadMode: "essential",
      async execute(_toolCallId, args, signal, _onUpdate, ctx) {
        const timeout = AbortSignal.timeout(35_000);
        const response = await fetch(endpoint, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ thread, name: tool.name, arguments: args, cwd: ctx.cwd }),
          signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
        });
        const body = await response.json();
        if (!response.ok || typeof body.error === "string") {
          throw new Error(body.error || `DROSTJE browser endpoint returned HTTP ${response.status}`);
        }
        if (!("result" in body)) throw new Error("DROSTJE browser endpoint returned no result");
        const result = body.result;
        const content = [];
        let details = result;
        if (tool.name === "browser" && args.action === "screenshot" && result.image) {
          const { image, ...metadata } = result;
          content.push({ type: "text", text: JSON.stringify(metadata) });
          content.push({ type: "image", mimeType: image.mimeType, data: image.data });
          details = metadata;
        } else {
          content.push({ type: "text", text: JSON.stringify(result) });
        }
        return { content, details };
      },
    });
  }
}
