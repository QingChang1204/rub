# Command Selection

Use this when you know the task intent but are unsure which `rub` surface is the
best first move.

The goal is not to list every command. The goal is to choose the highest stable
surface first, then escalate only when the current authority is not enough.

Use the canonical operating loop in [SKILL.md](../SKILL.md). This reference only
helps choose the right lane.

## Choose the authority plane first

| Question | Plane | Default surfaces |
|----------|-------|------------------|
| “What is on the page right now?” | observation / snapshot | `observe`, `state --format a11y`, `state --format compact` |
| “Which tab currently owns the work?” | tab / window authority | `tabs`, `switch`, `close-tab` |
| “Which interactive target should I act on?” | interactive locator | `explain locator`, `find --explain` |
| “Where is this text/content in the live page?” | content / extraction | `find --content`, `get`, `inspect text/html`, `extract`, `inspect list` |
| “Did the workflow really advance?” | evidence / aftermath | `wait`, `inspect network`, `inspect curl`, `inspect storage`, `downloads`, `workflow_continuity` |
| “Why is the runtime noisy or blocked?” | runtime / observability | `doctor`, `inspect page`, `history`, `runtime *`, `explain blockers` |
| “Which existing logged-in runtime should continue the work?” | runtime continuity | `--profile`, `--user-data-dir` |
| “Does a human need to take temporary control?” | human control | `handoff *`, `takeover *` |

If one plane already owns the answer, stay there until you have evidence to
pivot.

## Default rules

1. Start from task intent, not command names.
2. Prefer the highest stable surface that already owns the job.
3. Stay on the same authority plane until you have evidence to pivot.
4. Use typed waits before shell loops.
5. Reuse authenticated runtime/profile state before inventing a new login path.

## Fast decision table

| Intent | First choice | Escalate to | Avoid first |
|--------|--------------|-------------|-------------|
| Understand the page | `observe` | `state --format a11y`, `state --format compact`, `inspect page` | screenshots alone, `exec` |
| Establish tab/window authority | `tabs`, then `switch <index>` | `close-tab`, re-observe | guessing inside the wrong tab |
| Establish frame authority | `frames`, then `frame <index>` | `frame --name`, `frame --top`, re-observe | guessing inside the top frame |
| Find a likely interactive target | `explain locator` | `find --explain`, stronger semantic locator fields | blind indexes |
| Find content anchors in the live page | `find --content` | `inspect text`, `extract`, `inspect list` | guessed clicks |
| Read one value | `get title/html/text/value/attributes/bbox` | `inspect *`, `extract` | `exec` |
| Extract structured data | `extract` | `inspect list`, `inspect harvest` | raw DOM scripting |
| Fill a form | `fill --validate`, then `fill` | `type`, `select`, `upload` | repeated ad hoc `type` |
| Run one interaction | `click`, `type`, `select`, `hover`, `keys`, `upload` | `--wait-after-*`, `--topmost` | `exec` |
| Explain why a target is blocked | `explain interactability` | typed waits, `explain blockers` | repeated retries |
| Explain page-level blockage | `explain blockers` | `runtime summary`, `runtime readiness`, `runtime observatory` | site-specific guesses |
| Wait for a UI change | `wait` | `inspect list --wait-field`, `inspect network --wait` | shell loops |
| Confirm downstream effect | `inspect network`, `inspect curl`, `inspect storage`, `downloads` | `runtime observatory`, `runtime readiness` | UI text alone |
| Run bounded automation | `pipe` | `trigger`, `orchestration` | bash loops |
| End the lifecycle | `teardown` | `close`, `cleanup` | leaving the runtime alive |

## 1. Understand the page

Use these in this order unless you have a strong reason not to:

| Surface | What it is for | Do not use it for |
|---------|----------------|-------------------|
| `observe` | Best default first step. Screenshot + shared snapshot + a11y summary together. | Proving a later live locator still resolves the same way. |
| `state --format a11y` | Lower-token accessibility-oriented snapshot. | Rich visual debugging. |
| `state --format compact` | Cheapest structural overview. | Detailed target diagnosis. |
| `inspect page` | Scoped/format-controlled inspection runtime projection when `observe` or `state` are not enough. | Your default first-pass page understanding step. |

Good default:

