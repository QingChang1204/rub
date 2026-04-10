<p align="center">
  <h1 align="center">rub</h1>
  <p align="center">
    <strong>Browser automation CLI built for AI agents</strong>
  </p>
  <p align="center">
    <a href="https://github.com/QingChang1204/rub/actions"><img src="https://github.com/QingChang1204/rub/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://github.com/QingChang1204/rub/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.94.1+-orange.svg" alt="Rust"></a>
  </p>
</p>

---

**rub** is a Rust CLI + persistent daemon for headless browser automation over Chrome DevTools Protocol (CDP). It produces deterministic machine-facing output: the standard JSON envelope on default surfaces, plus an explicit raw stdout surface for `exec --raw`. It maintains persistent browser sessions and targets DOM elements through snapshot-based addressing — all designed for reliable AI agent integration without the overhead of Node.js or Python runtimes.

## Why rub?

The following observations come from running rub as an AI agent on real tasks: navigating authenticated Chinese social media sites (Weibo, Baidu, Zhihu) across 7 simultaneous sessions, querying live network traffic, extracting structured page data, and inspecting runtime state between turns. These are measurements and experiences, not claims.

### The core friction with existing tools

When a language model drives a browser through Playwright or Puppeteer, it runs into the same three problems on every task:

**Raw output requires interpretation.** These tools return DOM or screenshots. The agent has to decide what is relevant, parse it, and convert it into something it can act on. On a complex page this can cost thousands of tokens per step, and the interpretation is lossy — the model may focus on the wrong part of the page.

**Every invocation is stateless.** Each tool call typically starts a fresh browser process or reconnects. A task requiring 15 sequential steps pays the browser launch cost 15 times. More importantly, there is no persistent state: cookies, session tokens, and any prior browser context must be reconstructed from scratch.

**Retry is unsafe.** If the model retries a form submit because the response was slow, the form submits twice. There is no built-in deduplication. The agent has to solve this externally, which is complex and error-prone.

### What using rub actually looks like

**State is pre-digested, not raw.** When I call `rub state` on a live Weibo session, the response contains 83 typed elements — each with its tag, visible text, ARIA role, bounding box, a stable `element_ref` handle, and a depth in the DOM tree. The snapshot was taken in 116ms against a session that had been open and authenticated for hours. There is nothing to parse. The agent reads the JSON and acts.

```json
{
  "index": 2,
  "tag": "input",
  "attributes": {"placeholder": "搜索微博", "type": "text"},
  "bounding_box": {"x": 194.0, "y": 14.5, "width": 153.0, "height": 32.0},
  "element_ref": "6F448F458A1D33EFF8F20E8E5E500C10:25"
}
```

**`dom_epoch` tells me if the world changed.** Every snapshot carries a monotonic `dom_epoch` counter that increments on navigation. When I ran two consecutive `exec` calls on the Weibo session, I watched it go from `6` to `7` in real time — meaning a navigation event happened between the calls. If my agent holds a stale `element_ref` and the DOM has been replaced, rub rejects the reference with `StaleSnapshot`. This is enforced at the structural level; I don't implement it.

**Sessions persist across every agent turn.** I had 7 browser sessions running simultaneously: `weibo`, `baidu`, `zhihu`, `mytest`, `wiki-test`, `ssq`, `default`. They showed up in a single `rub sessions` call with their PIDs, socket paths, and protocol versions. Switching context is one environment variable: `RUB_SESSION=baidu rub state`. The sessions keep their cookies, their authenticated state, their network history — across as many agent turns as the task requires.

**The network log is always there.** Without setting up any interceptors, the Weibo session had captured **1,024 network requests** in its observatory buffer — GET calls to `rm.api.weibo.com`, XHR polling, resource loads. I can query this at any time with `rub inspect network --match "api/" --method POST`. If the agent needs to extract a bearer token from a request, or verify that a form submission fired a specific API call, the log is already there.

**Errors tell the agent exactly what to do next.** Every error response has a `code` (machine-readable), a `message` (human-readable), a `suggestion` (actionable), and a `context` object (structured data for the failure). When `inspect text` matched 51 elements instead of one, the error said: `"suggestion": "use --first, --last, or --nth to select a single match"`. When `open` timed out, it said: `"suggestion": "use --load-strategy domcontentloaded"`. This is the contract: errors never just fail — they tell the agent what to try next.

