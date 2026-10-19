import { registerBrowserTools } from "./client.mjs";

// OMP injects its omptype-backed Zod builder into every extension factory.
export default function (api) {
  const z = api.zod;
  function schema(value) {
    let result;
    if (value.type === "object") {
      const properties = {};
      for (const [name, field] of Object.entries(value.properties)) {
        const child = schema(field);
        properties[name] = value.required?.includes(name) ? child : child.optional();
      }
      result = z.object(properties);
    } else if (value.enum) {
      result = z.enum(value.enum);
    } else if (value.type === "number") {
      result = z.number();
    } else if (value.type === "boolean") {
      result = z.boolean();
    } else {
      result = z.string();
    }
    return value.description ? result.describe(value.description) : result;
  }
  registerBrowserTools(api, schema);
}
