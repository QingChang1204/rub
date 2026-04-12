use super::{Commands, ExplainSubcommand, InspectSubcommand, render_nested_subcommand_long_help};
use crate::commands::Cli;
use clap::{CommandFactory, Parser};

fn render_root_long_help() -> String {
    let mut command = Cli::command();
    let mut buffer = Vec::new();
    command
        .write_long_help(&mut buffer)
        .expect("help should render");
    String::from_utf8(buffer).expect("help should be valid utf-8")
}

fn render_subcommand_long_help(name: &str) -> String {
    let mut root = Cli::command();
    let mut subcommand = root
        .find_subcommand_mut(name)
        .unwrap_or_else(|| panic!("missing subcommand {name}"))
        .clone();
    let mut buffer = Vec::new();
    subcommand
        .write_long_help(&mut buffer)
        .expect("help should render");
    String::from_utf8(buffer).expect("help should be valid utf-8")
}

#[test]
fn canonical_name_single_sources_public_command_strings() {
    assert_eq!(
        Commands::Downloads.canonical_name(),
        rub_core::command::CommandName::Downloads.as_str()
    );
    assert_eq!(
        Commands::Doctor.canonical_name(),
        rub_core::command::CommandName::Doctor.as_str()
    );
    assert_eq!(Commands::Sessions.canonical_name(), "sessions");
    assert_eq!(Commands::InternalDaemon.canonical_name(), "__daemon");
}

#[test]
fn local_projection_surface_marks_local_only_variants() {
    assert_eq!(
        Commands::Close { all: true }.local_projection_surface(),
        Some("close --all")
    );
    assert_eq!(
        Commands::Cleanup.local_projection_surface(),
        Some("cleanup")
    );
    assert_eq!(
        Commands::Teardown.local_projection_surface(),
        Some("teardown")
    );
    assert_eq!(
        Commands::InternalDaemon.local_projection_surface(),
        Some("internal daemon")
    );
    assert_eq!(Commands::Downloads.local_projection_surface(), None);
}

#[test]
fn observe_help_uses_index_flag_in_type_example() {
    let help = render_subcommand_long_help("observe");
    assert!(help.contains("rub type --index 5 \"hello\""), "{help}");
    assert!(!help.contains("rub type 5 \"hello\""), "{help}");
}

#[test]
fn pipe_help_examples_use_command_step_key() {
    let help = render_subcommand_long_help("pipe");
    assert!(help.contains("\"command\":\"open\""), "{help}");
    assert!(help.contains("{command, args} step objects"), "{help}");
    assert!(!help.contains("\"cmd\":\"open\""), "{help}");
    assert!(!help.contains("`cmd` key"), "{help}");
}

#[test]
fn exec_help_marks_raw_as_explicit_non_json_surface() {
    let help = render_subcommand_long_help("exec");
    assert!(
        help.contains("Print the result directly instead of the standard JSON envelope"),
        "{help}"
    );
}

#[test]
fn explain_help_points_to_extract_schema_and_examples() {
    let help = render_subcommand_long_help("explain");
    assert!(help.contains("extract"), "{help}");
    assert!(help.contains("blockers"), "{help}");
    assert!(help.contains("interactability"), "{help}");
    assert!(help.contains("locator"), "{help}");
}

#[test]
fn teardown_help_names_close_and_cleanup_authorities() {
    let help = render_subcommand_long_help("teardown");
    assert!(help.contains("close --all"), "{help}");
    assert!(help.contains("cleanup"), "{help}");
    assert!(
        help.contains("rub --rub-home /tmp/rub-bench teardown"),
        "{help}"
    );
}

#[test]
fn root_help_surfaces_quick_start_and_task_map() {
    let help = render_root_long_help();
    assert!(help.contains("Quick start:"), "{help}");
    assert!(help.contains("Task map:"), "{help}");
    assert!(help.contains("rub teardown"), "{help}");
}

#[test]
fn fill_help_uses_grouped_sections_and_multiline_examples() {
    let help = render_subcommand_long_help("fill");
    assert!(help.contains("Fill spec input:"), "{help}");
    assert!(help.contains("Submit action:"), "{help}");
    assert!(help.contains("Post-action wait:"), "{help}");
    assert!(help.contains("Examples:\n  Fill and submit:"), "{help}");
    assert!(
        help.contains("rub fill --file form.json --submit-label"),
        "{help}"
    );
}

