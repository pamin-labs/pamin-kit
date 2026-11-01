---
name: pamin-module
description: How to build or change a Pamin module (an extension such as memory or image generation) on pamin-kit and pamin-runtime so it plugs into every MCP host without host changes. Use it whenever you add a module, add or change a module operation or its schema, touch the Module trait, manifests, entitlements, metering, Tasks or MCP Apps, or are tempted to add module-specific code to a gateway or host.
---

# Building a Pamin module

The architecture is recorded in InternalDocs: `pamin-platform/decisions/2026-11-01-extension-architecture.md` and the checklist `pamin-platform/current/architecture/0.1.0/extension-guide.md`. Read both before structural changes. This skill is the working summary.

## The shape every module has

```
adapters (CLI, MCP stdio, MCP HTTP, REST when a caller needs it)
        → contract (serde + schemars types, one request and response per operation)
        → engine (plain Rust: no MCP, HTTP, auth or billing types)
        → pamin-runtime (only if the module runs models locally)
pamin-kit sits beside the adapters: Module trait, serving, seams, Tasks, MCP Apps, conformance.
```

## Rules that keep modules pluggable

- **Register, do not implement, cross-cutting concerns.** `ops.tool(name, feature, handler)`: the kit checks entitlements and records metering around every call. Handlers contain neither.
- **No tenant, user or project in arguments.** Read the space from the context the kit's `Identity` seam resolved. The conformance suite fails a schema that has such a field.
- **Describe, do not special-case.** Routing class, long-running flag, scopes, metering unit and hardware needs go in the manifest. If a gateway or host seems to need `if module == ...`, add a manifest field to the kit instead.
- **Long work returns a task** (MCP Tasks) and stores large results outside the response (object storage, signed URLs).
- **UI travels with the tool** as an MCP Apps resource, so every host renders it the same way.
- **Paid behaviour is composed, not flagged.** Open-source crates expose a few explicit extension points; private crates plug into them at build time. Plans and licences are entitlements checked at run time.

## Serving a product as MCP with the kit

The kit's `README.md` lists every registration, context method, seam and feature; [`examples/echo`](../../../examples/echo/main.rs) uses each of them. The steps:

1. **Contract crate:** one `Deserialize + JsonSchema` arguments type per operation, its doc comment the description a model reads; results `Serialize + JsonSchema`. No tenant, user or project fields.
2. **`impl Module`:** the manifest, then `register`: `ops.tool` for quick operations, `ops.task` for long ones (and `long_running: true`), `resource`, `prompt`, `complete`, `app` as the product needs. Names: tools and prompts `<module>_<action>`, resources `<module>://...`, app pages `ui://<module>/...`.
3. **Handlers call the engine** with `ctx.space()`; they check `ctx.is_cancelled()` between steps of long work, report `ctx.progress`, `ctx.ask` before anything the user must confirm, `ctx.updated(uri)` after changing what a resource shows, and `ctx.attach` for images or links beside the result.
4. **Binary:** `Kit::new(module, Local::from_working_directory()?).serve_stdio()` locally; hosted, the platform's seams with `serve_http` (feature `http`).
5. **CI:** `pamin_kit::conformance!(Module, "SKILL.md")` in a test, with feature `conformance` in dev-dependencies; review the snapshot it writes.

Anything an MCP feature needs belongs in the kit, never in the product: if the kit lacks it, add it to the kit for every module.

## Changing a contract

- Additive only within a major version: new optional fields pass; removing a field or changing a type or meaning needs a new major contract version.
- Update the schema snapshots (`insta`) in the same commit and say why in the PR.
- Renames keep the old name for a deprecation period; remove it only when metering shows no use.
- Update the module's `SKILL.md` so it still names every tool; CI checks it.

## Done means

- Conformance suite passes; schema snapshots reviewed.
- Hosts and the gateway needed no code change.
- Latency of the operation through the kit measured against calling the engine directly; the kit's overhead stays under its budget.
