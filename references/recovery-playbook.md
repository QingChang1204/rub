# Recovery Playbook

Use this when `rub` commands succeed, fail noisily, or return evidence that does
not match the workflow you expected.

The main rule is simple:

**Do not retry first. Reconstruct current authority first.**

Assume the default loop from [SKILL.md](../SKILL.md) already applies. This
playbook starts when that loop stalled or produced confusing evidence.

## 0. Classify the failed plane first

Before you touch the page again, ask which plane actually failed:

- observation / snapshot
  - the page you are looking at may no longer be the page you think it is
- interactive locator
  - the target may be ambiguous, stale, blocked, or now in a different frame
- content / extraction
  - the page may have the content, but not as stable interactive targets
- evidence / aftermath
  - the action may have succeeded locally, but the business effect is only visible in network/storage/downloads
- runtime continuity
  - the work may still belong to an existing authenticated browser/profile/session
- human control
  - login, MFA, CAPTCHA, or human verification may now require a human to step in

Then recover in that plane first.

If the failure is clearly auth-related, do this before the broader runtime ladder:

1. check whether the current authenticated runtime/profile should continue the work
2. only if that is not enough, inspect handoff/takeover state
3. only then transfer control to a human

## 1. Reconstruct current authority

Start by rebuilding recent context from the current page and runtime, not from
memory:

```bash
rub tabs --rub-home "$RUB_HOME"
rub inspect page --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
rub state --format a11y --rub-home "$RUB_HOME"
rub history --rub-home "$RUB_HOME"
rub doctor --rub-home "$RUB_HOME"
rub runtime summary --rub-home "$RUB_HOME"
rub runtime readiness --rub-home "$RUB_HOME"
rub runtime observatory --rub-home "$RUB_HOME"
rub explain blockers --rub-home "$RUB_HOME"
```

This answers the questions that matter first:

- are we still in the expected runtime and tab?
- did the URL/title change?
- is the page still hydrating or blocked?
- did the command produce follow-up network/runtime evidence?

## 2. Check for drift before you retry

Many “rub is flaky” moments are really context drift:

- route changed
- active tab changed
- frame changed
- page is still hydrating
- a guide/login wall appeared

Before you retry an interaction:

1. confirm the current tab with `tabs`
2. confirm current page metadata with `inspect page`
3. if the work is frame-scoped, confirm or re-enter the right frame
4. if route/readiness changed, wait explicitly
5. re-observe or reacquire the snapshot before acting again

Example:

```bash
rub wait --url-contains "/dashboard" --timeout 8000 --rub-home "$RUB_HOME"
rub wait --title-contains "Dashboard" --timeout 8000 --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
```

Do not keep using an old observation as proof that a later live locator must
still resolve the same way.

## 3. Re-check target authority

When a target is ambiguous, blocked, or missing, explain first:

```bash
rub explain locator --label "Submit" --rub-home "$RUB_HOME"
rub explain interactability --label "Submit" --rub-home "$RUB_HOME"
rub find --explain --selector "button[type=submit]" --rub-home "$RUB_HOME"
rub find --content --target-text "Submit" --rub-home "$RUB_HOME"
```

Use the content lane when the thing you need is a live content anchor, not an
interactive snapshot target.

If the page is volatile and you need strict continuity, reacquire a fresh
snapshot and use explicit snapshot-scoped targeting.

If the work is frame-scoped, recover frame authority before retrying:

```bash
rub frames --rub-home "$RUB_HOME"
rub frame 1 --rub-home "$RUB_HOME"
rub observe --rub-home "$RUB_HOME"
```

## 4. Failure taxonomy

### `ELEMENT_NOT_FOUND`

Do this in order:

1. re-check page state with `observe` / `state`
2. switch from index guessing to semantic locators
3. run `explain locator` or `find --explain`
4. use `find --content` when the page has text/content but not stable interactive targets
5. if the element is in an iframe, switch frame context first with `frames` / `frame`

### `ELEMENT_NOT_INTERACTABLE`

Do not force retries first:

```bash
rub explain interactability --selector ".button" --rub-home "$RUB_HOME"
rub wait --selector ".button" --state visible --timeout 8000 --rub-home "$RUB_HOME"
rub wait --selector ".button" --state interactable --timeout 8000 --rub-home "$RUB_HOME"
rub wait --selector "#spinner" --state hidden --timeout 8000 --rub-home "$RUB_HOME"
```

Remember:

- `state=interactable` requires current-runtime visible/enabled/writable readiness plus live top-level hit-test geometry authority
- if geometry authority cannot be proven, it fails closed instead of projecting DOM-only readiness as interactable

