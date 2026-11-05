# Repository Instructions

This repository holds `pamin-kit`, the open-source framework every Pamin module is built on: the `Module` trait, operation registration with entitlements, MCP serving over stdio and Streamable HTTP, the identity, entitlement, metering and task seams with open-source defaults, MCP Apps helpers and the conformance suite. It is published to crates.io.

## Source of truth

- Specification, decisions and plans live in the private InternalDocs repository under `pamin-kit/` (start with its `README.md`), plus the shared extension architecture in `pamin-platform/decisions/`. InternalDocs is a sibling clone at `$PAMIN_LABS_DEV_INTERNAL_DOCS` (default `../InternalDocs`); pull it before reading. Read the relevant documents before changing behaviour.
- When a design or decision changes, update InternalDocs in a PR there; cite it from here with SHA permalinks.
- This repository is public. Keep private planning, pricing, strategy and anything about paid or closed modules out of it; that lives in InternalDocs.

## Rules

You must follow `.agents/skills/pamin-engineering/references/core.md`: @.agents/skills/pamin-engineering/references/core.md
You must follow `.agents/skills/pamin-engineering/references/rust.md`: @.agents/skills/pamin-engineering/references/rust.md
You must follow `.agents/skills/pamin-engineering/references/performance.md`: @.agents/skills/pamin-engineering/references/performance.md

- This crate is a library, so `core.md`'s rule for published libraries applies; it pins `rmcp` exactly, for the reason in InternalDocs `pamin-platform/decisions/2026-11-01-library-dependency-requirements.md`.
- The kit is the only crate in the system that names `rmcp`; an MCP specification or SDK change happens here once.
- Nothing module-specific enters the kit, and a feature enters only when a module needs it.
- Per-call overhead (entitlement check, metering, dispatch) stays under its benchmarked budget.
- Extending the kit means one more transport, seam, operation option or conformance check: `core.md`'s extensibility check applies to each.
- Generated files committed here are the conformance snapshots, which a gate checks.

## How the shared rules apply here

- The coordination tool is pamin-labs-dev: its `protocol` skill, its `pamin-labs-dev:commit` skill for commits and PRs, and `pamin-labs-dev:peer-review` for the review before every PR, which also checks the change against the `pamin-engineering` skill.
- The roadmap and decision records are in InternalDocs; a published library's caret minimums are InternalDocs `pamin-platform/decisions/2026-11-01-library-dependency-requirements.md`.
- Prefer `rtk <command>` for shell commands when `rtk` is available.
- Provenance: the layer and domain rules came from an audit of Tala's server on 2026-11-03, and the errors, data, settings and calls rules from Tala's decisions of 2026-11-01 to 2026-11-05; the owner set the reuse ladder and the review before every PR on 2026-11-04 and the proven-final-commit merge rule on 2026-11-05.
- A module served by the Pamin platform is metered by the platform, never by its own code.

## Layout

The crate is one project in one language, so this file and the references it imports hold all its rules. **A directory says what its code is:** this list names every module and folder by its path, with what it owns, and a new one is described here in the PR that adds it, never placed beside them without a line; no test reads this list yet, so review checks it.

- `src/lib.rs`: the crate's documentation, its modules and its public re-exports.
- `src/kit.rs`: a module with its seams, the one place every call is identified, checked, run and metered, whatever transport it arrived on.
- `src/module.rs`: what a module is to the kit, its manifest and the operations it registers.
- `src/ops.rs`: operation registration, schemas from contract types and handlers erased to one shape.
- `src/seams.rs`: the identity, entitlement, metering and task seams, with their open-source defaults.
- `src/serve.rs`: serving a `Kit` as MCP over stdio and Streamable HTTP; the only module that names `rmcp`.
- `src/tasks.rs`: long operations as MCP tasks.
- `src/trace.rs`: one span per MCP request, joined to the caller's trace with the `otel` feature.
- `src/conformance.rs`: the checks every module runs in CI (`conformance!`).
- `tests/`: integration tests through the public API, helpers in `tests/common/`, snapshots in `tests/snapshots/`; `benches/`: the per-call overhead benchmarks; `examples/`: a minimal module.

Business logic lives in `src/kit.rs` and the seams it calls; `src/serve.rs` only adapts a transport to it, and a rule found there is a bug.

## Layers here

The kit is one crate, so its layers are modules under "Layout" above, and their `use` paths point one way: `serve` → `kit` → `module`, `ops` and `tasks` → `seams`.

- **Entry and wiring:** none in the crate; a module's own binary builds its `Kit` with the seams it chooses and serves it (`examples/`).
- **Transport:** `src/serve.rs`, MCP over stdio and Streamable HTTP; the only module that names `rmcp`.
- **Domain:** `src/kit.rs` (identify, check, run and meter every call), `src/module.rs`, `src/ops.rs` and `src/tasks.rs`.
- **Integrations:** `src/seams.rs`, the traits the kit declares for identity, entitlements, metering and the task store, with their open-source defaults; a host implements them.
- **Store:** none; there is no database here, and a task store is a seam its host implements, so the store rules have no subject in this crate.
- **Beside the layers:** `src/conformance.rs` (checks a module runs through the public API) and `src/trace.rs` (one span per MCP request, used by transport).

Checks: the test over the modules' `use` paths that fails when a layer points back up is not yet written; on the roadmap. The one-store-function-one-domain test has no subject here.

## How this repository meets the errors, data, settings and calls rules

pamin-kit is a library with no database, operator API or deployment of its own. The rules above apply to it for errors, model calls, logs and deadlines; the rest has no subject here, beyond the seams through which a host meets them.

- **Errors:** every failure of a call is one `CallError` variant in `src/kit.rs`, which `src/serve.rs` answers as MCP; the variants carry messages, not yet a stable code each.
- **API documents:** the MCP tool schemas are built from each operation's contract types (`src/ops.rs`), never written by hand.
- **Calls and deadlines:** the kit calls other services only through its seams; a deadline on such a call is the host's implementation's, and the kit sets none of its own yet.
- **Models and cost:** the kit makes no model call; it hands every call to the `Metering` seam once, and pricing and alerts are the host's.
- **Stubs:** the open-source defaults in `src/seams.rs` and `InProcessTasks` are for local runs and tests; a hosted deployment replaces them.
- **Logs:** `src/trace.rs` opens one span per MCP request, with its method, tool, request id and error type, never its arguments or results.

## Gates before every push

`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`.

## Skills

Longer guidance lives in `.agents/skills/`, one directory per skill; `.claude/skills/` links to the same directories.

| Skill | Use it when |
| --- | --- |
| `pamin-engineering` | Writing or changing code, adding a dependency, claiming a speed-up, reviewing |
| `pamin-module` | Adding or changing a module, an operation or its schema, manifests, entitlements, metering, Tasks or MCP Apps |
