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

## Layers, domains and decoupling

These rules are shared word for word by every Pamin Labs repository. Each came from a defect found in an audit of Tala's server on 2026-11-03, and each stops that defect from coming back. Each project's `AGENTS.md` names its own layers and the tests that check them.

- **Layers point one way.** Code is layered by concern: entry, wiring, transport, domain, then store and integrations, then values. Dependencies point only from left to right, and nothing points back up. A test fails when one does:
  - in a Rust workspace, a test that reads `cargo metadata`;
  - in a single crate, a test over its modules' `use` paths;
  - in Go, the import test.

  A new module or crate goes in its layer's directory, never beside the layers.
- **Transport only translates.** A handler, middleware or CLI command does three things: it parses the request into types, calls one domain function, and shapes that function's result as the API's own types. A business rule in transport is a bug, and that includes refusals, quotas, limits, metering decisions and checks of any kind. A middleware may read the request, but it calls the function that decides, and that function declares the error.
- **Transport never reaches the store.** It uses no call, type or error from it. Even a domain function that only passes a call through is worth having, so that a rule added later has one place to go.
- **Each layer converts at its edge.** Row types never leave the store. The store returns its own plain read types, the domain converts them to its own types, and the API converts the domain's types to its own wire types. A schema change then fails to compile at a conversion instead of silently changing the API. A domain's public functions and fields take and return the domain's own types, never store or row types; the only store type a domain shows is the store handle it is built with. A type several domains and the store share is a value, not a store type.
- **Errors are declared once, with their kind and code, in the domain that owns them.** Transport answers any declared error by its kind, with no table that lists every domain's errors. A store error that transport must act on is mapped by the domain to its own error, so transport never names the store's errors.
- **A domain owns its data.** A domain calls another domain's service, never that domain's queries or tables, and the caller's domain orchestrates cross-domain work, not the store. Each store function is called from one domain only, and a test fails otherwise.
- **A domain module holds its service, its types, its errors and its rules.** It depends only on the store, on integrations through traits or interfaces it declares, and on other domains' services. It never imports an HTTP framework, MCP transport code or the API layer.
- **Wiring is in one place.** The entry point or app module builds each service once and hands each transport the services it uses. Every background task is domain code run on a schedule, under the same rules, and it is owned by the wiring that awaits it at shutdown.
- **Each handler group sees only its own services.** An API is not one handler holding every dependency. Each business area registers its own operations in its own file and receives only its own service and the caller's identity.
- **Traits and interfaces are declared by their consumer,** with only the methods it calls. Functions return concrete types.
- **One definition per concern.** Each rule, constant, limit, price, page size, setting name, header, path or schema has one definition, in the module that owns it, and everything else imports it. Before adding a helper, search for one. A second copy of a loop over an SDK is a bug: put one helper per repeated SDK flow in its integration module.
- **A rule is computed in one function,** and another domain calls that function rather than restating the rule. What a client must do first is that function's answer, not a set of flags the client has to order.
- **A counter and its refusal come from one function.** What a caller is told it has left must agree with what is refused. The "left" figure asks the same function that refuses, and a test checks that the two agree.
- **A closed set of values is one named type with one list.** Wire enums and schemas are built from that list, never written again in a tag or a string, and a test checks the list against the database's check constraint. A store value is never a wire name.
- **Modules are named for what they hold.** Names like `utils`, `common`, `models` and `helpers` are not allowed. Each module covers one concern with a small public surface and is private by default. A file covers one business area, not one type. Within a file the order is:
  1. imports;
  2. constants and statics;
  3. types;
  4. constructors;
  5. methods grouped by type, public first;
  6. plain helpers, in rough call order.

  A type is declared before the code that uses it.
- **Writes are safe to repeat.** A write that a client or a retry may send twice is idempotent: a repeat returns what the first one stored and changes nothing more. A write that also reaches another service calls that service outside the transaction, and is safe to try again, with that service's "not found" counting as done. Steps are ordered so that a failure part way leaves no gap.
- **Per-transaction state only.** A deployed PostgreSQL may sit behind a transaction-pooling proxy such as PlanetScale's PgBouncer, so a connection belongs to the code for one transaction only. Use `set local` or `set_config(name, value, true)`, and protocol-level prepared statements. Never use:
  - a session `set`, or `set_config(..., false)`, meant to outlast its transaction;
  - session advisory locks (use `pg_advisory_xact_lock` instead);
  - `LISTEN`/`NOTIFY`;
  - temporary tables;
  - SQL `PREPARE`;
  - cursors held across transactions.

  Test fixtures may use these.