### `WAIT_TIMEOUT`

Treat timeout as missing evidence, not a mandate to click harder:

- verify the condition is the right one
- confirm selector/locator authority
- check `runtime observatory` for JS/network failures
- move to `inspect list --wait-field` or `inspect network --wait` if the business effect is not best represented in the DOM

### `NAVIGATION_FAILED`

Check:

- requested URL
- actual page URL/title via `inspect page`
- tabs for popup/new-tab behavior
- blockers/readiness for guide wall or security gate behavior

### `IPC_TIMEOUT`

The daemon may be degraded or blocked:

```bash
rub cleanup --rub-home "$RUB_HOME"
rub doctor --rub-home "$RUB_HOME"
```

### `SESSION_BUSY`

A previous command is still running. Wait for it or clean up stale work. Use a
separate `RUB_HOME` for true parallelism.

## 5. If the DOM plane and data plane disagree

This is common on modern sites.

If the UI still looks wrong but network/storage evidence is strong, pivot to the
data plane instead of forcing more UI guesses:

```bash
rub inspect network --rub-home "$RUB_HOME"
rub inspect curl <request_id> --rub-home "$RUB_HOME"
rub inspect storage --rub-home "$RUB_HOME"
rub runtime observatory --rub-home "$RUB_HOME"
```

Typical cases:

- network says the list/detail request succeeded, but the page is still sparse
- same-origin read-like follow-up suggests you should stay on the current runtime and re-check page state
- storage or cookies changed even though the DOM has not yet caught up

## 6. If the site is noisy

Treat noise as explicit interference, not randomness:

### Observe state first

```bash
rub tabs --rub-home "$RUB_HOME"
rub runtime interference --rub-home "$RUB_HOME"
rub explain blockers --rub-home "$RUB_HOME"
rub runtime handoff --rub-home "$RUB_HOME"   # status only
rub runtime takeover --rub-home "$RUB_HOME"  # status only
rub intercept list --rub-home "$RUB_HOME"
```

Classify what happened:

- cookie banner → dismiss/accept explicitly
- login guide or soft wall → confirm whether public browsing is still possible before retrying
- CAPTCHA / bot detection → inspect handoff/takeover status, then use `rub handoff ...` or `rub takeover ...` to transfer control
- unexpected new tab → switch back to the authoritative tab
- unknown drift → re-check page metadata, tabs, readiness, then reacquire authority

Remember:

- `rub runtime handoff` / `rub runtime takeover` are visibility/status surfaces
- `rub handoff ...` / `rub takeover ...` are the action surfaces

### Take action only after state is clear

If state inspection shows a human must step in:

```bash
rub handoff start --rub-home "$RUB_HOME"
rub takeover start --rub-home "$RUB_HOME"
rub takeover elevate --rub-home "$RUB_HOME"
```

## 7. If authentication or verification is the real blocker

Do not invent a second login path mid-workflow unless you mean to.

Prefer:

1. stay in the current authenticated runtime if it still owns the work
2. reuse `--profile` / `--user-data-dir` deliberately
3. use `handoff` when a human needs to complete login or verification
4. use `takeover` when a human needs direct temporary control

Examples:

```bash
rub handoff start --rub-home "$RUB_HOME"
rub handoff status --rub-home "$RUB_HOME"
rub handoff complete --rub-home "$RUB_HOME"

rub takeover start --rub-home "$RUB_HOME"
rub takeover elevate --rub-home "$RUB_HOME"
rub takeover resume --rub-home "$RUB_HOME"
```

If a workflow/spec path accepts secret references, prefer those over raw secret
values.

If auth input behavior looks wrong, inspect the effective local-vs-environment
secret provenance before changing the workflow:

```bash
rub secret inspect RUB_PASSWORD --rub-home "$RUB_HOME"
```

## 8. Use trace only after reconstructing the basics

Once page/runtime authority is understood, then rerun the failing command with:

```bash
rub <command> --verbose --rub-home "$RUB_HOME"
rub <command> --trace --rub-home "$RUB_HOME"
```

`--trace` is for richer diagnostics, not for replacing the basic authority
reconstruction steps above.

## 9. Before falling back to `exec`

Pause and re-check [command-selection.md](command-selection.md) first.

Use `exec` only when no stable rub surface owns the task after you have already:

1. rebuilt current authority
2. checked drift
3. re-checked target authority
4. considered the data plane when the DOM plane is weak

## 10. Close cleanly

If the workflow is done:

```bash
rub teardown --rub-home "$RUB_HOME"
```

Prefer `teardown` over manually remembering `close` plus `cleanup`.