```bash
rub open https://example.com --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
rub state --format a11y --rub-home "$RUB_HOME"
```

Use `--load-strategy domcontentloaded` first on heavy public sites where
`networkidle` is likely to over-wait.

If the work is inside an iframe, establish frame authority before any locator or
content operation:

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

## 2. Find or validate a target

The main mistake here is guessing too early.

### Interactive targets

Start with explain:

```bash
rub explain locator --label "Submit" --rub-home "$RUB_HOME"
rub find --explain --selector "button[type=submit]" --rub-home "$RUB_HOME"
```

Prefer these locator families in this order:

1. `--selector`
2. `--target-text`
3. `--role`
4. `--label`
5. `--testid`
6. `--ref`
7. index only as a last resort

Use:

- `--visible` when visibility is the ambiguity
- `--prefer-enabled` when disabled/readonly candidates are polluting the winner
- `--topmost` only when live occlusion is the real question

### Content anchors

If you are not targeting an interactive snapshot element, do not force the
interactive locator lane. Start with live content discovery:

```bash
rub find --content --target-text "Rust engineer" --rub-home "$RUB_HOME"
rub inspect text --selector ".job-card h3" --many --rub-home "$RUB_HOME"
```

This is often better than trying to click or explain a thing that is not yet an
interactive target.

### Frame-scoped work

If the job is inside an iframe, establish frame authority before locator or
content work:

```bash
rub frames --rub-home "$RUB_HOME"
rub frame 1 --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
```

## 3. Read data

### Read one value

Use `get` for the cheapest single-value read:

```bash
rub get title --rub-home "$RUB_HOME"
rub get html --selector ".main" --rub-home "$RUB_HOME"
rub get text --selector "h1" --rub-home "$RUB_HOME"
rub get value --label "Email" --rub-home "$RUB_HOME"
rub get attributes --selector "a" --rub-home "$RUB_HOME"
rub get bbox --selector ".modal" --rub-home "$RUB_HOME"
```

Escalate to `inspect` when you need inspection-runtime scoping, projection, or
multi-match handling:

```bash
rub inspect text       --selector "h1" --rub-home "$RUB_HOME"
rub inspect html       --selector ".main" --rub-home "$RUB_HOME"
rub inspect value      --label "Email" --rub-home "$RUB_HOME"
rub inspect attributes --selector "a" --many --rub-home "$RUB_HOME"
rub inspect bbox       --selector ".modal" --rub-home "$RUB_HOME"
```

Use `--many` only when multi-value output is intentional.

### Extract structured data

Use `extract` when you already know the structure:

```bash
rub extract '{"title":"h1","price":".price"}' --rub-home "$RUB_HOME"
rub extract --examples
rub extract --schema
```

Use `inspect list` when you want a builder-style list surface:

```bash
rub inspect list \
  --collection ".item" \
  --field "name=.name" \
  --field "price=text:.price" \
  --rub-home "$RUB_HOME"

rub inspect list --builder-help
```

Use `inspect harvest` when you already have rows/URLs and want bounded
follow-up extraction:

```bash
rub inspect harvest \
  --file links.json \
  --url-field href \
  --field "title=h1" \
  --field "body=.content" \
  --limit 5 \
  --rub-home "$RUB_HOME"
```

## 4. Fill and interact

### Forms

If there is more than one field, prefer coordinated fill:

```bash
rub fill '<spec>' --validate --rub-home "$RUB_HOME"
rub fill '<spec>' --rub-home "$RUB_HOME"
```

Important:

- `--snapshot <id>` gives strict preflight continuity for target resolution only
- `--atomic` is a rollbackable subset, not a general transaction engine
- rich editors may route to editor-safe typing, not plain input semantics

Use `type` / `select` / `upload` when you are intentionally doing one local action:

```bash
rub type "alice@example.com" --label "Email" --rub-home "$RUB_HOME"
rub select --selector "select" --value "CN" --rub-home "$RUB_HOME"
rub upload /path/file.pdf --selector "input[type=file]" --rub-home "$RUB_HOME"
```

### One interaction

```bash
rub click --target-text "Submit" --rub-home "$RUB_HOME"
rub hover --selector ".menu-item" --rub-home "$RUB_HOME"
rub keys "Escape" --rub-home "$RUB_HOME"
```

If the interaction should fence on a follow-up change, add explicit wait-after
arguments instead of guessing from the success bit alone.

