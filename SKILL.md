---
name: rub-browser
description: >
  Use rub for browser work in this repo. Prefer rub over raw CDP, Playwright,
  curl, or exec whenever a stable rub surface exists. Covers isolated RUB_HOME
  workflows, observation, explain surfaces, typed waits, structured extract/list
  reads, network/storage/download inspection, authenticated runtime reuse,
  handoff/takeover, and canonical teardown.
---

# rub-browser

Use this skill whenever browser context matters in this repo.

rub is the default browser authority here. The main failure mode is not “rub
cannot do it”; it is choosing the wrong surface or skipping the right fence.

## How to invoke rub in this repo

Examples below use `rub ...` shorthand. In practice, prefer one of:

- `cargo run -q -p rub-cli -- ...`
  Best during active development because it always matches the current source tree.
- `./target/release/rub ...`
  Best when a fresh release build already exists and you want lower startup cost.

## Load only what you need

Use this file for the default operating rules. Read the references only when needed:

- [references/command-selection.md](references/command-selection.md)
  When you know the task but are unsure which rub surface fits best.
- [references/recovery-playbook.md](references/recovery-playbook.md)
  When commands succeed but the workflow does not advance, or when failures are noisy.

## Non-Negotiables

Follow these unless there is a clear, explicit reason not to:

1. Isolate one workflow per `RUB_HOME`.
2. Fix the browser connection mode once and keep it stable.
3. Observe before acting.
4. Explain before guessing.
5. Verify results with current runtime evidence instead of inferring from `success: true`.
6. Use typed waits before shell polling.
7. Teardown when done.

## Authority Planes

Pick the right plane before you pick a command.

| Plane | What it owns | Default surfaces |
|------|---------------|------------------|
| Observation / snapshot | What the page currently projects as the shared orientation snapshot | `observe`, `state --format a11y`, `state --format compact` |
| Tab / window authority | Which tab currently owns the work | `tabs`, `switch`, `close-tab` |
| Interactive locator | Candidate ordering and target selection for interactive elements | `explain locator`, `find --explain`, semantic locators |
| Content / extraction | Live text/content anchors and structured reads | `find --content`, `get`, `inspect text/html/value`, `extract`, `inspect list`, `inspect harvest` |
| Interaction / mutation | Actual page mutation attempts | `click`, `type`, `select`, `upload`, `fill`, `keys`, `hover` |
| Evidence / aftermath | Whether the workflow really advanced | `wait`, `inspect network`, `inspect curl`, `inspect storage`, `downloads`, `workflow_continuity` |
| Runtime / observability | Why the runtime is noisy, degraded, or blocked | `doctor`, `inspect page`, `history`, `runtime *`, `explain blockers` |
| Runtime continuity | Which existing browser/profile/session should continue the work | `--profile`, `--user-data-dir` |
| Human control | Whether a human must temporarily own the session | `handoff *`, `takeover *` |

Use the plane that already owns the question. Do not stay in the DOM plane when
network/storage/downloads already carry the stronger evidence.
If the task is obviously content-first, start in the content plane instead of
spending a cycle in generic page understanding.

## Never Do These First

These are the most common agent mistakes:

1. Do not start by guessing a click/type target before observing.
2. Do not treat one observation as proof that a later live locator will resolve the same way.
3. Do not keep using a volatile page after route/readiness changes without reacquiring authority.
4. Do not default to `exec` for reads, fills, or common workflows.
5. Do not assume “success” means the business workflow is done.
6. Do not share one `RUB_HOME` across unrelated or parallel tasks.

## Golden Path

This is the default lane unless the task clearly calls for something else.

### 1. Isolate the runtime

```bash
export RUB_HOME=$(mktemp -d /tmp/rub-XXXX)
```

One `RUB_HOME` should own one serial workflow.

### 2. Fix transport first; reuse profile and presentation deliberately

Treat these as separate decisions:

- transport / attachment authority:
  - default managed Chrome
  - `--cdp-url`
  - `--connect`
- profile / runtime reuse:
  - `--profile`
  - `--user-data-dir`
- presentation:
  - `--headed`

Keep the transport stable unless the authority itself changes. Reuse profile and
presentation deliberately; they are not a second attach mode.

### 3. Open and observe first

