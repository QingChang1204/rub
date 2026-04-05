# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-05

### Added

#### Navigation
- `open <url>` — navigate to a URL with configurable load strategy (`load` / `domcontentloaded` / `networkidle`)
- `back` — navigate back in browser history
- `forward` — navigate forward in browser history
- `reload` — reload the current page with configurable load strategy

#### Observation
- `state` — DOM snapshot with format projection (`snapshot` / `a11y` / `compact`), viewport filtering, depth limiting, a11y annotation, JS event listener detection, snapshot diff, and semantic scope filtering (`--scope-role` / `--scope-label` / `--scope-testid`)
- `observe` — atomic multimodal frame: a11y summary + highlighted screenshot + element map in a single command
- `find` — locate elements by canonical locator (selector, text, role, label, testid, ref) against snapshot or live DOM
- `screenshot` — capture viewport or full-page screenshot with optional element highlight overlays
- `frames` — list the live iframe inventory for the current page context
- `frame` — select the active frame context by index, name, or reset to top

#### Interaction
- `click` — click an element by index, selector, or text; supports `--double`, `--right`, and raw `--xy` coordinates
- `type` — type text into an element with optional `--clear` to replace existing content
- `keys` — send a key press or combination (e.g., `Enter`, `Control+a`, `Shift+Tab`)
- `hover` — move the cursor over an element
- `scroll` — scroll the viewport up or down by pixel or direction
- `select` — select an option from a dropdown by value, text, or index
- `upload` — upload a file to a file input element
- `wait` — wait for a CSS selector, text, role, label, testid, or element state (`visible` / `hidden` / `attached` / `detached`)

#### Form & Workflow
- `fill` — batch fill multiple form fields in a single command with optional submit target (index / selector / text / role / label / testid / ref)
- `extract` — extract typed structured data from the page using a JSON spec with field types, transforms, `many`, `first`, `last`, `nth`, and `default`
- `pipe` — execute a multi-step workflow pipeline from inline JSON, a file, or a saved named workflow; supports `--var` parameter binding

#### Execution
- `exec <code>` — evaluate JavaScript and return the result; `--raw` prints the value directly without the JSON envelope

#### Inspection
- `inspect page` — get structured page metadata (URL, title, status, navigation info)
- `inspect text <selector>` — read the visible text of elements matching a selector
- `inspect html <selector>` — read the outer HTML of elements matching a selector
- `inspect value <selector>` — read the current value of form inputs
- `inspect attributes <selector>` — read all attributes of elements matching a selector
- `inspect bbox <selector>` — get the bounding box (position and size) of elements
- `inspect list <selector>` — extract structured multi-element data
- `inspect storage` — inspect localStorage and sessionStorage for the current origin
- `inspect network` — inspect the recorded network request timeline; `--wait` blocks until a matching request is observed
- `inspect curl <id>` — export a recorded network request as a reproducible `curl` command

#### Storage
- `storage get` — read a key from web storage
- `storage set` — write a key/value to web storage
- `storage remove` — delete a key from web storage
- `storage clear` — clear all keys from web storage
- `storage export` — export all storage to JSON
- `storage import` — import storage from a JSON payload

#### Tabs
- `tabs` — list all open browser tabs with index, URL, title, and active status
- `switch <index>` — switch to a tab by index and make it the active tab
- `close-tab` — close a tab (current tab if no index specified)

#### Dialogs
- `dialog` — inspect pending JavaScript dialog state
- `dialog accept` — accept a JavaScript `alert` / `confirm` / `prompt`
- `dialog dismiss` — dismiss a JavaScript `confirm` / `prompt`
- `dialog intercept` — pre-arm an intercept to accept or dismiss the next dialog before it appears

#### Cookies
- `cookies get` — read cookies for the current page
- `cookies set` — set a cookie value
- `cookies delete` — delete a cookie
- `cookies clear` — clear all cookies for the current origin
- `cookies export` — export all cookies to JSON
- `cookies import` — import cookies from a JSON payload

#### Downloads
- `downloads` — inspect the current download runtime state
- `download wait` — wait for a download to complete
- `download save` — save a completed download to a destination path
- `download cancel` — cancel an in-progress download

#### Network Interception
- `intercept block <pattern>` — block requests matching a URL pattern
- `intercept allow <pattern>` — allow requests matching a URL pattern (override a block rule)
- `intercept redirect <from> <to>` — redirect requests from one URL pattern to another
- `interference` — public-web interference controls (cookie banners, overlays, tracking noise)

#### Diagnostics
- `doctor` — structured health check covering browser, daemon, socket, and disk state
- `history` — recent command history with timing, confirmation status, and `dom_epoch`; supports workflow export (`--export-pipe` / `--export-script`) and save to named workflow asset (`--save-as`)
- `sessions` — list all active named sessions
- `cleanup` — clean up stale sessions and orphaned browser/daemon artifacts

#### Runtime Surfaces
- `runtime` — session runtime capabilities report
- `runtime integration` — CDP connectivity and protocol check
- `runtime observatory` — CDP event stream diagnostics
- `runtime state-inspector` — live session state snapshot
- `runtime readiness` — page interaction readiness signal
- `runtime handoff` — human-in-the-loop handoff state
- `runtime storage` — web storage runtime status
- `runtime downloads` — download manager runtime status

#### Human Handoff
- `handoff start` — pause automation and elevate to headed browser for human intervention
- `handoff commit` — commit the human-modified state back to the automation session
- `takeover` — full session authority transfer for human operation

#### Trigger & Orchestration
- `trigger` — session-scoped cross-tab trigger registry (register, fire, list, clear)
- `orchestration` — cross-session orchestration rule registry (register, list, cancel, probe)

#### Stealth & Identity
- L1 stealth baseline enabled by default: `navigator.webdriver` suppression, Chrome runtime injection, permissions API override, plugin list normalization, language and platform normalization, environment profile seeding (screen geometry, timezone, hardware concurrency, device memory)
- L2 humanized interaction (`--humanize`): Bézier mouse paths, human-realistic timing presets (`human` / `fast` / `instant`)
- `--no-stealth` flag to opt out for trusted or dev environments
- `--humanize-speed` to control L2 timing preset

#### Session & Daemon
- Auto-start: first command for a session spawns a dedicated daemon; subsequent commands reuse it
- Named sessions: `--session <name>` for parallel isolated browser instances
- `--cdp-url` to attach to an existing Chrome instance via CDP
- `--connect` to connect to a running rub session by socket path
- `--profile` to reuse a persistent Chrome user profile
- Session-scoped command history replayed in structured JSON
- `close` — graceful browser teardown; `--all` closes every active session
- Structured JSON output (`stdout_schema_version: "3.0"`) on every command — consistent envelope across all surfaces
- `command_id` at-most-once execution guarantee for mutating commands
- Post-action waiting via `--wait-after-selector` and `--wait-after-text` on all interaction commands
- Interaction confirmation: `value_applied`, `context_change`, `confirmed`, `unconfirmed`, `contradicted`
- Interaction trace modes: compact (default), `--verbose`, `--trace`
- Workflow parameterization: `--var KEY=VALUE` with `$ENV` secret resolution via `secrets.env`
