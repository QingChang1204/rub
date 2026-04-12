use clap::Subcommand;

use crate::commands::ElementAddressArgs;

const EXPLAIN_EXTRACT_LONG_ABOUT: &str = "\
Explain how `extract` interprets a spec without opening a browser session.

This surface parses the same extract spec contract used by the runtime,
shows the normalized shape after shorthand inference, and adds guidance
when the input is malformed.";

const EXPLAIN_EXTRACT_AFTER_LONG_HELP: &str = "\
Examples:
  Explain an inline spec:
    rub explain extract '{\"title\":\"h1\",\"price\": \".price\"}'

  Explain a spec file:
    rub explain extract --file article.json

Related:
  rub extract --schema
  rub extract --examples
  rub extract --examples collection";

const EXPLAIN_LOCATOR_LONG_ABOUT: &str = "\
Explain how a canonical locator resolves candidates on the current page.

This surface reuses the existing `find` authority, then projects the ordered
candidate set and the winner that `--first`, `--last`, or `--nth` would select.";

const EXPLAIN_LOCATOR_AFTER_LONG_HELP: &str = "\
Examples:
  Explain a text locator:
    rub explain locator --target-text \"New Topic\"

  Explain a label locator inside a strict snapshot fence:
    rub explain locator --snapshot snap-123 --label \"Consent\" --first

Notes:
  This first slice explains interactive snapshot locators.
  Use `rub find --content ...` when you need live content-anchor discovery.";

const EXPLAIN_INTERACTABILITY_LONG_ABOUT: &str = "\
Explain whether a target is likely to be interactable on the current page.

This first slice reuses the canonical snapshot locator authority plus the
runtime readiness/interference surfaces. It explains disabled-state and
ambient blockers without inventing a second interactability DSL.";

const EXPLAIN_INTERACTABILITY_AFTER_LONG_HELP: &str = "\
Examples:
  Explain whether a consent button is blocked:
    rub explain interactability --label \"Consent\"

  Explain a strict snapshot target:
    rub explain interactability --snapshot snap-123 --selector \"button[type=submit]\" --first

Notes:
  This first slice summarizes the target, readiness state, blocking signals,
  and interference hints from the current authoritative surfaces.
  Use `rub explain locator ...` first when you still need to understand
  candidate ordering or ambiguity.";

const EXPLAIN_BLOCKERS_LONG_ABOUT: &str = "\
Explain the current page-level blocker or interference state before you act.

This surface summarizes the canonical readiness and interference projections,
classifies the dominant blocker type, and recommends the next safe command
without requiring raw runtime spelunking.";

const EXPLAIN_BLOCKERS_AFTER_LONG_HELP: &str = "\
Examples:
  Explain the current blocker on the active page:
    rub explain blockers

Notes:
  This surface is page-level, not target-level.
  Use `rub explain interactability ...` when you already know the target and
  need to understand why that specific control is not safely interactable.";

#[derive(Debug, Clone, Subcommand)]
pub enum ExplainSubcommand {
    /// Explain how an extract spec will be normalized and interpreted
    #[command(
        long_about = EXPLAIN_EXTRACT_LONG_ABOUT,
        after_long_help = EXPLAIN_EXTRACT_AFTER_LONG_HELP
    )]
    Extract {
        /// Inline JSON extract specification
        #[arg(conflicts_with = "file", help_heading = "Extract spec input")]
        spec: Option<String>,
        /// Load the extract specification from a JSON file
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "spec",
            help_heading = "Extract spec input"
        )]
        file: Option<String>,
    },
    /// Explain likely interactability blockers for one resolved target
    #[command(
        long_about = EXPLAIN_INTERACTABILITY_LONG_ABOUT,
        after_long_help = EXPLAIN_INTERACTABILITY_AFTER_LONG_HELP
    )]
    Interactability {
        #[command(flatten)]
        target: ElementAddressArgs,
    },
    /// Explain the dominant page-level blocker or interference state
    #[command(
        long_about = EXPLAIN_BLOCKERS_LONG_ABOUT,
        after_long_help = EXPLAIN_BLOCKERS_AFTER_LONG_HELP
    )]
    Blockers,
    /// Explain how a locator resolves ordered candidates and winner selection
    #[command(
        long_about = EXPLAIN_LOCATOR_LONG_ABOUT,
        after_long_help = EXPLAIN_LOCATOR_AFTER_LONG_HELP
    )]
    Locator {
        #[command(flatten)]
        target: ElementAddressArgs,
    },
}