- **State kept per user, tenant or key beyond a request is bounded,** like a rate limiter's buckets or a cache. Use a bounded structure with eviction, never a map pruned by hand. Per-user work that must be serialised is serialised in the database (a lease or row lock), not in one instance's memory, so instances stay stateless.
- **Hot paths keep their database round trips flat.** A request's round trips do not grow with the size of a user's data: no `count(*)`, `offset` or `max()` over a growing set per request. Keep counters, use keyset pagination with indexes that match each query, and batch independent reads.
- **Parse once.** Large values such as manifests, schemas, prompts and models are parsed once and cached by version in a bounded cache, never copied per request. Answers are streamed rather than buffered when they can be.

### Layers here

The kit is one crate, so its layers are modules under "Layout" above, and their `use` paths point one way: `serve` → `kit` → `module`, `ops` and `tasks` → `seams`.

- **Entry and wiring:** none in the crate; a module's own binary builds its `Kit` with the seams it chooses and serves it (`examples/`).
- **Transport:** `src/serve.rs`, MCP over stdio and Streamable HTTP; the only module that names `rmcp`.
- **Domain:** `src/kit.rs` (identify, check, run and meter every call), `src/module.rs`, `src/ops.rs` and `src/tasks.rs`.
- **Integrations:** `src/seams.rs`, the traits the kit declares for identity, entitlements, metering and the task store, with their open-source defaults; a host implements them.
- **Store:** none; there is no database here, and a task store is a seam its host implements, so the store rules have no subject in this crate.
- **Beside the layers:** `src/conformance.rs` (checks a module runs through the public API) and `src/trace.rs` (one span per MCP request, used by transport).

Checks: the test over the modules' `use` paths that fails when a layer points back up is not yet written; on the roadmap. The one-store-function-one-domain test has no subject here.

## Errors, data, settings and calls to other services

These rules are shared word for word by every Pamin Labs repository. They came from Tala's server rules and decisions of 2026-11-01 to 2026-11-05. Each project's `AGENTS.md` names how it meets them.

### Errors and API documents

- **Values are leaves.** A type that two layers share is declared once, in a value module that imports no service: a risk level, a usage count, a place or an identity, for example. The store reads and writes these types. It never imports a domain, a harness or transport.
- **One code, one error.** Every error a client is told about is declared once, where it is owned, with a kind (invalid, forbidden, conflict, too many, unavailable, ...) and a stable code. A code declared twice fails at start or in a test. A limit says when it lifts.
  - A business error belongs to the domain whose rule raises it. The store keeps only data errors, such as not found or busy.
  - A layer with nothing more particular to say reuses the error of its kind and adds context. It never declares the same code again.
- **An API document lists the codes its operations send, and no other.** Each operation declares the errors it answers with, and each middleware declares its own where it is installed. The document's enum of codes is built from those declarations, so a client never maps a code it cannot receive.
  - A document builds without warnings.
  - A file committed from a generator (an API document, a catalogue, a list another project reads) is checked to be current by a test or a CI step.
- **APIs are isolated.** A public API, an operator API and an internal API each have their own module, port or listener, and document. A module of one API never imports another's, and the operator API is reachable only from its own host.

### Data and transactions

- **Rules inside a transaction are the domain's.** When a rule must be judged atomically with a write, the store reads the facts inside its transaction and hands them to a function the domain passes in. The store reports counts and flags; it never decides.
  - A query takes the domain's numbers as parameters (a window's start, a limit, a reason). It never names a period, an age or a reason itself.
- **Calls to other services stay out of transactions.** Commit, then call, and make the change idempotent so a failed call can be retried. What must still happen elsewhere is recorded in the same transaction as the change. A worker retries it until it is done, with the other service's "not found" counting as done.
- **A write that charges, and that a client may send twice, takes a client id.** The id is generated by the client, the same on every attempt and unique per caller.
  - A repeat with the same id and the same request returns what was stored and charges nothing.
  - A repeat with the same id and a different request is refused.
- **Schema changes expand, then contract.** A migration adds what the new code needs and removes only what no deployed version reads, so the previous release runs on the new schema and can be rolled back to. A service starts on a schema at or above its own version.
- **Queries and migrations.**
  - One query per concern, reused rather than copied.
  - No SQL is assembled in code except test fixtures. A column that holds one of a type's values decodes straight into that type, never through a conversion written by hand.
  - Migrations are written by hand, numbered and append-only. They are checked for locking and destructive changes before they merge.

