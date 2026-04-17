pub(super) const OBSERVE_LONG_ABOUT: &str = "\
Atomically capture a token-friendly page summary plus screenshot.

Produces an element index map and screenshot in one round trip.
The `element_map` in the result lists all visible interactive elements
with their numeric index; use that index with `click`, `type`, `hover`,
and `fill` to interact with the page.

After `observe`, interact using the index numbers in `element_map`:
  rub observe --path /tmp/page.png
  rub click 3                    # click element #3 from element_map
  rub type --index 5 \"hello\"     # type into element #5
  rub fill '[{\"index\":3,\"value\":\"hello\"},{\"index\":5,\"value\":\"world\"}]'

Screenshot is base64 in JSON by default; use --path to save to disk.
Use --compact for a token-efficient text summary instead of the full a11y tree.";

pub(super) const TEARDOWN_LONG_ABOUT: &str = "\
Canonical lifecycle exit for one RUB_HOME.

`teardown` is the operator-facing wrapper over:
  1. `close --all`
  2. `cleanup`

It closes active sessions for the target RUB_HOME, waits for daemon
shutdown fences, then sweeps orphaned temporary browser profiles and
stale temp-owned homes for that same authority. If the current RUB_HOME
is itself temp-owned, `teardown` also removes it after release commits.

Examples:
  rub teardown
  rub --rub-home /tmp/rub-bench teardown";

pub(super) const FILL_LONG_ABOUT: &str = "\
Fill multiple form fields through the canonical interaction runtime.

Use `fill` when you already know the fields you want to set and want one
command to apply them, optionally followed by a canonical submit click.

The spec is a JSON array. Each entry targets one field and sets its value.
You can mix locator styles in the same array.";

pub(super) const FILL_AFTER_LONG_HELP: &str = "\
Spec formats:
  By index:
    rub fill '[{\"index\":3,\"value\":\"alice@example.com\"},{\"index\":5,\"value\":\"$RUB_PASSWORD\"}]'

  By label:
    rub fill '[{\"label\":\"Email\",\"value\":\"alice@example.com\"},{\"label\":\"Password\",\"value\":\"$RUB_PASSWORD\"}]'

  By selector:
    rub fill '[{\"selector\":\"#email\",\"value\":\"alice@example.com\"}]'

  By text:
    rub fill '[{\"target_text\":\"Email address\",\"value\":\"alice@example.com\"}]'

Examples:
  Fill and submit:
    rub fill '[{\"label\":\"Email\",\"value\":\"user@example.com\"},{\"label\":\"Password\",\"value\":\"$RUB_PASSWORD\"}]' --submit-label \"Log in\"

  Load spec from a file:
    rub fill --file form.json --submit-label \"Submit\"

Notes:
  Mixed locators are allowed in the same spec array.
  Use `--snapshot` when you want strict preflight continuity against one captured snapshot.
  Use the \"Submit action\" options to click a follow-up button after filling.
  Use the \"Post-action wait\" options when you need an explicit confirmation fence.";

pub(super) const EXTRACT_LONG_ABOUT: &str = "\
Extract structured data through the canonical query surface.

Use `extract` to turn page content into stable JSON fields without dropping
down to ad hoc JavaScript for common scraping/query tasks.";

pub(super) const EXTRACT_AFTER_LONG_HELP: &str = "\
Examples:
  Shorthand field-to-selector mapping:
    rub extract '{\"title\":\"h1\",\"price\":\".price\",\"desc\":\".desc\"}'

  Explicit extraction kind:
    rub extract '{\"title\":{\"selector\":\"h1\",\"kind\":\"text\"}}'

  Attribute extraction:
    rub extract '{\"link\":{\"selector\":\"a.main\",\"kind\":\"attribute\",\"attr\":\"href\"}}'

  Collection extraction:
    rub extract '{\"items\":{\"collection\":\"li.item\",\"fields\":{\"name\":{\"kind\":\"text\"},\"price\":{\"selector\":\".price\"}}}}'

Output shape:
  {\"result\":{\"fields\":{\"title\":\"...\"},\"field_count\":N}}

Use --snapshot when you want strict continuity against a previously captured snapshot.";

pub(super) const PIPE_LONG_ABOUT: &str = "\
Execute a workflow pipeline over existing canonical commands.

SPEC is a JSON array of step objects, each with a `command` key and optional
`args` object. Steps run sequentially; each step result is included in the
final response under `steps[n].result`.

Minimal example (open and take screenshot):
  rub pipe '[{\"command\":\"open\",\"args\":{\"url\":\"https://example.com\"}},{\"command\":\"screenshot\"}]'

Form automation:
  rub pipe '[{\"command\":\"open\",\"args\":{\"url\":\"https://login.example.com\"}},{\"command\":\"fill\",\"args\":{\"spec\":[{\"label\":\"Email\",\"value\":\"user@example.com\"},{\"label\":\"Password\",\"value\":\"$RUB_PASSWORD\"}],\"submit_label\":\"Log in\"}}]'

Named workflow (saved under RUB_HOME/workflows/<name>.json):
  rub secret set RUB_PASSWORD --stdin
  rub pipe --workflow login --var email=user@example.com --var 'password=$RUB_PASSWORD'

Step result references: Use {{prev.result.PATH}} to inject the previous step's
result, or {{steps[N].result.PATH}} / {{steps[LABEL].result.PATH}} to reference
any completed prior step by index or label:
  rub pipe '[{\"command\":\"extract\",\"args\":{\"spec\":\"{\\\"title\\\":\\\"h1\\\"}\"},\"label\":\"get_title\"},{\"command\":\"exec\",\"args\":{\"code\":\"document.title = \\\"{{prev.result.fields.title}}\\\"\"}}]'

Allowed commands in pipe: open, state, click, type, exec, scroll, back,
  keys, wait, tabs, switch, close-tab, get, hover, upload, select, fill, extract";