```bash
rub open https://example.com --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
rub doctor --rub-home "$RUB_HOME"
```

For heavy public sites, prefer:

```bash
rub open https://example.com \
  --load-strategy domcontentloaded \
  --rub-home "$RUB_HOME"
```

Use `domcontentloaded` first on news sites, forums, and other long-lived pages
where `networkidle` over-waits.

### 4. Choose the right plane before guessing

```bash
rub explain locator --label "Submit" --rub-home "$RUB_HOME"
rub explain interactability --label "Submit" --rub-home "$RUB_HOME"
rub explain blockers --rub-home "$RUB_HOME"
```

If the job is really content discovery rather than interactive targeting, pivot:

```bash
rub find --content --target-text "Quarterly report" --rub-home "$RUB_HOME"
rub get text --selector "h1" --rub-home "$RUB_HOME"
```

### 5. Act at the highest stable surface

Prefer:

- `fill` over repeated `type`
- `extract` / `inspect list` / `inspect harvest` over `exec`
- `pipe`, `trigger`, or `orchestration` over shell loops

### 6. Verify the actual effect

Use current runtime evidence:

```bash
rub wait --url-contains "dashboard" --rub-home "$RUB_HOME"
rub wait --label "Email" --description-contains "confirm" --rub-home "$RUB_HOME"
rub inspect list --collection ".mail-row" --field "subject=text:.subject" \
  --wait-field subject --wait-contains "Confirm your account" \
  --rub-home "$RUB_HOME"
rub inspect network --wait --match "/api/" --rub-home "$RUB_HOME"
```

Also read `workflow_continuity` in command results.

### 7. Teardown when done

```bash
rub teardown --rub-home "$RUB_HOME"
```

Use `teardown` as the canonical lifecycle exit.

## Choose the Right Lane

Start from task intent, not command names.

| Intent | First choice | Escalate to | Avoid first |
|--------|--------------|-------------|-------------|
| Understand the page | `observe` | `state --format a11y`, `state --format compact`, `inspect page` | screenshots alone, `exec` |
| Establish tab/window authority | `tabs`, then `switch <index>` | `close-tab`, re-observe | guessing inside the wrong tab |
| Find content anchors | `find --content` | `get text`, `inspect text`, `extract`, `inspect list` | guessed clicks |
| Validate a target | `explain locator` / `find --explain` | `state` with stronger locator choices | blind `--first/--last` |
| Establish frame authority | `frames`, then `frame <index>` | `frame --name`, `frame --top`, re-observe | guessing inside the top frame |
| Understand why a target is blocked | `explain interactability` | typed `wait`, `explain blockers` | repeated clicks/types |
| Understand page-level blockers | `explain blockers` | `interference recover`, `handoff`, `takeover` | guessing site behavior |
| Read one value | `get title/html/text/value/attributes/bbox` | `inspect *`, `extract` | `exec` |
| Extract structured data | `extract` | `inspect list`, `inspect harvest` | raw DOM scripting |
| Confirm external side effects | `inspect network`, `inspect curl`, `inspect storage`, `downloads` | `runtime observatory`, `runtime readiness` | guessing from UI text alone |
| Fill a form | `fill --validate`, then `fill` | `type`, `select`, `upload` | many uncoordinated `type`s |
| Run one interaction | `click`, `type`, `select`, `hover`, `keys`, `upload` | `--wait-after-*`, `--topmost` | `exec` |
| Wait for a change | `wait`, `inspect list --wait-field`, `inspect network --wait` | `runtime readiness` | shell loops |
| Run bounded automation | `pipe` | `trigger`, `orchestration` | `while true; do ...; done` |
| End the lifecycle | `teardown` | `close`, `cleanup` | leaving the runtime alive |

## Authority Rules That Matter in Practice

### Observation is not live locator authority

Use observation to understand the page.
If the page is volatile, route-changing, or still hydrating, reacquire current authority before interacting.

Good default:

1. `open`
2. `observe`
3. `explain ...`
4. `wait ...` if needed
5. act

### When volatility is high, prefer explicit continuity

If the page is changing quickly:

- wait for URL/title/readiness first
- re-observe before acting
- use `--snapshot <id>` when strict preflight continuity matters

### Know when to pivot planes

Pivot earlier instead of forcing the wrong surface:

- interactive -> content
  when the page clearly has text/content but not stable interactive targets
- DOM -> network/storage/downloads
  when the business effect is better proven outside the page text
- automation -> human auth control
  when login, CAPTCHA, MFA, or verification is the real blocker
- top frame -> explicit frame
  when the work is actually inside an iframe

### Success is not the same as workflow completion

After a “successful” interaction, confirm one of:

- page URL/title changed as expected
- target state changed as expected
- list gained the expected item
- network request reached the expected lifecycle
- current runtime guidance says stay/branch explicitly

## High-Value Surfaces

### Observation

Use these in roughly this order:

```bash
rub observe --rub-home "$RUB_HOME"
rub state --format a11y --rub-home "$RUB_HOME"
rub state --format compact --rub-home "$RUB_HOME"
rub inspect page --rub-home "$RUB_HOME"
```

Rules:

- `observe` is the default first pass.
- `state --format a11y` is the default lower-token follow-up.
- `state --format compact` is for the cheapest structural overview.
- `inspect page` is for scoped/format-controlled inspection runtime projections
  when `observe` or `state` are not enough; it is not the default first-pass
  orientation surface.

For iframe work, establish frame authority explicitly before locator/content work:

```bash
rub frames --rub-home "$RUB_HOME"
rub frame 1 --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
```

If tab ownership is uncertain, re-establish it explicitly:

```bash
rub tabs --rub-home "$RUB_HOME"
rub switch 1 --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
```

### Explain

Use explain surfaces as pre-flight tools:

```bash
rub explain extract '<spec>'
rub explain locator --target-text "New Topic"
rub explain interactability --label "Consent"
rub explain blockers --rub-home "$RUB_HOME"
```

Use them when:

- a target is ambiguous
- a click/type fails
- the page is noisy, gated, or drifting

### Built-In Help

These are local and side-effect-free:

```bash
rub --help
rub explain --help
rub extract --examples
rub extract --schema
rub inspect list --builder-help
```

Prefer these over recalling JSON shapes from memory.

## Locator Rules

### Prefer semantic locators first

Resolution order:

1. `--selector`
2. `--target-text`
3. `--role`
4. `--label`
5. `--testid`
6. `--ref`
7. positional index as last resort

### Use ranking filters deliberately

- `--visible` narrows to non-zero authoritative snapshot boxes
- `--prefer-enabled` deprioritizes disabled or non-writable writable targets
- `--topmost` adds live hit-test cost and disables memo caching

Use `--topmost` only when occlusion is the real question.

## Default Patterns

These are reminders for the most failure-prone lanes. Use
[references/command-selection.md](references/command-selection.md) when you need
the fuller command-choice matrix.

### Reliable form fill

```bash
rub fill '<spec>' --validate --rub-home "$RUB_HOME"
rub fill '<spec>' --rub-home "$RUB_HOME"
```

Remember:

- `--snapshot <id>` is strict preflight continuity for target resolution only
- `--atomic` is a strict rollbackable subset, not a general transaction engine
- explicit `--wait-after-*` fences are often the right next step

### Rich editors / contenteditable

Do not assume a rich editor behaves like a plain input.

```bash
rub explain interactability --label "Body" --rub-home "$RUB_HOME"
rub type --text "Hello world" --label "Body" --rub-home "$RUB_HOME"
```

If safe-path classification rejects the target, stop and inspect blocker details.

### Wait for downstream effects

Prefer typed waits to shell polling:

```bash
rub wait --url-contains "/activate" --rub-home "$RUB_HOME"
rub wait --title-contains "Confirm your account" --rub-home "$RUB_HOME"
rub wait --state interactable --label "Submit" --rub-home "$RUB_HOME"
rub wait --label "Email" --description-contains "confirm" --rub-home "$RUB_HOME"
rub inspect list --wait-field subject --wait-contains "Confirm" --rub-home "$RUB_HOME"
rub inspect network --wait --match "/api/" --rub-home "$RUB_HOME"
```

Important semantics:

- `wait --state interactable` requires current-runtime visible/enabled/writable readiness plus live top-level hit-test geometry authority
- if geometry authority cannot be proven, it fails closed instead of projecting DOM-only readiness as interactable

### Authenticated workflows