**Retry is structurally safe.** A mutating command (click, type, fill, submit) that carries a `command_id` is deduplicated inside the daemon. If the network drops between the command dispatch and the response, the agent can retry with the same `command_id`. The daemon returns the cached original response. The action never runs twice. This works without any coordination logic on the agent side.

**One call gives a health dashboard.** `rub runtime summary` returns the status of every subsystem in one round trip: dialog state (is a JS alert blocking?), frame context (am I in an iframe?), download progress, handoff state, interference mode, network observatory health, orchestration connectivity. When something is wrong and I don't know why, this is the first call I make.

### Who this is for

- **AI agents** that need structured browser output without building a parser around raw DOM or screenshots
- **LLM pipelines** where authentication state and browser context must survive across multiple tool calls within a conversation turn
- **Multi-session automation** where separate browser workers need to coordinate — one session monitors, a trigger bridges the condition to an action on a different browser
- **Production agent infrastructure** that needs at-most-once execution, safe retry, structured error codes, and passive network observability without configuration overhead

## Features

### 🤖 Agent-Native Interface
- **Structured stdout contract** — default command surfaces return one JSON object to stdout; `exec --raw` is an explicit raw-value surface. No HTML to parse, no selectors to guess.
- **Snapshot-based element targeting** — elements are addressed by snapshot ID + index, eliminating race conditions from DOM mutations.
- **At-most-once execution** — mutating commands carry a `command_id` to prevent duplicate side effects during retries.
- **Interaction trace** — `--verbose` / `--trace` flags expose what the runtime observed before and after each action.

### 🔗 Persistent Daemon Architecture
- **Auto-start sessions** — first command spawns a per-session daemon; subsequent commands reuse the same browser.
- **Named sessions** — `--session work` / `--session test` for parallel isolated browser instances.
- **Session health** — `rub doctor` reports browser, daemon, socket, and disk health in structured JSON.
- **Session takeover** — pause automation and hand control to a human, with bounded headless-to-headed elevation.

### 🌐 Complete Browser Control

**Navigation & Pages**
- `open`, `back`, `forward`, `reload` with configurable load strategies (`load`, `domcontentloaded`, `networkidle`)
- Multi-tab management: `tabs`, `switch`, `close-tab`
- Frame targeting: `frames`, `frame` (select by index, name, or reset to top)

**DOM Observation**
- `state` — full DOM snapshot with optional a11y info, viewport filtering, listener detection, and diff mode
- `observe` — atomic page summary + screenshot in one call
- `find` — locate elements through multiple locator strategies
- Three output formats: `snapshot`, `a11y`, `compact` (token-optimized `[index@depth]` notation)
- Observation scoping: `--scope-selector`, `--scope-role`, `--scope-label`, `--scope-testid`
- Depth filtering with `--depth N` for focused views

**Element Interaction**
- `click` with `--double` / `--right` / `--xy` (coordinate click)
- `type` for text input with `--clear` to reset fields first
- `keys` for keyboard shortcuts (`Enter`, `Control+a`, `Shift+Tab`)
- `hover` for mouse-over effects
- `select` for dropdown options
- `upload` for file inputs
- `fill` for batch form filling with JSON specs and optional submit targeting
- `scroll` with directional control and pixel amounts
- `wait` for conditions: CSS selector, text, role, label, testid — with state control (`visible`, `hidden`, `attached`, `detached`)

**Element Targeting** (works across `click`, `type`, `inspect`, etc.)
- Snapshot index: `rub click 3 --snapshot <id>`
- CSS selector: `--selector ".btn-primary"`
- Visible/accessible text: `--target-text "Submit"`
- Semantic role: `--role button`
- Accessible label: `--label "Search"`
- Testing ID: `--testid "login-form"`
- Stable ref: `--ref <frame-bound-ref>` from `state`/`observe`
- Disambiguation: `--first`, `--last`, `--nth N`
- Implicit snapshots: omit `--snapshot` for automatic live snapshot

**Post-Action Waits** (available on most interaction commands)
- `--wait-after-selector`, `--wait-after-text`, `--wait-after-role`, `--wait-after-label`, `--wait-after-testid`
- `--wait-after-state` (`visible`, `hidden`, `attached`, `detached`)
- `--wait-after-timeout` for custom timeout

### 🔍 Unified Inspection Surface

The `inspect` command family provides read-only queries:

