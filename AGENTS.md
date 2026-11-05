# Repository Instructions

This repository holds `pamin-kit`, the open-source framework every Pamin module is built on: the `Module` trait, operation registration with entitlements, MCP serving over stdio and Streamable HTTP, the identity, entitlement, metering and task seams with open-source defaults, MCP Apps helpers and the conformance suite. It is published to crates.io.

## Source of truth

- Specification, decisions and plans live in the private InternalDocs repository under `pamin-kit/` (start with its `README.md`), plus the shared extension architecture in `pamin-platform/decisions/`. InternalDocs is a sibling clone at `$PAMIN_LABS_DEV_INTERNAL_DOCS` (default `../InternalDocs`); pull it before reading. Read the relevant documents before changing behaviour.
- When a design or decision changes, update InternalDocs in a PR there; cite it from here with SHA permalinks.
- This repository is public. Keep private planning, pricing, strategy and anything about paid or closed modules out of it; that lives in InternalDocs.

## Rules

- Everything committed is English: code, comments, logs, commit messages, PR titles and bodies, documents.
- Conventional Commits with a required scope for commits and PR titles (`fix(engine): ...`), one logical change per commit, each building and passing on its own. A commit's first line and a PR title are at most 72 characters, scope included: the pamin-labs-dev hook refuses a longer commit and CI fails a longer title, and a pushed commit is not rewritten without the owner, so count before committing.
- Coordinate through pamin-labs-dev before editing. With the plugin, its hooks and the `protocol` skill guide you. Without it, declare an intent with the `pamin-labs-dev` CLI first (`pamin-labs-dev intent declare ...`), work on its branch `<type>/i-<number>-<slug>` (no person or tool names), and put `Intent: I-<number>` in the PR description; CI rejects PRs without a live intent for their branch. Intents of the products this one uses appear in the session briefing; check them before changing code that depends on them.
- Keep PRs focused on one concern and reviewable in one sitting; split large work into a stack of PRs, each mergeable on its own. PR descriptions have a summary, the changes, what it reused, how it was verified, and any risk or follow-up.
- Commits and PRs carry no agent attribution: no `Co-Authored-By` trailers, no session URLs, no "Generated with" footers. Some tools append such a footer when they create or edit a PR, so read the description back after every such call and remove it.
- The commit author is always the engineer's own git identity; never invent one. If it is missing, stop and ask, then save it as `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL` in the environment settings.
- Never squash-merge; keep every commit.
- **A PR merges only when its final commit is proven** (owner, 2026-11-05): GitHub reports it clean, every check has passed (a failed or pending check blocks it, however small the change), its tests pass, and, when it changes a service or binary that is deployed or shipped, an end-to-end check has passed on that same final commit: a local acceptance run or staging, never a run from before the last review fix.
- Work found and not done (a review's follow-ups, an audit's findings, a flaky test) goes into the maintainers' roadmap in InternalDocs in the same session, and a decision the owner makes goes into a decision record there before or with the change, so nothing lives only in a conversation.
- Never write tokens, keys or credentials to files, commits or logs.
- Prefer `rtk <command>` for shell commands when `rtk` is available.

## Code

The bar is high performance, low resource use, fast builds, and clean, DRY, decoupled code that is easy to maintain and extend. The `pamin-engineering` skill says how to meet it; follow it for every change.

- One implementation per concern: search for an existing one before adding logic, and extend it through its interface.
- Do not reinvent what exists: the repository's own implementation of the concern, then the official SDK of a service, then the standard library, then the platform, then a dependency already in the tree, then a maintained, widely used dependency; write your own only when none of them meets the need. A PR's "Reused" section names what it searched for, what it reused, and why anything was written by hand; hand-written code where one of those already covers the need is a defect, and review sends it back. This crate is a library: require dependencies with caret minimums and let lock files pin exact versions; pin exactly only with a stated reason, as for `rmcp` (InternalDocs `pamin-platform/decisions/2026-11-01-library-dependency-requirements.md`).
- No speculative abstractions, options or layers; add them when a second real use appears.
- **Model external data with types.** Parse external JSON (requests, MCP results, other services' answers, webhooks) into typed structs and enums that ignore unknown fields, and serialize requests from typed structs. No hand-built JSON strings, no chains of untyped lookups, no searching payloads for substrings. A value stays opaque only when a protocol requires it to be echoed back unchanged, and the code says so.
- **Tests meet the same bar as code.** A test builds requests from typed values, decodes an answer into the API's own types and compares JSON by decoding it, never as strings, and names a value owned elsewhere from its owner, like any code.
- **No deferral inside a PR's own code.** A written rule that code the PR adds or touches breaks is fixed in that PR, in its own commit; a follow-up is only work outside the PR's concern.
- **Comments claim only what the code guarantees.** A guarantee that waits on later work is named as not yet given.
- Before adding a dependency, check its maintenance, its licence and what it adds to build time and binary size, and record that in the PR.
- Measure before and after any change that claims a speed-up or a smaller footprint, and put the numbers in the PR.
- **No hard-coded value in more than one place.** Every port, address, URL, id, version, limit, price, file name and default is defined once, in the crate or file that owns it, and everything else reads it from there or is checked against it by a test or a generator. A literal that names a value owned elsewhere is a bug, in code, config, scripts, tests and docs alike.
- **Deployments have no defaults.** A setting a deployed binary or service must be given (a database, keys, other services' addresses) has no fallback value in code: a missing or empty one stops the program at start, naming itself. Only local use names local values (a cache directory, a local socket), each once.
- **Built files are never committed:** `target/`, binaries, benchmark outputs and generated caches are git-ignored; only generated files a gate checks are committed, such as the conformance snapshots.
- **Extensibility check before every PR:** adding one more of something the code already has (a transport, a seam, an operation option, a conformance check) touches its one definition plus its tests and docs. If the change needed more places, fold them into one source in the same PR.
- The kit is the only crate in the system that names `rmcp`; an MCP specification or SDK change happens here once.
- Nothing module-specific enters the kit, and a feature enters only when a module needs it.
- Per-call overhead (entitlement check, metering, dispatch) stays under its benchmarked budget.

## Layout

The crate is one project in one language, so this file holds all its rules. **A directory says what its code is:** this list names every module and folder by its path, with what it owns, and a new one is described here in the PR that adds it, never placed beside them without a line; no test reads this list yet, so review checks it.

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

## Review before every PR

Before a PR opens, go through its whole diff, tests included, against each rule of every `AGENTS.md` the change touches and the `pamin-engineering` skill, and fix what misses in new commits; then run the `pamin-labs-dev:peer-review` skill and put its verdict and findings under the PR's verification. Work an agent delegated is reviewed the same way: the agent that opens the PR reads its diff and tests itself and never accepts them on the delegate's report alone.

## Gates before every push

`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`.

## Skills

Longer guidance lives in `.agents/skills/`, one directory per skill; `.claude/skills/` links to the same directories.

| Skill | Use it when |
| --- | --- |
| `pamin-engineering` | Writing or changing code, adding a dependency, claiming a speed-up, reviewing |
| `pamin-module` | Adding or changing a module, an operation or its schema, manifests, entitlements, metering, Tasks or MCP Apps |