Prefer current runtime/profile continuity before inventing a new login path:

- keep work inside one authenticated session when possible
- reuse `--profile` or `--user-data-dir` deliberately
- use `handoff` / `takeover` when human login or verification is required
- use secret references like `$RUB_PASSWORD` in workflow/spec surfaces instead of raw secrets when that path exists

Current rub already supports:

- `handoff *`
- `takeover *`
- browser profile reuse
- `binding capture/list/inspect/remove`
- `binding aliases/remember/resolve/rebind/forget`
- explicit command-time reuse through `--use <alias>`
- `rub secret list/set/inspect/remove`
- secret-referenced workflow/spec inputs with downstream redaction

It does **not** yet provide agent-blind secret enrollment or hidden secure
prompt semantics.

## Debug a “Successful” Command That Did Not Advance the Workflow

```bash
rub history --rub-home "$RUB_HOME"
rub doctor --rub-home "$RUB_HOME"
rub runtime summary --rub-home "$RUB_HOME"
rub runtime readiness --rub-home "$RUB_HOME"
rub runtime observatory --rub-home "$RUB_HOME"
rub inspect network --rub-home "$RUB_HOME"
```

Then rerun the failing command with:

```bash
rub <command> --verbose --rub-home "$RUB_HOME"
rub <command> --trace --rub-home "$RUB_HOME"
```

See [references/recovery-playbook.md](references/recovery-playbook.md) for the
authority-first recovery ladder.

## Workflows and Automation

Use:

- `pipe` for bounded, known-ahead-of-time multi-step flows
- `trigger` for same-session reactive automation
- `orchestration` for cross-session or multi-action reactive flows

Do not replace these with shell polling.

When these surfaces carry `fill`, `extract`, `inspect list`, or `inspect harvest`
arguments, prefer structured `spec` JSON directly inside `args`:

```bash
rub pipe '[{"command":"extract","args":{"spec":{"title":"h1","items":{"collection":"li.item","fields":{"name":".name"}}}}}]'
```

Legacy stringified JSON is still accepted for compatibility, but it is not the canonical shape.

## State Surfaces Beyond the DOM

Reach for these when page text is not enough:

- `inspect network` / `inspect curl`
- `inspect storage`, `storage *`
- `cookies *`
- `downloads`, `download wait`, `download save`
- `runtime *`
- `intercept *`
- `interference *`
- `handoff *`
- `takeover *`

These are often better than trying to infer everything from page text alone.

## Result Parsing

All default command surfaces return one JSON object.

```bash
rub <command> | jq '.success'
rub <command> | jq '.data.workflow_continuity'
```

Rules:

- check `.success` before assuming `.data` exists
- prefer `jq` for shell-side result slicing
- prefer the strongest evidence plane available, not the noisiest one
- on failure, inspect:
  - `.error.code`
  - `.error.message`
  - `.error.suggestion`
- read `workflow_continuity` when deciding whether to stay or branch
- default success output is intentionally slimmer than `--trace`

## Anti-Patterns

Avoid these:

- dropping straight to `exec` for ordinary reading or filling tasks
- using screenshots as the first-line page understanding surface
- treating observation as proof that a later live locator must resolve the same way
- skipping route/readiness checks on volatile pages
- using the same `RUB_HOME` for unrelated or parallel tasks
- leaving temporary sessions open instead of calling `teardown`
- treating `fill --atomic` as a general transaction engine

## Sharp Edges Worth Remembering

| Situation | Correct form |
|-----------|--------------|
| `find "a"` | Wrong. Use `--selector a`. |
| `inspect text h1` | Wrong. Use `--selector h1`. |
| `fill --atomic` on rich editors | Not supported by the atomic rollback subset. |
| stringified nested `args.spec` everywhere | Legacy-compatible, but not canonical. Prefer structured objects/arrays directly. |
| `observe` result access | Use `snapshot.element_map`, not `snapshot.elements`. |
| parallel commands on one `RUB_HOME` | They queue. Use separate `RUB_HOME`s for true parallelism. |
| `--topmost` everywhere | Expensive and live-only. Use only when occlusion matters. |
| “success” means the workflow is done | Not necessarily. Check waits, network, state, and `workflow_continuity`. |

When in doubt, choose the more explicit rub surface, not the lower-level one.