| Subcommand | Description |
|---|---|
| `inspect page` | DOM snapshot with scoping, formatting, and projection options |
| `inspect text` | Element text content (supports `--many` for multi-match) |
| `inspect html` | Element or page HTML |
| `inspect value` | Input/textarea values |
| `inspect attributes` | Element attributes |
| `inspect bbox` | Element bounding box (x, y, width, height) |
| `inspect list` | Structured list extraction with JSON spec |
| `inspect storage` | Web storage snapshot (localStorage/sessionStorage) |
| `inspect network` | Network request log with filtering (`--match`, `--method`, `--status`, `--lifecycle`) and `--wait` |
| `inspect curl` | Export a recorded network request as a reproducible `curl` command |

### 📊 Structured Data Extraction

```bash
# Extract a typed data structure from the page
rub extract '{"title": "text:.product-name", "price": "text:.price", "in_stock": "exists:.in-stock"}'

# Extract collections with nested fields
rub inspect list '{"container": ".product-list", "item": ".product-card", "fields": {"name": "text:h3", "price": "text:.price"}}'
```

### 🍪 State Management

**Cookies** — full browser cookie jar access:
```bash
rub cookies get                       # All cookies for the session
rub cookies get --url https://api.example.com   # Filter to cookies sent to a URL
rub cookies set SESSION_ID abc123 --domain example.com --secure --http-only --same-site Strict --expires 1735689600
rub cookies clear                     # Clear all cookies
rub cookies clear --url https://example.com     # Clear cookies scoped to a URL
rub cookies export cookies.json       # Dump all cookies to JSON
rub cookies import cookies.json       # Restore cookies (session portability)
```

**Web Storage** — localStorage and sessionStorage, scoped to the current origin:
```bash
rub storage get my_key                # Search both areas
rub storage get my_key --area local   # Explicit localStorage
rub storage get my_key --area session # Explicit sessionStorage
rub storage set token abc123          # Set in both areas (defaults to local)
rub storage remove token              # Remove from both areas
rub storage clear --area local        # Clear only localStorage
rub storage clear                     # Clear both areas
rub storage export                    # Print snapshot JSON to stdout
rub storage export --path snap.json   # Write snapshot to a file
rub storage import snap.json          # Restore all storage keys into the current origin
```

**Downloads** — browser download management:
```bash
rub downloads                                 # List all in-flight and completed downloads
rub download wait                             # Block until any download reaches `completed`
rub download wait --id <GUID> --state in_progress   # Wait for specific download state
rub download cancel <GUID>                    # Abort an in-progress download
```

**Batch Asset Download** (`download save`) — authenticated bulk file fetcher using session cookies:
```bash
rub download save \
  --file urls.txt \
  --output-dir ./assets

# JSON source with nested path and URL/name fields
rub download save \
  --file api_response.json \
  --input-field data.items \
  --url-field download_url \
  --name-field filename \
  --output-dir ./media \
  --concurrency 12 \
  --base-url https://cdn.example.com \
  --cookie-url https://example.com \
  --limit 100 \
  --overwrite
```
Reads session cookies from the live browser, sends them as `Cookie` headers during fetch.

**JavaScript Execution**
```bash
rub exec "document.title"                   # Returns JSON-encoded result
rub exec "window.scrollTo(0, 500)" --raw    # Raw JS return value (no JSON wrapper)
rub exec "JSON.stringify(window.__APP_STATE__)"  # Extract embedded data
```

**Session Management**
```bash
rub sessions            # List all active sessions (name, PID, socket, protocol version)
rub close               # Close current session's browser
rub close --all         # Terminate every active session
rub cleanup             # Remove stale sockets, orphaned daemon state, temp artifacts
```

### 🛡️ Anti-Detection & Stealth

**L1: Stealth Baseline** (enabled by default)
- 9 JS injection patches applied via `evaluateOnNewDocument`:
  - `navigator.webdriver` removal, `chrome.runtime` mock, realistic `navigator.plugins`
  - `navigator.languages`, `Permissions.prototype.query` normalization
  - `window.chrome` object scaffold, `navigator.connection` simulation
  - WebGL debug renderer info spoofing
  - Worker context bridge (dedicated + shared workers inherit stealth patches)
- Launch-arg minimization to reduce automation fingerprint

**L2: Humanized Interaction** (`--humanize`)
- Bézier curve mouse paths with natural control points
- Randomized typing delays with speed presets: `--humanize-speed fast|normal|slow`
- Humanized scroll patterns

### 🔀 Workflow Engine

