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

```bash
# Build from source
cargo build --release
cp target/release/rub /usr/local/bin/

# Verify
rub --version
```

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

# Screenshot
rub screenshot --path result.png --full

# Inspect network traffic
rub inspect network --match "api/" --method GET

# Extract structured data
rub extract '{"title": "text:h1", "links": "text:a"}'

# Close when done
rub close
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
| `--json-pretty` | Pretty-print JSON output | `false` |
| `--verbose` | Include interaction trace summary | `false` |
| `--trace` | Include full interaction trace | `false` |
| `--cdp-url <url>` | Connect to external Chrome via CDP | — |
| `--connect` | Auto-discover local Chrome | `false` |
| `--profile <name>` | Connect using a Chrome profile | — |
| `--no-stealth` | Disable L1 stealth patches | `false` |
| `--humanize` | Enable L2 humanized interaction | `false` |
| `--humanize-speed <preset>` | Humanize speed: fast, normal, slow | `normal` |

**Environment variables**: `RUB_HOME`, `RUB_SESSION`, `RUB_IGNORE_CERT_ERRORS`, `RUB_USER_DATA_DIR`, `RUB_SHOW_INFOBARS`, `RUB_HUMANIZE`, `RUB_HUMANIZE_SPEED`, `RUB_STEALTH`

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

```
                                ┌──────────────────────────────┐
                                │         Chrome / CDP          │
                                └───────────────▲──────────────┘
                                                │
┌──────────┐    Unix Socket    ┌────────────────┴──────────────┐
│  CLI     │◄──── NDJSON ─────►│  Per-Session Daemon (tokio)   │
│  (clap)  │                   │                               │
└──────────┘                   │  ┌─────────┐  ┌────────────┐  │
                               │  │ Router  │  │ BrowserPort│  │
                               │  │ (FIFO)  │  │ (CDP)      │  │
                               │  └─────────┘  └────────────┘  │
                               └───────────────────────────────┘
```

```
crates/
  rub-core/           # Shared types, models, errors (zero internal deps)
  rub-cdp/            # Chrome DevTools Protocol adapter layer
  rub-ipc/            # IPC protocol and transport
  rub-daemon/         # Persistent daemon runtime, command router
  rub-cli/            # CLI entry point, command definitions, output formatting
  rub-test-harness/   # Shared test utilities
```

**Design properties**:

- **Persistent sessions** — daemon auto-starts on first command, reuses browser across commands
- **Snapshot authority** — `click` and `type` validate against a cached `snapshot_id` guarded by `dom_epoch`
- **At-most-once semantics** — `command_id` deduplication for mutating commands
- **Profile safety** — `--user-data-dir` conflicts are rejected before browser launch
- **Stealth layer** — L1 JS injection patches + L2 humanized Bézier mouse paths and timing
- **Structured logging** — `tracing`-based JSON logs to `~/.rub/daemon.log`

## Requirements

- Rust 1.94.1+
- Chrome, Chromium, or Edge (any CDP-compatible browser)
- macOS or Linux (Unix domain sockets)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

[MIT](LICENSE)
