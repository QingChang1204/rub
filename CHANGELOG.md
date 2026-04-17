
## Unreleased

### 🚀 Features

- Add authenticated runtime bindings, remembered alias reuse via `--use`, and explicit binding inspection/management commands (#operator-workflow)

- Add local secret-reference management commands plus sanitized secret provenance in workflow/extract/fill results (#operator-workflow)

- Reuse resolution state for list/inspect to reduce io load (followup/structure-debt-and-hotpath)

- Add fallback/lookup metrics to ease perf tuning (#followup/structure-debt-and-hotpath)

- Index unresolved map by lookup key to speed fallback (#followup/structure-debt-and-hotpath)

- Factor JSON registry IO helpers & avoid extra depth loops (#followup/structure-debt-and-hotpath)

- Instrument frame-runtime with cost-tracking counters, skip inventory on primary (#followup/structure-debt-and-hotpath)

- Add per-wait frame context cache to reduce redundant queries (#followup/structure-debt-and-hotpath)

- Add transient-retry backoff to wait loops (#followup/structure-debt-and-hotpath)

- Apply capped 50-200ms poll backoff ladder (#followup/structure-debt-and-hotpath)

### 🐛 Bug Fixes

- Fail closed when `--use` is passed to local-only control-plane surfaces, and cover profile-backed no-live-match reuse through browser-backed E2E (followup/structure-debt-and-hotpath)

- Harden `--profile` resolution to reject ambiguous prefix and cross-namespace exact collisions instead of silently picking one profile (followup/structure-debt-and-hotpath)

- Restore browser authority/fence integrity across active-tab selection, local tab actuation handoff, atomic tab-projection commit, continuity-backed rollback, and fail-closed browser install rollback instead of leaving half-installed or contradictory browser state visible (pre-merge authority / contract; browser-authority / commit-integrity)

- Isolate managed browser profiles per session and finish the authority migration end-to-end: session-scoped managed profile roots, alias-normalized profile identity, marker-backed temp ownership, post-commit durable ownership adoption, transaction-integrated ownership snapshot/rollback, exact-path orphan cleanup, harness strict verification, and crash recovery all now consume the same managed-profile authority model (browser-authority / commit-integrity; session-scoped managed-profile cleanup / recovery)

- Keep `close` and teardown semantics on one honest release fence: preserve selector authority for `close --use/--profile/--connect/--cdp-url`, canonicalize close attachment identity before dispatch, keep `close --all` kill fallback behind shutdown-fence failure, preserve temp-owned `RUB_HOME` release truth through cleanup, and normalize temp-home alias comparisons during stale-home sweep (pre-merge authority / idempotency; browser-authority / commit-integrity)

- Split daemon-committed truth from transport and local follow-up failures: only execution-committed commands now reach post-commit history/workflow/journal surfaces, delivery-failed-after-commit responses keep their authoritative workflow capture, and post-commit local CLI failures preserve committed daemon payloads instead of collapsing them to generic protocol loss (pre-merge authority / contract)

- Make timeout authority end-to-end instead of per-helper best effort: request materialization, existing-daemon attach/upgrade/handshake, back/forward history navigation, and command-embedded wait budgets now all consume one shared deadline rather than silently resetting phase-local timers (pre-merge authority / contract; authority / idempotency)

- Separate session-lifetime spent-command authority from bounded replay-result retention so `command_id` at-most-once semantics survive cache eviction; retries now replay cached truth when available and otherwise fail closed as already-spent instead of silently executing twice (browser-authority / commit-integrity)

- Tighten IPC and stdout contracts at their real fences: daemon ingress now rejects protocol/version/blank-command violations at decode time, response protocol mismatches surface as explicit `IPC_VERSION_MISMATCH`, post-write response timeouts stay replay-sensitive, and stdout contract checks now admit the one intentional post-commit local-failure envelope without dropping committed daemon context (pre-merge authority / idempotency; browser-authority / commit-integrity)

- Harden publication fences so committed bytes and socket identities cannot switch authority after proof: atomic file publish now commits from the authoritative synced handle instead of the original temp pathname, and stale socket cleanup quarantines the fenced socket off the bind path before deletion (browser-authority / commit-integrity)

- Align editable input authority across actuation and confirmation: typing now reuses the same focused editable fence for inputs and contenteditable targets, and confirmation reads the same shared editable projection instead of contradicting successful edits on non-`.value` surfaces (pre-merge authority / idempotency)

- Split durable lifecycle from last outcome for repeat triggers and bound background automation reservations to a real worker-cycle budget, preventing temporary failures from terminally disarming triggers and preventing background reservations from indefinitely starving foreground queue authority (pre-merge authority / idempotency)

- Align ambiguous locator, orchestration identity, and history durability contracts: `wait` now fails closed on ambiguous locator matches unless selection is explicit, omitted-key orchestration registration derives a stable request identity instead of a fresh UUID, and display-only/best-effort history projections can no longer be persisted as durable replay assets (pre-merge authority / idempotency; authority / contract)

- Remove write side effects from read-only runtime refresh paths, keep handoff escalation on explicit policy-driven mutation lanes only, and make validation/reporting surfaces honest about degradation: request-correlation eviction/TTL now publish degraded observatory signals, explicit-frame runtime snapshots mark mixed page-global cookie authority as degraded instead of pretending it is frame-local truth, and harness/browser-backed cleanup now fails by default when product teardown needed harness fallback (pre-merge authority / contract; browser-authority / commit-integrity)

- Strengthen browser-backed guardrails around replay, crash recovery, and cleanup residue so exact session-scoped managed-profile authority is what tests exercise and verify, rather than stale pid-shaped heuristics or missing-home side effects (browser-authority / commit-integrity; session-scoped managed-profile cleanup / recovery)

## v0.1.6 — 2026-04-13

### 🚀 Features

- Uplift extract spec to accept both string shorthand and fully structured object forms; introduce `NormalizedJsonSpec` to unify spec parsing across CLI and daemon surfaces

- Harden request correlation registry: replace linear unresolved scan with HashMap keyed lookup, add correlation fallback metrics for perf tuning

- Extend `automation_timeout` budget model with subcommand-level timeout classification

- Add `inspect harvest` command for batch URL inspection from structured JSON source files

### 🐛 Bug Fixes

- Fix secret redaction to cover both request args and response payloads in workflow/pipe results

- Harden orchestration action arg parsing to reject unknown fields at the spec boundary

- Fix trigger and orchestration command dispatch to correctly propagate spec validation errors

## v0.1.5 — 2026-04-12

### 🚀 Features

- Add `rub teardown` — canonical lifecycle exit that closes all sessions, waits for daemon shutdown fences, and sweeps orphaned temporary browser profiles

- Add `rub explain` subcommand surface: `explain extract`, `explain interactability`, `explain interference`, `explain locator` — agent-facing diagnostics for understanding why a command behaves a certain way

- Add `rub wait` command with full condition set: `--selector`, `--label`, `--role`, `--text`, `--url-contains`, `--title-contains`, `--description-contains`; state options: `visible`, `hidden`, `attached`, `detached`, `interactable`

- Add `rub fill --atomic` (explicit rollback on failure) and `rub fill --validate` (dry-run plan without mutating the page)

- Add `rub extract --schema` and `rub extract --examples [TOPIC]` for built-in spec documentation

- Add orchestration workflow asset persistence under `RUB_HOME/orchestrations/`; add `orchestration list-assets` and `orchestration export --save-as`

- Add named workflow persistence under `RUB_HOME/workflows/`; add `pipe --workflow <name>` and `pipe --list-workflows`

- Add `find --explain` to project candidate set through the read-only locator explain surface

- Add `state --diff <snapshot_id>` to return only elements changed since a previous snapshot

### 🐛 Bug Fixes

- Fix browser-backed runtime status and wait baselines to correctly reflect domcontentloaded vs load lifecycle

- Harden dialog intercept recovery: fix race between dialog dismiss and page teardown during fixture cleanup

- Harden orchestration dispatch: reject malformed action specs at registry boundary instead of silently no-op

- Tighten runtime guardrails on orchestration rule execution to prevent double-fire on concurrent triggers

## v0.1.4 — 2026-04-10

### 🚀 Features

- Add durable daemon-side post-commit journaling behind the commit fence; journal reads are tolerant to torn tail records

- Extend journal redaction to cover both request and response payloads

- Clarify authority boundaries between daemon commit truth, bounded post-commit projection, operator/display-only surfaces, and artifact/reference/path semantics

- Extract mixed-authority root modules into focused sub-modules around protocol, projection, args, mutation, execution, lifecycle, outcome, and reservation boundaries

- Reduce browser-backed E2E cold-start cost by grouping correctness units under shared browser/session lifecycle

## v0.1.3 — 2026-04-07

### 🐛 Bug Fixes

- Scope socket runtime dir per OS user to fix multi-user `Permission denied` on shared machines

- Strip inspect routing key at dispatcher; fix iframe element hit-test coordinate calculation

## v0.1.2 — 2026-04-07

### 🏠 Chores

- Bump workspace version to 0.1.2


### 🐛 Bug Fixes

- Support shorthand spec format and harden error UX

- Remove leaked snapshot_id/frame_id from selector error contexts

- Replace 'rub inspect' suggestions with 'rub observe'

- Preserve JSON types in whole-placeholder refs, reject duplicate labels

- Use tokio::fs for async-safe file read in import


### 🔧 Refactor

- Deduplicate JS locator helpers into LOCATOR_JS_HELPERS


### 🚀 Features

- Feat(pipe): step-result forwarding via {{prev.*}} and {{steps[N].*}} references
Implement template variable resolution in pipe step args, enabling
multi-step workflows where later steps can reference data from earlier steps.
Syntax:
  {{prev.result.PATH}}            - previous step result
  {{steps[N].result.PATH}}        - by step index
  {{steps[LABEL].result.PATH}}    - by step label
Features:
- Recursive JSON value walking (objects, arrays, strings)
- Nested path navigation with array indexing (items[0].name)
- Multiple references in a single string value
- Forward/circular reference prevention (hard error)
- Unclosed {{ treated as literal (no silent failures)
- Secret-redacted values stay redacted through references
Also:
- Add GEMINI.md to .gitignore (local agent config, not for GitHub)
- Update pipe --help with reference syntax documentation
- 14 new unit tests covering all resolver paths and edge cases
Tests: 906 passed, 0 failed; clippy clean; fmt clean

## v0.1.1 — 2026-04-07

### 🏠 Chores

- Bump workspace version to 0.1.1


### 🐛 Bug Fixes

- Align Homebrew install docs and skip docs-only workflows

- Harden CDP navigation commit semantics and settle fence integrity

- Remove paths-ignore from release.yml to match cargo-dist template

## v0.1.0 — 2026-04-06

### ⚙️ CI/CD

- Fix stale build cache causing false test failures on ubuntu

- Extend E2E timeout to 45 minutes for full orchestration test suite

- Skip CI on workflow/docs-only pushes to main


### 🏠 Chores

- Setup cargo-dist for automated release and homebrew tap


### 🐛 Bug Fixes

- Resolve linux CI unit test regressions on fresh runners

- Harden linux CI - profile-in-use error classification, upgrade probe, doctor schema refactor

- Serialize socket identity tests to prevent inode reuse race on parallel CI

- Rewrite stale socket identity test to avoid Linux tmpfs inode reuse

- Bind both sockets simultaneously to guarantee distinct inodes in test

- Zero-panic hardening + CI pipeline optimization

- Downgrade GitHub Actions to @v4 (cargo-dist compat)

- Upgrade cargo-dist to v0.31.0, fix runner to ubuntu-22.04, update actions to latest versions

- Remove Windows target; skip CI on workflow/docs-only pushes


### 📖 Documentation

- Comprehensive README overhaul with Homebrew install and architecture deep-dive

- Rewrite Why rub section with real agent measurements and experience


### 🚀 Features

- Initial open-source release v0.1.0