### Settings and switches

- **Settings have one source each.** A deployment setting (address, key, connection cap) is read in one place at start into a typed config. The repository's `.env.example`, or the service's own list of required names, names it once, and a test keeps the two in step. It has no default in code.
- **Product settings live in the database.** Models, prices, plans, limits, retention and similar settings are named with their defaults in one settings module. They are stored only where they differ from those defaults, changed through the operator API with a reason that is audited, and read live, never as a copy taken at start.
- **No development-only behaviour in production code.** Local runs use a stub of each service, never a back door.
- **A kill switch is a product setting, judged in one domain function** that every entry point of its kind already calls, never as a check in a handler. Operators are warned while it is on. Its refusal is a declared error that is not retryable.
- **Operator writes are audited and safe to repeat.** Every operator change carries a reason, and the change and its audit record are written in one transaction.

### Calls to other services and models

- **Every call a request waits on has its own deadline and fallback.** The deadline is named once beside the call, as a constant, or as a setting when operators tune it. It is set on the call's context or future, never left to an HTTP client's default timeouts. When it passes, it is logged and the request goes on without the result, or fails with a declared error.
- **One integration module per external service.** It wraps the service's official SDK, or a small typed client where there is none, and it holds the service's URLs and ids as its own constants. A domain that needs it declares the trait or interface it calls, and the wiring hands it the implementation. Only the integration module imports the SDK.
- **Model and inference calls have one owner.** Every call to a model goes through one layer (a harness, an inference module or a pool). It names the call's task, its budget and its settings in one list, and prices or meters each call's own usage. A new model call gets a row in the project's list of model calls, with its cost and latency measured, and an entry on the roadmap.
- **Cost is metered and watched.** Every metered unit is recorded once, by the platform, never by a module's own code. Spend that no single user causes (background work, batch jobs, re-checks) is metered too, attributed to no user. A total past an operator-set threshold raises an alert.
- **Stubs exist only for tests and local runs.** A stub of a service is reachable only from tests, the benchmark and the local runner, and a test fails when production code imports it.
- **Logs carry ids, counts and durations only.** They never carry message text, memory content, tokens or other personal data. Each request or call writes one line, joined to its trace.

### How this repository meets them

pamin-kit is a library with no database, operator API or deployment of its own. The rules above apply to it for errors, model calls, logs and deadlines; the rest has no subject here, beyond the seams through which a host meets them.

- **Errors:** every failure of a call is one `CallError` variant in `src/kit.rs`, which `src/serve.rs` answers as MCP; the variants carry messages, not yet a stable code each.
- **API documents:** the MCP tool schemas are built from each operation's contract types (`src/ops.rs`), never written by hand.
- **Calls and deadlines:** the kit calls other services only through its seams; a deadline on such a call is the host's implementation's, and the kit sets none of its own yet.
- **Models and cost:** the kit makes no model call; it hands every call to the `Metering` seam once, and pricing and alerts are the host's.
- **Stubs:** the open-source defaults in `src/seams.rs` and `InProcessTasks` are for local runs and tests; a hosted deployment replaces them.
- **Logs:** `src/trace.rs` opens one span per MCP request, with its method, tool, request id and error type, never its arguments or results.

## Rust

These rules are the same in every Pamin Labs Rust repository; change them in all of them together.