```bash
# Execute a pipeline of commands
rub pipe '[{"command": "open", "args": {"url": "https://example.com"}}, {"command": "screenshot", "args": {"path": "shot.png"}}]'

# Load from file
rub pipe --file workflow.json

# Parameterized workflows with secret resolution
rub pipe --workflow login --var "username=admin" --var "password=secret"

# Save/load named workflows
rub history --export-pipe --save-as "my-flow"
rub pipe --workflow my-flow
rub pipe --list-workflows

# Export command history as replayable script
rub history --export-script --output replay.sh
```

### 🌍 Network Controls

**Request Interception** (`intercept`) — session-scoped CDP `Fetch.enable` rules:
```bash
# Rewrite: redirect matching requests to a different base URL
# Supports exact match or trailing-* prefix patterns
rub intercept rewrite "https://api.prod.example.com/*" "https://api.staging.example.com"

# Block: drop matching requests before they reach the network
rub intercept block "https://analytics.example.com/*"

# Allow: explicitly pass through requests (takes precedence over block rules)
rub intercept allow "https://api.example.com/health"

# Header override: inject or replace request headers
rub intercept header "https://api.example.com/*" Authorization "Bearer my-token"
rub intercept header "https://api.example.com/*" --header "X-Debug=1" --header "X-Env=staging"

# Manage rules
rub intercept list              # List all active rules with stable IDs
rub intercept remove 2          # Remove rule by ID from `list`
rub intercept clear             # Remove all interception rules
```

**Public-Web Interference** (`interference`) — automatic popup/overlay detection and recovery:
```bash
# Set session interference tolerance mode
rub interference mode normal          # Default: fail on unexpected navigation
rub interference mode public_web_stable  # Tolerate popups and consent walls
rub interference mode strict          # Reject any deviation from expected flow

# Trigger safe automated recovery for the current classified interference
rub interference recover
```
The interference classifier runs on every command cycle and categorizes: cookie consent dialogs, email capture modals, geo-redirect drift, CAPTCHA presence, and login-wall detection.

### 🤝 Human-in-the-Loop

**Handoff** — pause automation and wait for a human to complete a verification step:
```bash
rub handoff start     # Pause all automation commands (returns AutomationPaused to callers)
rub handoff status    # Check current handoff state and who initiated it
rub handoff complete  # Mark verification done and unblock automation
```
While handoff is active, all automation commands are rejected with `AutomationPaused`. Only `handoff status` and `handoff complete` are allowed through.

**Takeover** — full human control of the browser session, designed for supervised automation:
```bash
rub takeover start    # Pause automation; marks session as human-controlled
rub takeover status   # Show current takeover state and accessibility binding
rub takeover elevate  # Re-launch a headless browser into a visible headed window (when supported)
rub takeover resume   # Hand browser back to automation and unblock commands
```
`elevate` is distinct from `start` — it physically makes the browser window visible for direct interaction, useful when the session was originally launched headless.

### 🎭 Cross-Session Orchestration

Event-driven multi-step automation rules that span **multiple named sessions**. A rule has a `source` session (condition evaluation) and a `target` session (action execution). Each rule is a sequenced list of actions executed in order — partial failures report exactly which step failed.

```bash
# Register from a JSON spec file or a named asset under RUB_HOME/orchestrations/
rub orchestration add --file rule.json
rub orchestration add --asset my-rule          # Load from ~/.rub/orchestrations/my-rule.json
rub orchestration add --file rule.json --paused  # Register but keep paused

# Manage active rules
rub orchestration list                          # Show all rules, status, and last result
rub orchestration execute --id 3               # Execute rule 3 immediately (bypass condition)
rub orchestration pause 3
rub orchestration resume 3
rub orchestration remove 3
rub orchestration trace --last 20              # Recent lifecycle/outcome events

# Persist and share rules as named assets
rub orchestration export 3 --save-as my-rule   # Save to ~/.rub/orchestrations/my-rule.json
rub orchestration export 3 --output ./rule.json # Export to explicit path
rub orchestration list-assets                  # List all saved orchestration assets
```

**Rule structure** (JSON spec):
- `mode`: `once` (fires then becomes `fired`) or `repeat` (re-arms after cooldown)
- `source`: `{session_name, tab_target_id, frame_id}` — where conditions are evaluated
- `target`: `{session_name}` — where actions are dispatched
- `actions[]`: ordered list of `{kind: "browser_command"|"workflow", command, payload}`
- `execution_policy`: `{cooldown_ms, retry_limit}` — up to 3 transient retries with 100ms delay
- Workflow actions support `vars` (static bindings) and `source_vars` (live bindings read from source tab at dispatch time)