## 5. Wait and verify

### Wait for UI conditions

```bash
rub wait --selector ".result" --timeout 10000 --rub-home "$RUB_HOME"
rub wait --selector "#spinner" --state hidden --timeout 8000 --rub-home "$RUB_HOME"
rub wait --state interactable --label "Submit" --timeout 8000 --rub-home "$RUB_HOME"
rub wait --url-contains "/dashboard" --timeout 8000 --rub-home "$RUB_HOME"
rub wait --title-contains "Confirm your account" --timeout 8000 --rub-home "$RUB_HOME"
rub wait --label "Email" --description-contains "confirm" --timeout 8000 --rub-home "$RUB_HOME"
```

Remember:

- `state=interactable` requires current-runtime visible/enabled/writable readiness plus live top-level hit-test geometry authority
- if geometry authority cannot be proven, it fails closed instead of projecting DOM-only readiness as interactable

### Wait for list or downstream effects

```bash
rub inspect list \
  --collection ".mail-row" \
  --field "subject=text:.subject" \
  --wait-field subject \
  --wait-contains "Confirm" \
  --rub-home "$RUB_HOME"

rub inspect network --wait --match "/api/" --rub-home "$RUB_HOME"
rub inspect curl <request_id> --rub-home "$RUB_HOME"
rub inspect storage --rub-home "$RUB_HOME"
```

When network/storage/downloads already carry the evidence you need, do not keep
forcing the DOM plane.

## 6. Automation

Use the most constrained automation surface that already matches the task.

### Pipe

Best for bounded, known-ahead-of-time flows:

```bash
rub pipe '[{"command":"open","args":{"url":"https://example.com"}},{"command":"extract","args":{"spec":{"title":"h1"}}}]' \
  --rub-home "$RUB_HOME"
```

When nested commands carry `fill`, `extract`, `inspect list`, or `inspect harvest`
specs, prefer structured JSON objects/arrays directly inside `args`.

Legacy stringified nested `args.spec` is still accepted for compatibility, but
it is not the canonical shape.

### Trigger and orchestration

Use `trigger` when the automation is same-session and reactive.
Use `orchestration` when it spans sessions or multiple actions.

Do not replace either with shell polling.

## 7. Authenticated work

The right first move is usually to reuse existing authenticated runtime state,
not to invent a new login flow.

Prefer this order:

1. stay inside the current authenticated session if possible
2. reuse `--profile` / `--user-data-dir` deliberately
3. use `handoff` when a human needs to complete authentication or verification
4. use `takeover` when a human needs direct temporary control of the session

Examples:

```bash
rub handoff start --rub-home "$RUB_HOME"
rub handoff status --rub-home "$RUB_HOME"
rub handoff complete --rub-home "$RUB_HOME"

rub takeover start --rub-home "$RUB_HOME"
rub takeover elevate --rub-home "$RUB_HOME"
rub takeover resume --rub-home "$RUB_HOME"
```

For workflow/spec-driven auth input, prefer secret references over raw secrets
when that path exists. Treat a fresh login workflow as fallback re-authentication,
not the default first move:

```bash
rub pipe --workflow login \
  --var "username=admin" \
  --var 'password=$RUB_PASSWORD' \
  --rub-home "$RUB_HOME"
```

Current rub supports secret-referenced workflow/spec inputs plus downstream
redaction, explicit `rub secret *` local surfaces, named `binding` surfaces,
and explicit command-time reuse through `--use <alias>`. It does **not** yet
provide agent-blind secret enrollment.

## 8. Runtime and recovery surfaces

When a command “worked” but the workflow did not advance, use these before
retrying blindly:

```bash
rub history --rub-home "$RUB_HOME"
rub doctor --rub-home "$RUB_HOME"
rub runtime summary --rub-home "$RUB_HOME"
rub runtime readiness --rub-home "$RUB_HOME"
rub runtime observatory --rub-home "$RUB_HOME"
rub explain blockers --rub-home "$RUB_HOME"
```

See [recovery-playbook.md](recovery-playbook.md) when you need the fuller
authority-first recovery ladder.

Remember the split:

- `rub runtime handoff` / `rub runtime takeover` are status projections
- `rub handoff ...` / `rub takeover ...` are the control surfaces
