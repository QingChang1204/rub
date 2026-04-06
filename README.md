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

**rub** is a Rust CLI + persistent daemon for headless browser automation over Chrome DevTools Protocol (CDP). It produces deterministic JSON output, maintains persistent browser sessions, and targets DOM elements through snapshot-based addressing — all designed for reliable AI agent integration without the overhead of Node.js or Python runtimes.

## Features

### 🤖 Agent-Native Interface
- **Structured JSON output** — every command returns one JSON object to stdout. No HTML to parse, no selectors to guess.
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

**Cookies**
- `cookies get` (with optional `--url` filter), `cookies set`, `cookies clear`
- `cookies export` / `cookies import` for session portability

**Web Storage**
- `storage get`, `storage set`, `storage remove`, `storage clear` (per-area: `local` / `session`)
- `storage export` / `storage import` for state snapshots

**Downloads**
- `downloads` — list download state
- `download wait` — block until download reaches target state (`completed`, `failed`, etc.)
- `download cancel` — abort in-progress downloads

**JavaScript Execution**
- `exec <code>` — evaluate JavaScript and return the result as JSON
- `exec <code> --raw` — print the raw JS result without the JSON envelope

**Session Management**
- `sessions` — list all active named sessions with PID, socket, and metadata
- `close` — close the current session's browser
- `close --all` — close every active session across all names
- `cleanup` — remove stale sockets, orphaned daemon state, and temporary browser artifacts

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

**Request Interception** (`intercept`)
- `intercept rewrite` — redirect matching requests to a different base URL
- `intercept block` — block requests by URL pattern
- `intercept allow` — explicitly allow requests
- `intercept header` — override request headers per URL pattern
- `intercept list` / `intercept remove` / `intercept clear`

**Public-Web Interference** (`interference`)
- Mode-based interference handling: `normal`, `public_web_stable`, `strict`
- Automatic classification of popups, navigation drift, consent walls
- `interference recover` for safe automated recovery

### 🤝 Human-in-the-Loop

**Handoff** — pause automation for human verification:
```bash
rub handoff start     # Pause automation
# ... human takes action ...
rub handoff complete  # Resume
```

**Takeover** — full session accessibility control:
```bash
rub takeover start    # Pause for human control
rub takeover elevate  # Relaunch headless → headed browser
rub takeover resume   # Hand back to automation
```

### 🎭 Cross-Session Orchestration

Event-driven rules that survive session restarts and span multiple sessions:
```bash
# Register an orchestration rule from a JSON spec file
rub orchestration add --file rule.json
rub orchestration add --file rule.json --paused   # Add in paused state

# Manage rules
rub orchestration list
rub orchestration execute --id 3   # Run a rule immediately by id
rub orchestration pause 3
rub orchestration resume 3
rub orchestration remove 3
rub orchestration clear

# List saved orchestration assets
rub orchestration list-assets
```

### 💬 Dialog Handling

```bash
rub dialog status                          # Check for pending JS dialogs
rub dialog accept --prompt-text "answer"   # Accept with optional prompt text
rub dialog dismiss                         # Dismiss
```

### 🔄 Cross-Tab Triggers

Event-driven automation across browser tabs:
```bash
# Register a trigger from a JSON spec file
rub trigger add --file trigger-spec.json

# Manage triggers
rub trigger list
rub trigger pause 1
rub trigger resume 1
rub trigger remove 1
rub trigger trace --last 20
```

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
brew install rub
```

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