### 💬 Dialog Handling

```bash
rub dialog status                          # Check for pending JS dialogs
rub dialog accept --prompt-text "answer"   # Accept with optional prompt text
rub dialog dismiss                         # Dismiss
```

### 🔄 Cross-Tab Triggers

Event-driven automation that monitors a **source tab** for a condition and executes an action on a **target tab** when the condition fires. Evaluated every 500ms in a background worker independent of CLI activity.

```bash
rub trigger add --file trigger-spec.json        # Register and arm immediately
rub trigger add --file trigger-spec.json --paused  # Register but keep paused
rub trigger list                                # Show all triggers, status, last evidence
rub trigger pause 1
rub trigger resume 1
rub trigger remove 1
rub trigger trace --last 20                     # Recent fire/block/degraded events with evidence
```

**Condition types** (field `condition.kind` in spec):

| Kind | What is evaluated on the source tab |
|------|-------------------------------------|
| `url_match` | Current tab URL contains `condition.url_pattern` |
| `text_present` | Page contains `condition.text` |
| `locator_present` | A CSS / role / label / testid locator resolves to ≥1 element |
| `readiness` | Page readiness matches `condition.readiness_state` (`domcontentloaded`, `networkidle`, etc.) |
| `network_request` | A recorded network request matches `condition.method` + `condition.status_code` + URL pattern |
| `storage_value` | `condition.area` storage at `condition.key` equals `condition.value` |

**Action types** (field `action.kind` in spec):

| Kind | What executes on the target tab |
|------|--------------------------------|
| `browser_command` | Any rub command (`click`, `type`, `fill`, `navigate`, …) with a JSON payload |
| `workflow` | A named workflow or inline `steps` array, with optional `source_vars` bindings from the source tab |

**Reliability guarantees**:
- **Double-fire prevention**: evidence fingerprint (`consumed_evidence_fingerprint`) gates each fire cycle — same network request or storage value cannot trigger twice
- **Post-queue re-validation**: condition is re-checked after acquiring the FIFO lock, preventing stale fires if the condition cleared while queued
- **Tab reconciliation**: if source or target tab closes/reopens, the trigger degrades to `unavailable` and auto-recovers when tabs reappear
- **Mode**: `once` (trigger fires once then stops) or `repeat` (re-arms after each fire)

### 📡 Runtime Observability

```bash
rub runtime summary          # All runtime surfaces in one call
rub runtime dialog           # JS dialog state
rub runtime frame            # Current frame context
rub runtime integration      # Request interception state
rub runtime interference     # Public-web interference state
rub runtime observatory      # Recent runtime events
rub runtime state-inspector  # Auth/session storage visibility
rub runtime readiness        # Page stabilization heuristics
rub runtime handoff          # Human handoff state
rub runtime downloads        # Download runtime state
rub runtime storage          # Storage runtime state
rub runtime takeover         # Takeover/accessibility state
rub runtime trigger          # Trigger registry state
```

### 🔌 External Browser Connection

```bash
# Connect to an already-running Chrome via CDP
rub --cdp-url ws://localhost:9222 open https://example.com

# Auto-discover locally-running Chrome (ports 9222-9229)
rub --connect open https://example.com

# Connect using a named Chrome profile
rub --profile "Work" open https://example.com
```

## Installation

### Homebrew (macOS / Linux)

```bash
brew tap QingChang1204/tap
brew install rub-cli
```

The Homebrew formula is published as `rub-cli`, and it installs the `rub` binary.

### Build from Source

```bash
cargo build --release
cp target/release/rub /usr/local/bin/

# Verify
rub --version
```

> **Requirements**: Chrome, Chromium, or Edge must be installed separately. rub connects to the browser via CDP and does not bundle a browser.

## Quick Start

```bash
# Open a page and capture state
rub open https://example.com
rub state

# Interact with elements (implicit snapshot)
rub click --role link --first
rub type --selector "#search" "rust browser automation" --clear
rub keys Enter

# Wait for results
rub wait --text "Results" --timeout 5000

# Screenshot (plain or with element index overlays)
rub screenshot --path result.png --full
rub screenshot --path result.png --highlight

# Execute JavaScript
rub exec "document.title"
rub exec "window.scrollTo(0, 500)" --raw

# Inspect network traffic
rub inspect network --match "api/" --method GET

# Extract structured data
rub extract '{"title": "text:h1", "links": "text:a"}'

# Session management
rub sessions            # list all active sessions
rub close               # close current session browser
rub close --all         # close every active session
rub cleanup             # purge stale sockets and orphaned state

# Pretty-print output (--json is an alias for --json-pretty)
rub --json state
```