#[test]
fn extract_help_uses_examples_and_snapshot_section() {
    let help = render_subcommand_long_help("extract");
    assert!(help.contains("Extract spec input:"), "{help}");
    assert!(help.contains("Snapshot continuity:"), "{help}");
    assert!(help.contains("Built-in help:"), "{help}");
    assert!(help.contains("--examples [<TOPIC>]"), "{help}");
    assert!(help.contains("--schema"), "{help}");
    assert!(
        help.contains("Examples:\n  Shorthand field-to-selector mapping:"),
        "{help}"
    );
    assert!(
        help.contains("Use --snapshot when you want strict continuity"),
        "{help}"
    );
}

#[test]
fn inspect_list_help_surfaces_builder_help_and_wait_builder_examples() {
    let help = render_nested_subcommand_long_help("inspect", "list");
    assert!(help.contains("Built-in help:"), "{help}");
    assert!(help.contains("--builder-help"), "{help}");
    assert!(help.contains("--wait-field"), "{help}");
    assert!(help.contains("--collection"), "{help}");
}

#[test]
fn find_explain_conflicts_with_limit_in_cli_surface() {
    let error = Cli::try_parse_from([
        "rub",
        "find",
        "--target-text",
        "Consent",
        "--explain",
        "--limit",
        "3",
    ])
    .expect_err("find --explain --limit should fail clap parsing");

    let rendered = error.to_string();
    assert!(rendered.contains("--limit"), "{rendered}");
    assert!(rendered.contains("--explain"), "{rendered}");
}

#[test]
fn local_projection_surface_marks_extract_examples_and_schema_as_local_only() {
    assert_eq!(
        Commands::Extract {
            spec: None,
            file: None,
            snapshot: None,
            examples: Some("all".to_string()),
            schema: false,
        }
        .local_projection_surface(),
        Some("extract built-in help")
    );
    assert_eq!(
        Commands::Extract {
            spec: None,
            file: None,
            snapshot: None,
            examples: None,
            schema: true,
        }
        .local_projection_surface(),
        Some("extract built-in help")
    );
    assert_eq!(
        Commands::Inspect(InspectSubcommand::List {
            builder_help: true,
            spec: None,
            file: None,
            collection: None,
            row_scope: None,
            field: Vec::new(),
            snapshot: None,
            scan_until: None,
            scan_key: None,
            max_scrolls: None,
            scroll_amount: None,
            settle_ms: None,
            stall_limit: None,
            wait_field: None,
            wait_contains: None,
            wait_timeout: None,
        })
        .local_projection_surface(),
        Some("inspect list built-in help")
    );
    assert_eq!(
        Commands::Explain {
            subcommand: ExplainSubcommand::Extract {
                spec: Some("{\"title\":\"h1\"}".to_string()),
                file: None,
            }
        }
        .local_projection_surface(),
        Some("explain extract")
    );
    assert_eq!(
        Commands::Explain {
            subcommand: ExplainSubcommand::Locator {
                target: crate::commands::ElementAddressArgs {
                    snapshot: None,
                    element_ref: None,
                    selector: Some(".cta".to_string()),
                    target_text: None,
                    role: None,
                    label: None,
                    testid: None,
                    visible: false,
                    prefer_enabled: false,
                    topmost: false,
                    first: false,
                    last: false,
                    nth: None,
                }
            }
        }
        .local_projection_surface(),
        None
    );
    assert_eq!(
        Commands::Explain {
            subcommand: ExplainSubcommand::Interactability {
                target: crate::commands::ElementAddressArgs {
                    snapshot: None,
                    element_ref: None,
                    selector: Some(".cta".to_string()),
                    target_text: None,
                    role: None,
                    label: None,
                    testid: None,
                    visible: false,
                    prefer_enabled: false,
                    topmost: false,
                    first: false,
                    last: false,
                    nth: None,
                }
            }
        }
        .local_projection_surface(),
        None
    );
}