- **Toolchain and lints.** The edition and toolchain are pinned in `rust-toolchain.toml` and upgraded as their own change; code is `rustfmt`-clean and `clippy`-clean with warnings denied. A lint beyond clippy's defaults (a `clippy::pedantic` lint, `missing_docs`) is enabled in `[lints]` only after its findings on the current tree have been counted and found few and worth fixing, and the PR that enables it fixes them all.
- **Errors.** A library declares typed errors with `thiserror`, one enum per boundary with variants a caller can act on; `anyhow` or a boxed error only in binaries and tests. Errors travel with `?`, each layer adding what it was doing (a variant's field, or `.context(...)` in a binary), and are handled once: returned or logged, never both.
- **No panic reachable from input.** `unwrap`, `expect`, `panic!`, `unreachable!`, indexing or slicing that can go out of bounds, and arithmetic that can overflow stay out of every path a request, a file or another service's answer reaches. They are allowed in tests, at start-up before serving, and where an invariant rules the failure out, with an `expect` message that states the invariant.
- **`unsafe`** only where no safe API gives the result (FFI, a measured SIMD kernel), in the smallest block, with a `// SAFETY:` comment naming every invariant it relies on and a test that exercises it. A new crate that needs none starts with `#![forbid(unsafe_code)]`.
- **Borrow before cloning.** A `.clone()` added to satisfy the borrow checker on a hot path is a defect: restructure, borrow, or share an `Arc` and say why. Parameters take `&str`, `&[T]` or `impl AsRef<_>`; functions return owned values.
- **Types over strings and integers.** Ids, units and amounts are newtypes or standard types (`struct TenantId(Uuid)`, `Duration`), never a bare `String`, `i64` or `u64`; a closed set of values is an enum, never a string compared in code. Input is parsed into these types at the boundary (`serde`, `FromStr`, `TryFrom`), so inner code only sees valid values.
- **Typed JSON.** Bodies are `serde` types; walking a `serde_json::Value` by hand, `json!` for a body whose shape is known and `format!` building JSON are defects. A value a protocol requires to be echoed unchanged is a `Box<RawValue>`, and the code says why.
- **Published crates** (one another repository depends on) keep semver: a public struct or enum that may grow is `#[non_exhaustive]` or has private fields and a constructor, and a breaking change is called out in the PR and released as one. Features are additive: enabling one never removes or changes what another relies on, and the default features are the smallest set a typical user needs.
- **Dependency features.** Each dependency is declared once (in `[workspace.dependencies]` where there is a workspace) with only the features the code calls; `tokio`, `axum` and other large crates never take `full` or unused defaults. A new dependency's added clean build time (`cargo build --timings`) and release binary size are measured and recorded in the PR.
- **No blocking in async.** No blocking file or network call, `std::thread::sleep`, long CPU-bound loop or contended `std::sync` lock on an async task: use the async API, `spawn_blocking` or a dedicated thread. No lock guard is held across `.await` (clippy's `await_holding_lock` stays on); a `tokio::sync` lock only when the guard must cross one.
- **Everything queued is bounded.** Channels, queues, caches and per-key maps have a bound named once as a constant or a setting, and the code says what happens when it is reached (wait, refuse or evict); an unbounded channel is a defect outside tests.
- **Tasks are owned.** Every spawned task's `JoinHandle` is kept (or the task lives in a `JoinSet` or a `TaskTracker`) and awaited or aborted by its owner, so its panic or error is seen. Every branch of a `tokio::select!` is cancellation-safe, or a comment says why the lost work does not matter. A long-running service shuts down gracefully on SIGTERM: it stops accepting, finishes or hands over work in flight within a deadline, flushes what it buffers, then exits.
- **Shared state** is `Send + Sync`, owned by one type that keeps each lock next to the fields it guards; on a hot path prefer atomics or message passing to a lock, and never take two locks in different orders.
- **Logs** go through `tracing` with structured fields: ids, counts and durations, never a secret, a token, a request body or user content. A type that holds a secret redacts it in `Debug`.
- **SQL** (`sqlx`) binds every value (`$1`, `.bind`); a query built with `format!` from input is a defect, and what cannot be bound (an identifier) comes from a closed list in code. Migrations are append-only: a merged migration is never edited, renamed or removed; a change is a new migration.
- **Tests.** Unit tests sit beside the code in `#[cfg(test)] mod tests`; tests of a crate's behaviour through its public API go in its `tests/` directory. Tests are deterministic and independent: no `sleep` to wait for something (await the event, poll with a deadline, or pause tokio's clock), no dependence on test order, the wall clock, the network or state another test left; anything random uses a fixed seed. A fix comes with a test that fails without it.

## Performance

- **Hot paths allocate once.** Reuse buffers (`clear()` and refill, `with_capacity` from a known size); no `collect()` into a `Vec` only to iterate it once; no `to_string()` or `format!` for a value that is only compared or written out.
- **Static dispatch on hot paths** (generics, `impl Trait`); `dyn` at boundaries crossed once per request or less.
- **Connections:** a database pool's size is set explicitly, and a transaction holds its connection only for SQL, never across a call to another service.
- **Start-up** does not wait on other services: connect, fetch keys and load what can wait on first use or in the background, unless the program can answer nothing without it.

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