## Global Options

| Option | Description | Default |
|---|---|---|
| `--session <name>` | Named session for isolation | `default` |
| `--rub-home <path>` | Data directory | `~/.rub` |
| `--timeout <ms>` | Per-command timeout | `30000` |
| `--headed` | Launch browser with visible window | `false` |
| `--ignore-cert-errors` | Ignore TLS certificate errors | `false` |
| `--user-data-dir <path>` | Reuse a browser profile | — |
| `--json-pretty` / `--json` | Pretty-print JSON output | `false` |
| `--verbose` | Include interaction trace summary | `false` |
| `--trace` | Include full interaction trace | `false` |
| `--cdp-url <url>` | Connect to external Chrome via CDP | — |
| `--connect` | Auto-discover local Chrome (ports 9222–9229) | `false` |
| `--profile <name>` | Connect using a Chrome profile | — |
| `--no-stealth` | Disable L1 stealth patches | `false` |
| `--humanize` | Enable L2 humanized interaction | `false` |
| `--humanize-speed <preset>` | Humanize speed: `fast`, `normal`, `slow` | `normal` |

**Environment variables**: `RUB_HOME`, `RUB_SESSION`, `RUB_IGNORE_CERT_ERRORS`, `RUB_USER_DATA_DIR`, `RUB_SHOW_INFOBARS`, `RUB_HUMANIZE`, `RUB_HUMANIZE_SPEED`, `RUB_STEALTH` (`0` to disable stealth)

## Configuration

Optional `$RUB_HOME/config.toml`:

```toml
default_timeout_ms = 45000
headed = false
ignore_cert_errors = false
user_data_dir = "/tmp/rub-profile"
hide_infobars = true
```

CLI flags override file configuration.

## Architecture

### Overview

```
┌─────────────────────────────────────────────────────────────────┐
│  rub CLI (clap)                                                  │
│  Parses args → builds IpcRequest → sends over Unix socket       │
└─────────────────────────────┬───────────────────────────────────┘
                              │ NDJSON over Unix domain socket
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  Per-Session Daemon  (tokio async runtime)                       │
│                                                                  │
│  ┌──────────────┐    ┌───────────────────────────────────────┐  │
│  │ IPC Server   │    │  DaemonRouter  (FIFO Semaphore = 1)   │  │
│  │ Unix Socket  ├───►│  · Replay fence (at-most-once)        │  │
│  │ NDJSON codec │    │  · Handoff gate                       │  │
│  └──────────────┘    │  · Timeout budget tracking            │  │
│                      │  · Command dispatch (40+ handlers)    │  │
│                      └──────────────┬────────────────────────┘  │
│                                     │                            │
│  ┌──────────────────────────────────▼────────────────────────┐  │
│  │  BrowserPort (CDP / chromiumoxide)                        │  │
│  │  · Snapshot engine + dom_epoch guard                      │  │
│  │  · Stealth injection (9 evaluateOnNewDocument patches)    │  │
│  │  · Humanized Bézier mouse + typing                        │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                  │
│  Background Workers                                              │
│  · trigger_worker   — cross-tab event rules                     │
│  · orchestration_worker — cross-session automation rules        │
└─────────────────────────────────────────────────────────────────┘
```

### IPC Transport

The CLI and daemon communicate over **Unix domain sockets** using an **NDJSON (newline-delimited JSON) codec**. Each CLI invocation opens one connection, writes exactly one `IpcRequest`, reads one `IpcResponse`, and exits. The protocol is framed by newline as the commit fence — a partial write without a trailing newline is classified as `partial_ndjson_frame` and rejected cleanly.

The socket bind path is guarded by a `.bind.lock` file (acquired via `flock(LOCK_EX)`) to serialize concurrent startup races. Before binding, the server probes the existing socket file: if it accepts connections it belongs to a live daemon and the new process backs off; if it refuses (`ConnectionRefused`) the stale file is unlinked — but only after verifying the socket's `(dev, inode)` identity matches, preventing a TOCTOU race where a new daemon creates a fresh socket between the probe and the unlink.

