import { Type } from "@sinclair/typebox";
import { registerArtifactQuestions } from "./ask.mjs";
import { registerBrowserTools } from "./client.mjs";
import { registerPeerDelivery } from "./peer.mjs";

// Pi aliases @sinclair/typebox to its installed TypeBox implementation.
export default function (api) {
  function schema(value) {
    const options = value.description ? { description: value.description } : {};
    if (value.type === "object") {
      const properties = {};
      for (const [name, field] of Object.entries(value.properties)) {
        const child = schema(field);
        properties[name] = value.required?.includes(name) ? child : Type.Optional(child);
      }
      return Type.Object(properties, { additionalProperties: false });
    }
    if (value.enum) return Type.Union(value.enum.map((item) => Type.Literal(item)), options);
    if (value.type === "number") return Type.Number(options);
    if (value.type === "boolean") return Type.Boolean(options);
    return Type.String(options);
  }
  registerBrowserTools(api, schema);
  registerArtifactQuestions(api);
  registerPeerDelivery(api, "pi");
}