### FIFO Command Queue

`DaemonRouter` owns a `tokio::sync::Semaphore` initialized to **1 permit**. Every incoming request must acquire this permit before executing, which means commands are serialized FIFO — there is no concurrent execution within a session. This is the foundational invariant that makes snapshot-based element addressing correct: no DOM mutation can race with a read.

The queue enforces a **timeout budget** split into two phases:
1. **Queue wait** — time spent waiting for the Semaphore permit
2. **Execution** — time spent in the actual CDP command

Both are measured independently and returned in every response as `timing.queue_ms`, `timing.exec_ms`, `timing.total_ms`.

### At-Most-Once Semantics (Replay Fence)

Mutating commands that carry a `command_id` field participate in the **replay fence**:

1. First arrival: daemon claims `command_id` ownership via a `watch` channel, executes the command, caches the exact response.
2. Concurrent duplicate: waits on the same `watch` channel until the first execution completes, then returns the cached response directly — no double execution.
3. Conflicting fingerprint: same `command_id` but different command/args — rejected with `IpcVersionMismatch`.

The cached response stored is the **post-commit** shaped response (after frame-limit enforcement and timing injection), ensuring any replay observes exactly the same wire format as the original caller.

### Snapshot Authority & DOM Epoch

The `state` command captures a DOM snapshot and assigns it a `snapshot_id`. Interaction commands (`click`, `type`, etc.) default to an **implicit live snapshot** (taken atomically before dispatch) or can reference an explicit `--snapshot <id>`, validated by a `dom_epoch` counter that increments on every navigation event. If the epoch has advanced since the snapshot was taken, the snapshot is rejected as stale — preventing automation from targeting elements that no longer exist post-navigation.

### Daemon Startup Commit Protocol

Startup uses a **two-phase commit** to prevent split-brain discovery:

1. Daemon binds the Unix socket and writes a PID file.
2. Daemon publishes a **pending** registry entry (not yet authoritative).
3. Daemon writes canonical socket and PID **projections** (symlinks + plain files) using `atomic_write` + `fdatasync` for durability.
4. Daemon writes a **startup committed marker** — only after this file exists can discovery treat the daemon as canonical.
5. `promote_session_authority` atomically makes the entry the authoritative record for the session name.

If any step fails, `StartupCommitGuard` (a drop-guard) rolls back the registry entry, removes all projections, and restores the previous authority — so a crash during startup never leaves a permanently broken session.

### Daemon Idle Shutdown

The daemon evaluates idle shutdown each 60-second tick. It exits only when **all** of the following hold:
- No connected CLI clients (`connected_client_count == 0`)
- No in-flight command transactions (`in_flight_count == 0`)
- No active cross-tab triggers
- No active cross-session orchestration rules
- No active human-control handoff/takeover
- Last activity was > 30 minutes ago

Shutdown first drains pending background projections (history, workflow assets), then waits for all workers to exit, then closes the browser — in strict order to avoid cutting an in-flight transaction.

### Crate Dependency Graph

```
rub-cli
  └─ rub-daemon   ← orchestration, triggers, session, router
  └─ rub-cdp      ← CDP adapter, stealth, snapshot engine
  └─ rub-ipc      ← Unix socket server/client, NDJSON codec
  └─ rub-core     ← shared models, errors, port traits (no internal deps)
```

`rub-core` has **zero internal dependencies** and defines the `BrowserPort` trait that allows `rub-cdp` to be swapped or mocked in tests.

### Key Design Invariants

| Invariant | Enforcement Point |
|-----------|------------------|
| Commands execute FIFO, one at a time | `DaemonRouter::exec_semaphore` (capacity = 1) |
| Mutating commands are at-most-once | Replay fence in `prepare_command_dispatch` |
| Stale element addresses are rejected | `dom_epoch` guard in snapshot validation |
| Daemon startup is atomic or rolled back | `StartupCommitGuard` drop impl |
| Socket bind excludes concurrent starters | `flock(LOCK_EX)` on `.bind.lock` |
| IPC frames are bounded | `MAX_FRAME_BYTES` enforced in `finalize_response` |

## Requirements

- Chrome, Chromium, or Edge installed separately (any version supporting CDP)
- macOS or Linux (Unix domain sockets required for daemon IPC)
- **Build from source only**: Rust 1.94.1+

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

[MIT](LICENSE)
