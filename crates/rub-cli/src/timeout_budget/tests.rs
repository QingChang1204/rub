use super::helpers::resolve_extract_builder_spec_source;
use super::{
    WAIT_IPC_BUFFER_MS, align_embedded_timeout_authority, build_request, command_timeout_ms,
    humanize_budget_ms_for_command_args,
};
use crate::commands::{
    Commands, CookiesSubcommand, EffectiveCli, ElementAddressArgs, ExplainSubcommand,
    InspectSubcommand, InterceptSubcommand, ObservationProjectionArgs, ObservationScopeArgs,
    OrchestrationSubcommand, RequestedLaunchPolicy, RuntimeSubcommand, StateFormatArg,
    StorageSubcommand, TakeoverSubcommand, TriggerSubcommand, WaitAfterArgs,
};
use rub_core::DEFAULT_WAIT_AFTER_TIMEOUT_MS;
use rub_core::error::ErrorCode;
use std::fs;
use std::path::{Component, Path, PathBuf};

fn cli_with(command: Commands) -> EffectiveCli {
    cli_with_with_home(command, PathBuf::from("/tmp/rub-test"))
}

fn cli_with_with_home(command: Commands, rub_home: PathBuf) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home,
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command,
        cdp_url: None,
        connect: false,
        profile: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: RequestedLaunchPolicy::default(),
        effective_launch_policy: RequestedLaunchPolicy::default(),
    }
}

fn normalize_test_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[test]
fn wait_request_uses_wait_timeout_plus_ipc_buffer() {
    let cli = cli_with(Commands::Wait {
        selector: Some(".ready".to_string()),
        target_text: None,
        role: None,
        label: None,
        testid: None,
        text: None,
        description_contains: None,
        url_contains: None,
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 12_000,
        state: "visible".to_string(),
    });

    let request = build_request(&cli).expect("wait request should build");
    assert_eq!(request.timeout_ms, 12_000 + WAIT_IPC_BUFFER_MS);
    assert_eq!(request.args["timeout_ms"], 12_000);
}

#[test]
fn shrinking_wait_request_timeout_updates_embedded_wait_budget() {
    let cli = cli_with(Commands::Wait {
        selector: Some(".ready".to_string()),
        target_text: None,
        role: None,
        label: None,
        testid: None,
        text: None,
        description_contains: None,
        url_contains: None,
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 12_000,
        state: "visible".to_string(),
    });

    let mut request = build_request(&cli).expect("wait request should build");
    request.timeout_ms = 2_000 + WAIT_IPC_BUFFER_MS;
    align_embedded_timeout_authority(&mut request);
    assert_eq!(request.args["timeout_ms"], 2_000);
}

#[test]
fn download_wait_request_uses_wait_timeout_plus_ipc_buffer() {
    let cli = cli_with(Commands::Download {
        subcommand: crate::commands::DownloadSubcommand::Wait {
            id: None,
            state: crate::commands::DownloadWaitStateArg::Completed,
        },
    });

    let request = build_request(&cli).expect("download wait request should build");
    assert_eq!(request.timeout_ms, 30_000 + WAIT_IPC_BUFFER_MS);
    assert_eq!(request.args["timeout_ms"], 30_000);
}

#[test]
fn inspect_network_wait_request_uses_wait_timeout_plus_ipc_buffer() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::Network {
        id: None,
        wait: true,
        last: None,
        url_match: Some("api/items".to_string()),
        method: None,
        status: None,
        lifecycle: None,
        timeout: Some(18_000),
    }));

    let request = build_request(&cli).expect("inspect network wait request should build");
    assert_eq!(request.timeout_ms, 18_000 + WAIT_IPC_BUFFER_MS);
    assert_eq!(request.args["timeout_ms"], 18_000);
}

#[test]
fn inspect_network_wait_request_uses_global_timeout_when_subcommand_timeout_omitted() {
    let mut cli = cli_with(Commands::Inspect(InspectSubcommand::Network {
        id: None,
        wait: true,
        last: None,
        url_match: Some("api/items".to_string()),
        method: None,
        status: None,
        lifecycle: None,
        timeout: None,
    }));
    cli.timeout = 60_000;

    let request = build_request(&cli).expect("inspect network wait request should build");
    assert_eq!(request.timeout_ms, 60_000 + WAIT_IPC_BUFFER_MS);
    assert_eq!(request.args["timeout_ms"], 60_000);
}

#[test]
fn local_only_commands_fail_fast_in_request_builder() {
    for (command, expected_fragment) in [
        (Commands::Cleanup, "cleanup"),
        (Commands::Teardown, "teardown"),
        (Commands::Sessions, "sessions"),
        (Commands::Close { all: true }, "close --all"),
        (Commands::InternalDaemon, "internal daemon"),
    ] {
        let envelope = build_request(&cli_with(command))
            .expect_err("local-only commands must not project fake IPC requests");
        let envelope = envelope.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InternalError);
        assert!(
            envelope.message.contains(expected_fragment),
            "error should identify the local-only command"
        );
    }
}

#[test]
fn download_save_request_projects_bulk_asset_save_surface() {
    let expected_file = std::env::current_dir().expect("cwd").join("assets.json");
    let cli = cli_with(Commands::Download {
        subcommand: crate::commands::DownloadSubcommand::Save {
            file: "./assets.json".to_string(),
            output_dir: "./saved-assets".to_string(),
            input_field: Some("fields.items".to_string()),
            url_field: Some("image_url".to_string()),
            name_field: Some("note_id".to_string()),
            base_url: Some("https://example.com/".to_string()),
            cookie_url: Some("https://example.com/feed".to_string()),
            limit: Some(50),
            concurrency: 8,
            overwrite: false,
        },
    });

    let request = build_request(&cli).expect("download save request should build");
    assert_eq!(request.command, "download");
    assert_eq!(request.args["sub"], "save");
    assert_eq!(request.args["input_field"], "fields.items");
    assert_eq!(request.args["url_field"], "image_url");
    assert_eq!(request.args["name_field"], "note_id");
    assert_eq!(request.args["base_url"], "https://example.com/");
    assert_eq!(request.args["cookie_url"], "https://example.com/feed");
    assert_eq!(request.args["limit"], 50);
    assert_eq!(request.args["concurrency"], 8);
    assert_eq!(request.args["overwrite"], false);
    assert_eq!(request.timeout_ms, 30_000 + WAIT_IPC_BUFFER_MS);
    let actual_file = PathBuf::from(
        request.args["file"]
            .as_str()
            .expect("download save file path should be serialized"),
    );
    assert_eq!(normalize_test_path(&actual_file), expected_file);
    assert_eq!(
        request.args["file_state"]["path_authority"],
        "cli.download.save.file"
    );
    assert_eq!(
        request.args["file_state"]["path_kind"],
        "download_save_input_file"
    );
    let expected_output_dir = std::env::current_dir().expect("cwd").join("saved-assets");
    let actual_output_dir = PathBuf::from(
        request.args["output_dir"]
            .as_str()
            .expect("download save output dir should be serialized"),
    );
    assert_eq!(normalize_test_path(&actual_output_dir), expected_output_dir);
    assert_eq!(
        request.args["output_dir_state"]["path_authority"],
        "cli.download.save.output_dir"
    );
    assert_eq!(
        request.args["output_dir_state"]["path_kind"],
        "download_save_output_directory"
    );
}

#[test]
fn align_embedded_timeout_authority_shrinks_download_save_timeout_with_request_budget() {
    let cli = cli_with(Commands::Download {
        subcommand: crate::commands::DownloadSubcommand::Save {
            file: "./assets.json".to_string(),
            output_dir: "./saved-assets".to_string(),
            input_field: None,
            url_field: None,
            name_field: None,
            base_url: None,
            cookie_url: None,
            limit: None,
            concurrency: 2,
            overwrite: false,
        },
    });

    let mut request = build_request(&cli).expect("download save request should build");
    request.timeout_ms = 5_000 + WAIT_IPC_BUFFER_MS;
    super::align_embedded_timeout_authority(&mut request);

    assert_eq!(request.args["timeout_ms"], serde_json::json!(5_000));
}

#[test]
fn storage_export_import_requests_resolve_cli_paths_before_ipc() {
    let expected_export = std::env::current_dir()
        .expect("cwd")
        .join("storage-export.json");
    let export_request = build_request(&cli_with(Commands::Storage(StorageSubcommand::Export {
        path: Some("./storage-export.json".to_string()),
    })))
    .expect("export request");
    let export_path = PathBuf::from(
        export_request.args["path"]
            .as_str()
            .expect("export path should serialize"),
    );
    assert_eq!(normalize_test_path(&export_path), expected_export);
    assert_eq!(
        export_request.args["path_state"]["path_authority"],
        "cli.storage.export.path"
    );
    assert_eq!(
        export_request.args["path_state"]["path_kind"],
        "storage_export_file"
    );

    let expected_import = std::env::current_dir()
        .expect("cwd")
        .join("storage-import.json");
    let import_request = build_request(&cli_with(Commands::Storage(StorageSubcommand::Import {
        path: "./storage-import.json".to_string(),
    })))
    .expect("import request");
    let import_path = PathBuf::from(
        import_request.args["path"]
            .as_str()
            .expect("import path should serialize"),
    );
    assert_eq!(normalize_test_path(&import_path), expected_import);
    assert_eq!(
        import_request.args["path_state"]["path_authority"],
        "cli.storage.import.path"
    );
    assert_eq!(
        import_request.args["path_state"]["path_kind"],
        "storage_import_file"
    );
}

#[test]
fn cookies_export_import_requests_resolve_cli_paths_before_ipc() {
    let expected_export = std::env::current_dir()
        .expect("cwd")
        .join("cookies-export.json");
    let export_request = build_request(&cli_with(Commands::Cookies(CookiesSubcommand::Export {
        path: "./cookies-export.json".to_string(),
    })))
    .expect("cookies export request");
    let export_path = PathBuf::from(
        export_request.args["path"]
            .as_str()
            .expect("cookies export path should serialize"),
    );
    assert_eq!(normalize_test_path(&export_path), expected_export);
    assert_eq!(
        export_request.args["path_state"]["path_authority"],
        "cli.cookies.export.path"
    );
    assert_eq!(
        export_request.args["path_state"]["path_kind"],
        "cookies_export_file"
    );

    let expected_import = std::env::current_dir()
        .expect("cwd")
        .join("cookies-import.json");
    let import_request = build_request(&cli_with(Commands::Cookies(CookiesSubcommand::Import {
        path: "./cookies-import.json".to_string(),
    })))
    .expect("cookies import request");
    let import_path = PathBuf::from(
        import_request.args["path"]
            .as_str()
            .expect("cookies import path should serialize"),
    );
    assert_eq!(normalize_test_path(&import_path), expected_import);
    assert_eq!(
        import_request.args["path_state"]["path_authority"],
        "cli.cookies.import.path"
    );
    assert_eq!(
        import_request.args["path_state"]["path_kind"],
        "cookies_import_file"
    );
}

#[test]
fn cookies_set_request_omits_unset_optional_fields() {
    let request = build_request(&cli_with(Commands::Cookies(CookiesSubcommand::Set {
        name: "session".to_string(),
        value: "abc".to_string(),
        domain: None,
        path: "/".to_string(),
        secure: false,
        http_only: false,
        same_site: None,
        expires: None,
    })))
    .expect("cookies set request");

    assert_eq!(request.command, "cookies");
    assert_eq!(request.args["sub"], "set");
    assert_eq!(request.args["name"], "session");
    assert_eq!(request.args["value"], "abc");
    assert!(request.args.get("domain").is_none(), "{request:?}");
    assert!(request.args.get("same_site").is_none(), "{request:?}");
    assert!(request.args.get("expires").is_none(), "{request:?}");
}

#[test]
fn humanized_typing_budget_scales_with_text_length() {
    let mut cli = cli_with(Commands::Type {
        index: None,
        target: ElementAddressArgs {
            selector: Some("#field".to_string()),
            ..ElementAddressArgs::default()
        },
        clear: false,
        text_flag: None,
        text: Some("hello world".to_string()),
        wait_after: WaitAfterArgs::default(),
    });
    cli.humanize = true;
    cli.humanize_speed = "slow".to_string();

    let request = build_request(&cli).expect("type request should build");
    assert!(request.timeout_ms > cli.timeout);
}

#[test]
fn default_non_humanized_commands_use_exact_timeout() {
    let cli = cli_with(Commands::Doctor);
    let request = build_request(&cli).expect("doctor request should build");
    assert_eq!(request.timeout_ms, command_timeout_ms(&cli));
    assert_eq!(request.timeout_ms, 30_000);
}

#[test]
fn pipe_file_request_loads_spec_and_records_file_source() {
    let path = std::env::temp_dir().join(format!("rub-pipe-spec-{}.json", uuid::Uuid::now_v7()));
    fs::write(&path, "[{\"command\":\"doctor\",\"args\":{}}]")
        .expect("pipe spec file should be written");

    let cli = cli_with(Commands::Pipe {
        spec: None,
        file: Some(path.display().to_string()),
        workflow: None,
        list_workflows: false,
        vars: Vec::new(),
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("pipe request should build");
    assert_eq!(
        request.args["spec"],
        serde_json::json!([{ "command": "doctor", "args": {} }])
    );
    assert_eq!(request.args["spec_source"]["kind"], "file");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.pipe.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_pipe_file_option"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_kind"],
        "workflow_spec_file"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn pipe_file_request_missing_file_reports_path_context() {
    let path = std::env::temp_dir().join(format!("rub-pipe-missing-{}.json", uuid::Uuid::now_v7()));
    let _ = fs::remove_file(&path);

    let cli = cli_with(Commands::Pipe {
        spec: None,
        file: Some(path.display().to_string()),
        workflow: None,
        list_workflows: false,
        vars: Vec::new(),
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli).expect_err("missing pipe file should be rejected");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::FileNotFound);
    let context = envelope.context.expect("pipe file error context");
    assert_eq!(context["reason"], "pipe_spec_file_not_found");
    assert_eq!(
        context["path_state"]["path_authority"],
        "cli.pipe.spec_source.path"
    );
}

#[test]
fn fill_file_request_loads_spec_and_records_file_source() {
    let path = std::env::temp_dir().join(format!("rub-fill-spec-{}.json", uuid::Uuid::now_v7()));
    fs::write(&path, r##"[{"selector":"#name","value":"Ada"}]"##).expect("fill spec file");

    let cli = cli_with(Commands::Fill {
        spec: None,
        file: Some(path.display().to_string()),
        validate: false,
        atomic: false,
        snapshot: None,
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("fill request should build");
    assert_eq!(
        request.args["spec"],
        serde_json::json!([{ "selector": "#name", "value": "Ada" }])
    );
    assert_eq!(request.args["spec_source"]["kind"], "file");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.fill.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_fill_file_option"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn fill_file_request_missing_file_reports_path_context() {
    let path = std::env::temp_dir().join(format!("rub-fill-missing-{}.json", uuid::Uuid::now_v7()));
    let _ = fs::remove_file(&path);

    let cli = cli_with(Commands::Fill {
        spec: None,
        file: Some(path.display().to_string()),
        validate: false,
        atomic: false,
        snapshot: None,
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli).expect_err("missing fill file should be rejected");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::FileNotFound);
    let context = envelope.context.expect("fill file error context");
    assert_eq!(context["reason"], "json_spec_file_not_found");
    assert_eq!(
        context["path_state"]["path_authority"],
        "cli.fill.spec_source.path"
    );
}

#[test]
fn fill_request_extends_timeout_for_step_wait_after_budget() {
    let cli = cli_with(Commands::Fill {
        spec: Some(
            r##"[
                {"selector":"#name","value":"Ada","wait_after":{"selector":"#ready","timeout_ms":1200}},
                {"selector":"#submit","activate":true}
            ]"##
            .to_string(),
        ),
        file: None,
        validate: false,
        atomic: false,
        snapshot: None,
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("fill request should build");
    assert_eq!(request.timeout_ms, command_timeout_ms(&cli) + 1_200);
}

#[test]
fn fill_validate_request_uses_internal_read_only_projection() {
    let cli = cli_with(Commands::Fill {
        spec: Some(r##"[{"selector":"#name","value":"Ada"}]"##.to_string()),
        file: None,
        validate: true,
        atomic: false,
        snapshot: None,
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("fill validate request should build");
    assert_eq!(request.command, "_fill_validate");
    assert!(
        request.command_id.is_none(),
        "validate surface should not look mutating at the protocol layer"
    );
}

#[test]
fn fill_snapshot_request_projects_snapshot_id_into_fill_args() {
    let cli = cli_with(Commands::Fill {
        spec: Some(r##"[{"selector":"#name","value":"Ada"}]"##.to_string()),
        file: None,
        validate: false,
        atomic: false,
        snapshot: Some("snap-123".to_string()),
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("fill snapshot request should build");
    assert_eq!(request.command, "fill");
    assert_eq!(request.args["snapshot_id"], "snap-123");
}

#[test]
fn fill_atomic_request_projects_flag_and_reserves_rollback_budget() {
    let cli = cli_with(Commands::Fill {
        spec: Some(r##"[{"selector":"#name","value":"Ada"}]"##.to_string()),
        file: None,
        validate: false,
        atomic: true,
        snapshot: None,
        submit_index: None,
        submit_selector: None,
        submit_target_text: None,
        submit_ref: None,
        submit_role: None,
        submit_label: None,
        submit_testid: None,
        submit_first: false,
        submit_last: false,
        submit_nth: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("atomic fill request should build");
    assert_eq!(request.command, "fill");
    assert_eq!(request.args["atomic"], true);
    assert!(
        request.timeout_ms > command_timeout_ms(&cli),
        "atomic fill should reserve additional rollback budget"
    );
}

#[test]
fn extract_file_request_loads_spec_and_records_file_source() {
    let path = std::env::temp_dir().join(format!("rub-extract-spec-{}.json", uuid::Uuid::now_v7()));
    fs::write(&path, r#"{"headline":{"selector":"h1","kind":"text"}}"#).expect("extract spec file");

    let cli = cli_with(Commands::Extract {
        spec: None,
        file: Some(path.display().to_string()),
        snapshot: None,
        examples: None,
        schema: false,
    });

    let request = build_request(&cli).expect("extract request should build");
    assert_eq!(
        request.args["spec"],
        serde_json::json!({ "headline": { "selector": "h1", "kind": "text" } })
    );
    assert_eq!(request.args["spec_source"]["kind"], "file");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.extract.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_extract_file_option"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn extract_builtin_help_is_local_only_projection() {
    let cli = cli_with(Commands::Extract {
        spec: None,
        file: None,
        snapshot: None,
        examples: Some("all".to_string()),
        schema: false,
    });

    let error = build_request(&cli).expect_err("extract built-in help should stay local");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InternalError);
    assert_eq!(
        envelope.message,
        "extract built-in help must be handled locally before IPC request projection"
    );
}

#[test]
fn explain_extract_is_local_only_projection() {
    let cli = cli_with(Commands::Explain {
        subcommand: ExplainSubcommand::Extract {
            spec: Some("{\"title\":\"h1\"}".to_string()),
            file: None,
        },
    });

    let error = build_request(&cli).expect_err("explain extract should stay local");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InternalError);
    assert_eq!(
        envelope.message,
        "explain extract must be handled locally before IPC request projection"
    );
}

#[test]
fn explain_locator_projects_to_find_without_selection() {
    let cli = cli_with(Commands::Explain {
        subcommand: ExplainSubcommand::Locator {
            target: ElementAddressArgs {
                snapshot: Some("snap-123".to_string()),
                element_ref: None,
                selector: None,
                target_text: Some("New Topic".to_string()),
                role: None,
                label: None,
                testid: None,
                visible: false,
                prefer_enabled: false,
                topmost: false,
                first: false,
                last: false,
                nth: Some(2),
            },
        },
    });

    let request = build_request(&cli).expect("explain locator should build");
    assert_eq!(request.command, "find");
    assert_eq!(request.args["snapshot_id"], "snap-123");
    assert_eq!(request.args["target_text"], "New Topic");
    assert_eq!(request.args["first"], false);
    assert_eq!(request.args["last"], false);
    assert_eq!(request.args["nth"], serde_json::Value::Null);
}

#[test]
fn find_explain_projects_to_find_without_selection() {
    let cli = cli_with(Commands::Find {
        target: ElementAddressArgs {
            snapshot: Some("snap-123".to_string()),
            element_ref: None,
            selector: None,
            target_text: Some("New Topic".to_string()),
            role: None,
            label: None,
            testid: None,
            visible: false,
            prefer_enabled: false,
            topmost: false,
            first: false,
            last: false,
            nth: Some(2),
        },
        content: false,
        explain: true,
        limit: None,
    });

    let request = build_request(&cli).expect("find --explain should build");
    assert_eq!(request.command, "find");
    assert_eq!(request.args["snapshot_id"], "snap-123");
    assert_eq!(request.args["target_text"], "New Topic");
    assert_eq!(request.args["content"], false);
    assert_eq!(request.args["explain"], true);
    assert_eq!(request.args["limit"], serde_json::Value::Null);
    assert_eq!(request.args["first"], false);
    assert_eq!(request.args["last"], false);
    assert_eq!(request.args["nth"], serde_json::Value::Null);
}

#[test]
fn find_explain_rejects_limit_because_explain_needs_full_candidate_set() {
    let cli = cli_with(Commands::Find {
        target: ElementAddressArgs {
            snapshot: Some("snap-123".to_string()),
            element_ref: None,
            selector: None,
            target_text: Some("New Topic".to_string()),
            role: None,
            label: None,
            testid: None,
            visible: false,
            prefer_enabled: false,
            topmost: false,
            first: false,
            last: false,
            nth: Some(2),
        },
        content: false,
        explain: true,
        limit: Some(5),
    });

    let envelope = build_request(&cli)
        .expect_err("find --explain --limit must fail closed")
        .into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert!(
        envelope
            .message
            .contains("full authoritative candidate set"),
        "{}",
        envelope.message
    );
}

#[test]
fn find_explain_projects_topmost_ranking_flag() {
    let cli = cli_with(Commands::Find {
        target: ElementAddressArgs {
            snapshot: Some("snap-789".to_string()),
            element_ref: None,
            selector: Some(".cta".to_string()),
            target_text: None,
            role: None,
            label: None,
            testid: None,
            visible: true,
            prefer_enabled: true,
            topmost: true,
            first: false,
            last: false,
            nth: None,
        },
        content: false,
        explain: true,
        limit: None,
    });

    let request = build_request(&cli).expect("find --explain should build with topmost");
    assert_eq!(request.command, "find");
    assert_eq!(request.args["snapshot_id"], "snap-789");
    assert_eq!(request.args["selector"], ".cta");
    assert_eq!(request.args["visible"], true);
    assert_eq!(request.args["prefer_enabled"], true);
    assert_eq!(request.args["topmost"], true);
    assert_eq!(request.args["first"], false);
    assert_eq!(request.args["last"], false);
    assert_eq!(request.args["nth"], serde_json::Value::Null);
}

#[test]
fn explain_interactability_projects_to_internal_probe() {
    let cli = cli_with(Commands::Explain {
        subcommand: ExplainSubcommand::Interactability {
            target: ElementAddressArgs {
                snapshot: Some("snap-321".to_string()),
                element_ref: None,
                selector: None,
                target_text: None,
                role: None,
                label: Some("Consent".to_string()),
                testid: None,
                visible: false,
                prefer_enabled: false,
                topmost: false,
                first: true,
                last: false,
                nth: None,
            },
        },
    });

    let request = build_request(&cli).expect("explain interactability should build");
    assert_eq!(request.command, "_interactability_probe");
    assert_eq!(request.args["snapshot_id"], "snap-321");
    assert_eq!(request.args["label"], "Consent");
    assert_eq!(request.args["first"], true);
}

#[test]
fn explain_blockers_projects_to_internal_probe() {
    let cli = cli_with(Commands::Explain {
        subcommand: ExplainSubcommand::Blockers,
    });

    let request = build_request(&cli).expect("explain blockers should build");
    assert_eq!(request.command, "_blocker_diagnose");
    assert_eq!(request.args, serde_json::json!({}));
}

#[test]
fn history_export_script_request_serializes_export_flags() {
    let cli = cli_with(Commands::History {
        last: 8,
        from: None,
        to: None,
        export_pipe: false,
        export_script: true,
        include_observation: true,
        save_as: None,
        output: None,
    });

    let request = build_request(&cli).expect("history export-script should build");
    assert_eq!(request.args["last"], 8);
    assert_eq!(request.args["from"], serde_json::Value::Null);
    assert_eq!(request.args["to"], serde_json::Value::Null);
    assert_eq!(request.args["export_pipe"], false);
    assert_eq!(request.args["export_script"], true);
    assert_eq!(request.args["include_observation"], true);
}

#[test]
fn pipe_request_parameterizes_workflow_file_before_dispatch() {
    let path = std::env::temp_dir().join(format!("rub-pipe-vars-{}.json", uuid::Uuid::now_v7()));
    fs::write(
        &path,
        r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#,
    )
    .expect("pipe spec file should be written");

    let cli = cli_with(Commands::Pipe {
        spec: None,
        file: Some(path.display().to_string()),
        workflow: None,
        list_workflows: false,
        vars: vec![String::from("target_url=https://example.com")],
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("pipe request should build");
    assert_eq!(
        request.args["spec"],
        serde_json::json!({ "steps": [{ "command": "open", "args": { "url": "https://example.com" } }] })
    );
    assert_eq!(
        request.args["spec_source"]["vars"],
        serde_json::json!(["target_url"])
    );

    let _ = fs::remove_file(path);
}

#[test]
fn pipe_request_extends_timeout_for_step_waits_and_humanize() {
    let mut cli = cli_with(Commands::Pipe {
        spec: Some(
            r##"{
                "steps":[
                    {"command":"type","args":{"text":"hello"}},
                    {"command":"click","args":{"selector":"#go","wait_after":{"selector":"#done","timeout_ms":1500}}},
                    {"command":"wait","args":{"selector":"#done","timeout_ms":2000}}
                ]
            }"##
            .to_string(),
        ),
        file: None,
        workflow: None,
        list_workflows: false,
        vars: Vec::new(),
        wait_after: WaitAfterArgs::default(),
    });
    cli.humanize = true;
    cli.humanize_speed = "slow".to_string();

    let request = build_request(&cli).expect("pipe request should build");
    let expected_extra = 1_500
        + 2_000
        + humanize_budget_ms_for_command_args(
            "type",
            &serde_json::json!({ "text": "hello" }),
            true,
            "slow",
        )
        + humanize_budget_ms_for_command_args("click", &serde_json::json!({}), true, "slow");
    assert_eq!(
        request.timeout_ms,
        command_timeout_ms(&cli) + expected_extra
    );
}

#[test]
fn pipe_request_extends_timeout_for_legacy_array_form_specs() {
    let cli = cli_with(Commands::Pipe {
        spec: Some(
            r##"[
                {"command":"click","args":{"selector":"#go","wait_after":{"selector":"#done","timeout_ms":800}}},
                {"command":"wait","args":{"selector":"#done","timeout_ms":1200}}
            ]"##
            .to_string(),
        ),
        file: None,
        workflow: None,
        list_workflows: false,
        vars: Vec::new(),
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("legacy array-form pipe request should build");
    assert_eq!(request.timeout_ms, command_timeout_ms(&cli) + 2_000);
}

#[test]
fn select_request_accepts_explicit_value_flag() {
    let cli = cli_with(Commands::Select {
        operands: Vec::new(),
        value: Some("California".to_string()),
        target: ElementAddressArgs {
            selector: Some("select[name=region]".to_string()),
            ..ElementAddressArgs::default()
        },
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("select request should build");
    assert_eq!(request.command, "select");
    assert_eq!(request.args["selector"], "select[name=region]");
    assert_eq!(request.args["value"], "California");
}

#[test]
fn scroll_y_shorthand_builds_direction_and_amount() {
    let cli = cli_with(Commands::Scroll {
        direction: "down".to_string(),
        amount: None,
        y: Some(-240),
    });

    let request = build_request(&cli).expect("scroll request should build");
    assert_eq!(request.command, "scroll");
    assert_eq!(request.args["direction"], "up");
    assert_eq!(request.args["amount"], 240);
}

#[test]
fn click_xy_rejects_conflicting_locator_inputs_before_bootstrap() {
    let cli = cli_with(Commands::Click {
        index: Some(3),
        target: ElementAddressArgs {
            selector: Some(".submit".to_string()),
            ..ElementAddressArgs::default()
        },
        xy: Some(vec![10.0, 20.0]),
        double: false,
        right: false,
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli).expect_err("conflicting click inputs should be rejected");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn scroll_y_rejects_explicit_direction_conflict() {
    let cli = cli_with(Commands::Scroll {
        direction: "up".to_string(),
        amount: None,
        y: Some(240),
    });

    let error = build_request(&cli).expect_err("conflicting scroll inputs should be rejected");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn screenshot_positional_path_builds_output_path() {
    let cli = cli_with(Commands::Screenshot {
        path: Some("capture.png".to_string()),
        output_path: None,
        full: false,
        highlight: false,
    });

    let request = build_request(&cli).expect("screenshot request should build");
    assert_eq!(request.command, "screenshot");
    let actual = PathBuf::from(
        request.args["path"]
            .as_str()
            .expect("screenshot path should serialize"),
    );
    let expected = std::env::current_dir().expect("cwd").join("capture.png");
    assert_eq!(normalize_test_path(&actual), expected);
    assert_eq!(
        request.args["path_state"]["path_authority"],
        "cli.screenshot.path"
    );
    assert_eq!(
        request.args["path_state"]["path_kind"],
        "screenshot_output_file"
    );
}

#[test]
fn observe_request_resolves_output_path_before_ipc() {
    let cli = cli_with(Commands::Observe {
        path: Some("./captures/observe.png".to_string()),
        full: false,
        limit: Some(12),
        scope: ObservationScopeArgs::default(),
        projection: ObservationProjectionArgs::default(),
    });

    let request = build_request(&cli).expect("observe request should build");
    let actual = PathBuf::from(
        request.args["path"]
            .as_str()
            .expect("observe path should serialize"),
    );
    let expected = std::env::current_dir()
        .expect("cwd")
        .join("captures/observe.png");
    assert_eq!(normalize_test_path(&actual), expected);
    assert_eq!(
        request.args["path_state"]["path_authority"],
        "cli.observe.path"
    );
    assert_eq!(
        request.args["path_state"]["path_kind"],
        "observe_output_file"
    );
}

#[test]
fn upload_request_resolves_input_path_before_ipc() {
    let cli = cli_with(Commands::Upload {
        operands: vec!["./fixtures/upload.txt".to_string()],
        target: ElementAddressArgs {
            selector: Some("input[type=file]".to_string()),
            ..ElementAddressArgs::default()
        },
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("upload request should build");
    let actual = PathBuf::from(
        request.args["path"]
            .as_str()
            .expect("upload path should serialize"),
    );
    let expected = std::env::current_dir()
        .expect("cwd")
        .join("fixtures/upload.txt");
    assert_eq!(normalize_test_path(&actual), expected);
    assert_eq!(
        request.args["path_state"]["path_authority"],
        "cli.upload.path"
    );
    assert_eq!(request.args["path_state"]["path_kind"], "upload_input_file");
}

#[test]
fn storage_set_defaults_to_local_area() {
    let cli = cli_with(Commands::Storage(crate::commands::StorageSubcommand::Set {
        key: "token".to_string(),
        value: "abc".to_string(),
        area: None,
    }));

    let request = build_request(&cli).expect("storage set request should build");
    assert_eq!(request.command, "storage");
    assert_eq!(request.args["sub"], "set");
    assert_eq!(request.args["area"], "local");
}

#[test]
fn inspect_list_builder_request_compiles_collection_and_fields() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
        builder_help: false,
        spec: None,
        file: None,
        collection: Some("option".to_string()),
        row_scope: Some(".flavor-card".to_string()),
        field: vec![
            "text".to_string(),
            "value=attribute:value".to_string(),
            "label=text:.label".to_string(),
        ],
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
    }));

    let request = build_request(&cli).expect("inspect list request should build");
    assert_eq!(request.command, "inspect");
    assert_eq!(request.args["sub"], "list");
    assert_eq!(request.args["spec_source"]["kind"], "builder");
    assert_eq!(request.args["spec_source"]["collection"], "option");
    assert_eq!(
        request.args["spec_source"]["row_scope_selector"],
        ".flavor-card"
    );
    let spec = request.args["spec"].clone();
    assert_eq!(spec["items"]["collection"], "option");
    assert_eq!(spec["items"]["row_scope_selector"], ".flavor-card");
    assert_eq!(spec["items"]["fields"]["text"]["kind"], "text");
    assert_eq!(spec["items"]["fields"]["value"]["kind"], "attribute");
    assert_eq!(spec["items"]["fields"]["value"]["attribute"], "value");
    assert_eq!(spec["items"]["fields"]["label"]["selector"], ".label");
}

#[test]
fn inspect_list_file_request_loads_spec_and_records_file_source() {
    let path = std::env::temp_dir().join(format!("rub-inspect-list-{}.json", uuid::Uuid::now_v7()));
    fs::write(
        &path,
        r#"{"items":{"collection":".item","fields":{"label":{"kind":"text"}}}}"#,
    )
    .expect("inspect list spec file");

    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
        builder_help: false,
        spec: None,
        file: Some(path.display().to_string()),
        collection: None,
        row_scope: None,
        field: Vec::new(),
        snapshot: Some("snap-1".to_string()),
        scan_until: None,
        scan_key: None,
        max_scrolls: None,
        scroll_amount: None,
        settle_ms: None,
        stall_limit: None,
        wait_field: None,
        wait_contains: None,
        wait_timeout: None,
    }));

    let request = build_request(&cli).expect("inspect list request should build");
    assert_eq!(request.args["sub"], "list");
    assert_eq!(request.args["snapshot_id"], "snap-1");
    assert_eq!(request.args["spec_source"]["kind"], "file");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.inspect_list.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_inspect_list_file_option"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn inspect_list_scan_request_projects_bounded_scan_surface() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
        builder_help: false,
        spec: None,
        file: None,
        collection: Some(".feed-item".to_string()),
        row_scope: Some(".feed-card".to_string()),
        field: vec![
            "note_id=attribute:data-id".to_string(),
            "title=text:.title".to_string(),
        ],
        snapshot: None,
        scan_until: Some(250),
        scan_key: Some("note_id".to_string()),
        max_scrolls: Some(40),
        scroll_amount: Some(1800),
        settle_ms: Some(900),
        stall_limit: Some(3),
        wait_field: None,
        wait_contains: None,
        wait_timeout: None,
    }));

    let request = build_request(&cli).expect("inspect list scan request should build");
    assert_eq!(request.command, "inspect");
    assert_eq!(request.args["sub"], "list");
    assert_eq!(
        request.args["spec_source"]["row_scope_selector"],
        ".feed-card"
    );
    assert_eq!(request.args["scan_until"], 250);
    assert_eq!(request.args["scan_key"], "note_id");
    assert_eq!(request.args["max_scrolls"], 40);
    assert_eq!(request.args["scroll_amount"], 1800);
    assert_eq!(request.args["settle_ms"], 900);
    assert_eq!(request.args["stall_limit"], 3);
    assert_eq!(request.timeout_ms, 30_000 + 40 * 900 + WAIT_IPC_BUFFER_MS);
}

#[test]
fn inspect_list_wait_request_projects_match_probe_and_timeout() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
        builder_help: false,
        spec: None,
        file: None,
        collection: Some(".mail-row".to_string()),
        row_scope: None,
        field: vec![
            "subject=text:.subject".to_string(),
            "from=text:.from".to_string(),
        ],
        snapshot: None,
        scan_until: None,
        scan_key: None,
        max_scrolls: None,
        scroll_amount: None,
        settle_ms: None,
        stall_limit: None,
        wait_field: Some("subject".to_string()),
        wait_contains: Some("Confirm your new account".to_string()),
        wait_timeout: Some(12_500),
    }));

    let request = build_request(&cli).expect("inspect list wait request should build");
    assert_eq!(request.command, "inspect");
    assert_eq!(request.args["sub"], "list");
    assert_eq!(request.args["wait_field"], "subject");
    assert_eq!(request.args["wait_contains"], "Confirm your new account");
    assert_eq!(request.args["wait_timeout_ms"], 12_500);
    assert_eq!(request.timeout_ms, 12_500 + WAIT_IPC_BUFFER_MS);
}

#[test]
fn inspect_list_wait_rejects_scan_and_wait_combination_before_bootstrap() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
        builder_help: false,
        spec: None,
        file: None,
        collection: Some(".mail-row".to_string()),
        row_scope: None,
        field: vec!["subject=text:.subject".to_string()],
        snapshot: None,
        scan_until: Some(25),
        scan_key: None,
        max_scrolls: None,
        scroll_amount: None,
        settle_ms: None,
        stall_limit: None,
        wait_field: Some("subject".to_string()),
        wait_contains: Some("Confirm".to_string()),
        wait_timeout: None,
    }));

    let error = build_request(&cli)
        .expect_err("inspect list wait must not mix with scan yet")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("cannot be combined"), "{error}");
}

#[test]
fn inspect_harvest_is_handled_locally() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::Harvest {
        file: "feed.json".to_string(),
        input_field: Some("data.fields.items".to_string()),
        url_field: Some("href".to_string()),
        name_field: Some("title".to_string()),
        base_url: Some("https://example.test".to_string()),
        extract: None,
        extract_file: Some("detail.json".to_string()),
        field: Vec::new(),
        limit: Some(10),
    }));

    let error = build_request(&cli).expect_err("inspect harvest should stay local");
    assert!(error.to_string().contains("handled locally"));
}

#[test]
fn inspect_list_builder_help_is_local_only_projection() {
    let cli = cli_with(Commands::Inspect(InspectSubcommand::List {
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
    }));

    let error = build_request(&cli).expect_err("inspect list built-in help should stay local");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InternalError);
    assert_eq!(
        envelope.message,
        "inspect list built-in help must be handled locally before IPC request projection"
    );
}

#[test]
fn extract_builder_spec_source_supports_common_field_shorthand() {
    let (spec, source) = resolve_extract_builder_spec_source(
        "inspect harvest",
        &[
            "title=text:.title".to_string(),
            "hero=attribute:src:img.hero".to_string(),
        ],
    )
    .expect("builder field shorthand should compile");

    let parsed = spec.as_value().clone();
    assert_eq!(source["kind"], "builder");
    assert_eq!(source["fields"], serde_json::json!(["title", "hero"]));
    assert_eq!(
        parsed,
        serde_json::json!({
            "title": { "kind": "text", "selector": ".title" },
            "hero": { "kind": "attribute", "attribute": "src", "selector": "img.hero" }
        })
    );
}

#[test]
fn extract_builder_spec_source_supports_match_selection_suffixes() {
    let (spec, _) = resolve_extract_builder_spec_source(
        "inspect harvest",
        &[
            "availability=text:.instock.availability@first".to_string(),
            "tags=.tag@many".to_string(),
            "hero=attribute:src:img.hero@nth(1)".to_string(),
        ],
    )
    .expect("builder field shorthand with selection should compile");

    let parsed = spec.as_value().clone();
    assert_eq!(
        parsed,
        serde_json::json!({
            "availability": { "kind": "text", "selector": ".instock.availability", "first": true },
            "tags": { "kind": "text", "selector": ".tag", "many": true },
            "hero": { "kind": "attribute", "attribute": "src", "selector": "img.hero", "nth": 1 }
        })
    );
}

#[test]
fn extract_builder_spec_source_supports_semantic_locator_shorthand() {
    let (spec, _) = resolve_extract_builder_spec_source(
        "inspect harvest",
        &[
            "headline=role:heading@first".to_string(),
            "cta=label:Follow".to_string(),
            "search=value:label:Search".to_string(),
            "hero=attribute:src:testid:hero-image".to_string(),
            "summary=html:target_text:Read more".to_string(),
        ],
    )
    .expect("semantic builder shorthand should compile");

    let parsed = spec.as_value().clone();
    assert_eq!(
        parsed,
        serde_json::json!({
            "headline": { "kind": "text", "role": "heading", "first": true },
            "cta": { "kind": "text", "label": "Follow" },
            "search": { "kind": "value", "label": "Search" },
            "hero": { "kind": "attribute", "attribute": "src", "testid": "hero-image" },
            "summary": { "kind": "html", "target_text": "Read more" }
        })
    );
}

#[test]
fn intercept_header_request_accepts_positional_name_and_value() {
    let cli = cli_with(Commands::Intercept {
        subcommand: InterceptSubcommand::Header {
            url_pattern: "https://httpbin.org/*".to_string(),
            name: Some("X-Rub-Live".to_string()),
            value: Some("1".to_string()),
            headers: Vec::new(),
        },
    });

    let request = build_request(&cli).expect("intercept header request should build");
    assert_eq!(request.command, "intercept");
    assert_eq!(request.args["sub"], "header");
    assert_eq!(request.args["headers"], serde_json::json!(["X-Rub-Live=1"]));
}

#[test]
fn history_include_observation_requires_export_surface() {
    let cli = cli_with(Commands::History {
        last: 8,
        from: None,
        to: None,
        export_pipe: false,
        export_script: false,
        include_observation: true,
        save_as: None,
        output: None,
    });

    let error = build_request(&cli).expect_err("include-observation should require export");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn history_export_pipe_request_serializes_range_filters() {
    let cli = cli_with(Commands::History {
        last: 10,
        from: Some(20),
        to: Some(28),
        export_pipe: true,
        export_script: false,
        include_observation: false,
        save_as: None,
        output: None,
    });

    let request = build_request(&cli).expect("history export-pipe should build");
    assert_eq!(request.args["from"], 20);
    assert_eq!(request.args["to"], 28);
}

#[test]
fn history_save_as_requires_pipe_export_surface() {
    let cli = cli_with(Commands::History {
        last: 10,
        from: None,
        to: None,
        export_pipe: false,
        export_script: true,
        include_observation: false,
        save_as: Some("login_flow".to_string()),
        output: None,
    });

    let error = build_request(&cli).expect_err("save-as should require pipe export");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn pipe_request_loads_named_workflow_assets() {
    let home = std::env::temp_dir().join(format!("rub-pipe-workflow-{}", uuid::Uuid::now_v7()));
    let workflows = home.join("workflows");
    fs::create_dir_all(&workflows).expect("workflow dir");
    let workflow_path = workflows.join("login_flow.json");
    fs::write(
        &workflow_path,
        r#"{"steps":[{"command":"doctor","args":{}}]}"#,
    )
    .expect("workflow file");

    let mut cli = cli_with(Commands::Pipe {
        spec: None,
        file: None,
        workflow: Some("login_flow".to_string()),
        list_workflows: false,
        vars: Vec::new(),
        wait_after: WaitAfterArgs::default(),
    });
    cli.rub_home = home.clone();

    let request = build_request(&cli).expect("pipe request should build");
    assert_eq!(request.args["spec_source"]["kind"], "workflow");
    assert_eq!(request.args["spec_source"]["name"], "login_flow");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(workflow_path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.pipe.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_pipe_workflow_option"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn runtime_takeover_subcommand_projects_canonical_runtime_surface() {
    let cli = cli_with(Commands::Runtime {
        subcommand: Some(RuntimeSubcommand::Takeover),
    });

    let request = build_request(&cli).expect("runtime takeover request should build");
    assert_eq!(request.command, "runtime");
    assert_eq!(request.args["sub"], "takeover");
}

#[test]
fn runtime_trigger_subcommand_projects_canonical_runtime_surface() {
    let cli = cli_with(Commands::Runtime {
        subcommand: Some(RuntimeSubcommand::Trigger),
    });

    let request = build_request(&cli).expect("runtime trigger request should build");
    assert_eq!(request.command, "runtime");
    assert_eq!(request.args["sub"], "trigger");
}

#[test]
fn runtime_orchestration_subcommand_projects_canonical_runtime_surface() {
    let cli = cli_with(Commands::Runtime {
        subcommand: Some(RuntimeSubcommand::Orchestration),
    });

    let request = build_request(&cli).expect("runtime orchestration request should build");
    assert_eq!(request.command, "runtime");
    assert_eq!(request.args["sub"], "orchestration");
}

#[test]
fn trigger_add_request_loads_spec_file_and_records_source() {
    let path = std::env::temp_dir().join(format!("rub-trigger-spec-{}.json", uuid::Uuid::now_v7()));
    fs::write(
        &path,
        r##"{
          "source_tab": 0,
          "target_tab": 1,
          "condition": { "kind": "text_present", "text": "Ready" },
          "action": { "kind": "browser_command", "command": "click", "payload": { "selector": "#go" } }
        }"##,
    )
    .expect("trigger spec file should be written");

    let cli = cli_with(Commands::Trigger {
        subcommand: TriggerSubcommand::Add {
            file: path.display().to_string(),
            paused: true,
        },
    });

    let request = build_request(&cli).expect("trigger add request should build");
    assert_eq!(request.command, "trigger");
    assert!(request.command_id.is_some());
    assert_eq!(request.args["sub"], "add");
    assert_eq!(request.args["paused"], true);
    assert_eq!(
        request.args["spec"],
        serde_json::json!({
          "source_tab": 0,
          "target_tab": 1,
          "condition": { "kind": "text_present", "text": "Ready" },
          "action": { "kind": "browser_command", "command": "click", "payload": { "selector": "#go" } }
        })
    );
    assert_eq!(request.args["spec_source"]["kind"], "file");
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.trigger.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_kind"],
        "trigger_registration_file"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn trigger_list_request_projects_registry_subcommand() {
    let cli = cli_with(Commands::Trigger {
        subcommand: TriggerSubcommand::List,
    });

    let request = build_request(&cli).expect("trigger list request should build");
    assert_eq!(request.command, "trigger");
    assert!(request.command_id.is_none());
    assert_eq!(request.args["sub"], "list");
}

#[test]
fn close_request_is_mutating_and_gets_command_id() {
    let cli = cli_with(Commands::Close { all: false });
    let request = build_request(&cli).expect("close request should build");
    assert_eq!(request.command, "close");
    assert!(request.command_id.is_some());
}

#[test]
fn trigger_trace_request_projects_dedicated_trace_surface() {
    let cli = cli_with(Commands::Trigger {
        subcommand: TriggerSubcommand::Trace { last: 7 },
    });

    let request = build_request(&cli).expect("trigger trace request should build");
    assert_eq!(request.command, "trigger");
    assert!(request.command_id.is_none());
    assert_eq!(request.args["sub"], "trace");
    assert_eq!(request.args["last"], 7);
}

#[test]
fn orchestration_add_request_loads_spec_file_and_records_source() {
    let path = std::env::temp_dir().join(format!(
        "rub-orchestration-spec-{}.json",
        uuid::Uuid::now_v7()
    ));
    fs::write(
        &path,
        r##"{
          "source": { "session_id": "sess-a" },
          "target": { "session_id": "sess-b" },
          "condition": { "kind": "text_present", "text": "Ready" },
          "actions": [
            { "kind": "workflow", "payload": { "workflow_name": "reply_flow" } }
          ]
        }"##,
    )
    .expect("orchestration spec file should be written");

    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Add {
            file: Some(path.display().to_string()),
            asset: None,
            paused: true,
        },
    });

    let request = build_request(&cli).expect("orchestration add request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "add");
    assert_eq!(request.args["paused"], true);
    assert_eq!(
        request.args["spec"],
        serde_json::json!({
          "source": { "session_id": "sess-a" },
          "target": { "session_id": "sess-b" },
          "condition": { "kind": "text_present", "text": "Ready" },
          "actions": [
            { "kind": "workflow", "payload": { "workflow_name": "reply_flow" } }
          ]
        })
    );
    assert_eq!(
        request.args["spec_source"]["path"],
        serde_json::json!(path.display().to_string())
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.orchestration.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_kind"],
        "orchestration_registration_file"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn orchestration_add_request_loads_named_asset_and_records_source() {
    let rub_home = std::env::temp_dir().join(format!(
        "rub-orchestration-asset-home-{}",
        uuid::Uuid::now_v7()
    ));
    let asset_path =
        rub_daemon::orchestration_assets::resolve_named_orchestration_path(&rub_home, "reply_rule")
            .unwrap();
    fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
    fs::write(
        &asset_path,
        r##"{
          "source": { "session_id": "sess-a" },
          "target": { "session_id": "sess-b" },
          "condition": { "kind": "text_present", "text": "Ready" },
          "actions": [
            { "kind": "browser_command", "command": "reload" }
          ]
        }"##,
    )
    .unwrap();

    let cli = cli_with_with_home(
        Commands::Orchestration {
            subcommand: OrchestrationSubcommand::Add {
                file: None,
                asset: Some("reply_rule".to_string()),
                paused: false,
            },
        },
        rub_home.clone(),
    );

    let request = build_request(&cli).expect("orchestration add asset request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "add");
    assert_eq!(
        request.args["spec"],
        serde_json::json!({
          "source": { "session_id": "sess-a" },
          "target": { "session_id": "sess-b" },
          "condition": { "kind": "text_present", "text": "Ready" },
          "actions": [
            { "kind": "browser_command", "command": "reload" }
          ]
        })
    );
    assert_eq!(request.args["spec_source"]["kind"], "asset");
    assert_eq!(request.args["spec_source"]["name"], "reply_rule");
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_authority"],
        "cli.orchestration.spec_source.path"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["upstream_truth"],
        "cli_orchestration_asset_option"
    );
    assert_eq!(
        request.args["spec_source"]["path_state"]["path_kind"],
        "orchestration_asset_reference"
    );

    let _ = fs::remove_dir_all(rub_home);
}

#[test]
fn orchestration_add_missing_asset_reports_path_context() {
    let rub_home = std::env::temp_dir().join(format!(
        "rub-orchestration-missing-asset-home-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = fs::remove_dir_all(&rub_home);

    let cli = cli_with_with_home(
        Commands::Orchestration {
            subcommand: OrchestrationSubcommand::Add {
                file: None,
                asset: Some("reply_rule".to_string()),
                paused: false,
            },
        },
        rub_home,
    );

    let error = build_request(&cli)
        .expect_err("missing orchestration asset should fail")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::FileNotFound);
    let context = error.context.expect("orchestration asset path context");
    assert_eq!(context["reason"], "named_orchestration_asset_not_found");
    assert_eq!(
        context["path_state"]["path_authority"],
        "cli.orchestration.spec_source.path"
    );
    assert_eq!(
        context["path_state"]["path_kind"],
        "orchestration_asset_reference"
    );
}

#[test]
fn trigger_add_missing_file_reports_path_context() {
    let cli = cli_with(Commands::Trigger {
        subcommand: TriggerSubcommand::Add {
            file: "./missing-trigger.json".to_string(),
            paused: false,
        },
    });

    let error = build_request(&cli)
        .expect_err("missing trigger file should fail")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::FileNotFound);
    let context = error.context.expect("trigger add path context");
    assert_eq!(context["reason"], "trigger_spec_file_not_found");
    assert_eq!(
        context["path_state"]["path_authority"],
        "cli.trigger.spec_source.path"
    );
}

#[test]
fn orchestration_add_missing_file_reports_path_context() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Add {
            file: Some("./missing-orchestration.json".to_string()),
            asset: None,
            paused: false,
        },
    });

    let error = build_request(&cli)
        .expect_err("missing orchestration file should fail")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::FileNotFound);
    let context = error.context.expect("orchestration add path context");
    assert_eq!(context["reason"], "orchestration_spec_file_not_found");
    assert_eq!(
        context["path_state"]["path_authority"],
        "cli.orchestration.spec_source.path"
    );
}

#[test]
fn orchestration_list_request_projects_registry_subcommand() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::List,
    });

    let request = build_request(&cli).expect("orchestration list request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "list");
}

#[test]
fn orchestration_trace_request_projects_dedicated_trace_surface() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Trace { last: 9 },
    });

    let request = build_request(&cli).expect("orchestration trace request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "trace");
    assert_eq!(request.args["last"], 9);
}

#[test]
fn orchestration_list_assets_is_handled_locally() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::ListAssets,
    });

    let error = build_request(&cli).expect_err("list-assets should stay local");
    assert!(error.to_string().contains("handled locally"));
}

#[test]
fn orchestration_export_request_projects_export_surface() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Export {
            id: 7,
            save_as: Some("reply_rule".to_string()),
            output: Some("/tmp/reply_rule.json".to_string()),
        },
    });

    let request = build_request(&cli).expect("orchestration export request should build");
    assert_eq!(request.command, "orchestration");
    assert!(request.command_id.is_none());
    assert_eq!(request.args["sub"], "export");
    assert_eq!(request.args["id"], 7);
}

#[test]
fn orchestration_execute_request_projects_mutating_execution_surface() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Execute {
            id: Some(27),
            id_option: None,
        },
    });

    let request = build_request(&cli).expect("orchestration execute request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "execute");
    assert_eq!(request.args["id"], 27);
}

#[test]
fn orchestration_execute_id_flag_projects_same_execution_surface() {
    let cli = cli_with(Commands::Orchestration {
        subcommand: OrchestrationSubcommand::Execute {
            id: None,
            id_option: Some(41),
        },
    });

    let request = build_request(&cli).expect("orchestration execute request should build");
    assert_eq!(request.command, "orchestration");
    assert_eq!(request.args["sub"], "execute");
    assert_eq!(request.args["id"], 41);
}

#[test]
fn takeover_resume_builds_mutating_takeover_request() {
    let cli = cli_with(Commands::Takeover {
        subcommand: Some(TakeoverSubcommand::Resume),
    });

    let request = build_request(&cli).expect("takeover resume request should build");
    assert_eq!(request.command, "takeover");
    assert_eq!(request.args["sub"], "resume");
}

#[test]
fn takeover_elevate_builds_mutating_takeover_request() {
    let cli = cli_with(Commands::Takeover {
        subcommand: Some(TakeoverSubcommand::Elevate),
    });

    let request = build_request(&cli).expect("takeover elevate request should build");
    assert_eq!(request.command, "takeover");
    assert_eq!(request.args["sub"], "elevate");
}

#[test]
fn wait_after_budget_extends_command_timeout() {
    let cli = cli_with(Commands::Open {
        url: "https://example.com".to_string(),
        load_strategy: "load".to_string(),
        wait_after: WaitAfterArgs {
            selector: Some("#ready".to_string()),
            target_text: None,
            role: None,
            label: None,
            testid: None,
            text: None,
            description_contains: None,
            url_contains: None,
            title_contains: None,
            first: false,
            last: false,
            nth: None,
            timeout_ms: Some(7_500),
            state: Some("visible".to_string()),
        },
    });

    let request = build_request(&cli).expect("open request should build");
    assert_eq!(request.timeout_ms, 37_500);
    assert_eq!(request.args["wait_after"]["selector"], "#ready");
}

#[test]
fn wait_after_budget_uses_bounded_default_timeout() {
    let cli = cli_with(Commands::Open {
        url: "https://example.com".to_string(),
        load_strategy: "load".to_string(),
        wait_after: WaitAfterArgs {
            selector: Some("#ready".to_string()),
            ..WaitAfterArgs::default()
        },
    });

    let request = build_request(&cli).expect("open request should build");
    assert_eq!(request.timeout_ms, 30_000 + DEFAULT_WAIT_AFTER_TIMEOUT_MS);
    assert_eq!(request.args["wait_after"]["selector"], "#ready");
    assert!(request.args["wait_after"].get("timeout_ms").is_none());
}

#[test]
fn default_wait_after_does_not_extend_timeout_when_payload_omits_wait_after() {
    let cli = cli_with(Commands::Click {
        index: None,
        target: ElementAddressArgs {
            selector: Some("#submit".to_string()),
            ..ElementAddressArgs::default()
        },
        xy: None,
        double: false,
        right: false,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("click request should build");
    assert_eq!(request.timeout_ms, cli.timeout);
    assert!(request.args.get("wait_after").is_none());
}

#[test]
fn state_request_serializes_semantic_observation_scope() {
    let cli = cli_with(Commands::State {
        limit: Some(25),
        format: Some(StateFormatArg::A11y),
        format_alias: None,
        a11y: false,
        viewport: false,
        diff: None,
        listeners: false,
        scope: ObservationScopeArgs {
            role: Some("main".to_string()),
            nth: Some(1),
            ..ObservationScopeArgs::default()
        },
        projection: ObservationProjectionArgs::default(),
    });

    let request = build_request(&cli).expect("state request should build");
    assert_eq!(request.args["scope"]["kind"], "role");
    assert_eq!(request.args["scope"]["role"], "main");
    assert_eq!(request.args["scope"]["selection"]["nth"], 1);
    assert!(request.args.get("selector").is_none());
}

#[test]
fn state_request_uses_positional_format_alias_when_explicit_flag_is_absent() {
    let cli = cli_with(Commands::State {
        limit: None,
        format: None,
        format_alias: Some(StateFormatArg::Compact),
        a11y: false,
        viewport: false,
        diff: None,
        listeners: false,
        scope: ObservationScopeArgs::default(),
        projection: ObservationProjectionArgs::default(),
    });

    let request = build_request(&cli).expect("state request should build with positional alias");
    assert_eq!(request.args["format"], "compact");
}

#[test]
fn inspect_page_request_reuses_state_projection_args() {
    let cli = cli_with(Commands::Inspect(
        crate::commands::InspectSubcommand::Page {
            limit: Some(10),
            format: Some(StateFormatArg::Compact),
            a11y: false,
            viewport: true,
            listeners: true,
            scope: ObservationScopeArgs {
                selector: Some("#content".to_string()),
                ..ObservationScopeArgs::default()
            },
            projection: ObservationProjectionArgs {
                depth: Some(2),
                ..ObservationProjectionArgs::default()
            },
        },
    ));

    let request = build_request(&cli).expect("inspect page request should build");
    assert_eq!(request.command, "inspect");
    assert_eq!(request.args["sub"], "page");
    assert_eq!(request.args["scope"]["kind"], "selector");
    assert_eq!(request.args["scope"]["css"], "#content");
    assert_eq!(request.args["format"], "compact");
    assert_eq!(request.args["depth"], 2);
    assert_eq!(request.args["viewport"], true);
    assert_eq!(request.args["listeners"], true);
}

#[test]
fn observe_request_serializes_compact_projection_policy() {
    let cli = cli_with(Commands::Observe {
        path: None,
        full: false,
        limit: Some(12),
        scope: ObservationScopeArgs {
            testid: Some("primary-content".to_string()),
            ..ObservationScopeArgs::default()
        },
        projection: ObservationProjectionArgs {
            compact: true,
            depth: Some(1),
        },
    });

    let request = build_request(&cli).expect("observe request should build");
    assert_eq!(request.args["scope"]["kind"], "test_id");
    assert_eq!(request.args["scope"]["testid"], "primary-content");
    assert_eq!(request.args["compact"], true);
    assert_eq!(request.args["depth"], 1);
}

#[test]
fn state_request_rejects_multiple_observation_scope_kinds() {
    let cli = cli_with(Commands::State {
        limit: None,
        format: None,
        format_alias: None,
        a11y: false,
        viewport: false,
        diff: None,
        listeners: false,
        scope: ObservationScopeArgs {
            selector: Some("#content".to_string()),
            role: Some("main".to_string()),
            ..ObservationScopeArgs::default()
        },
        projection: ObservationProjectionArgs::default(),
    });

    let error = build_request(&cli)
        .expect_err("multiple observation scope kinds should be rejected")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("--scope-selector"), "{error}");
}

#[test]
fn state_request_rejects_multiple_observation_scope_selections() {
    let cli = cli_with(Commands::State {
        limit: None,
        format: None,
        format_alias: None,
        a11y: false,
        viewport: false,
        diff: None,
        listeners: false,
        scope: ObservationScopeArgs {
            selector: Some("#content".to_string()),
            first: true,
            nth: Some(2),
            ..ObservationScopeArgs::default()
        },
        projection: ObservationProjectionArgs::default(),
    });

    let error = build_request(&cli)
        .expect_err("multiple observation scope selections should be rejected")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("--scope-first"), "{error}");
}

#[test]
fn type_request_rejects_ambiguous_locator_before_bootstrap() {
    let cli = cli_with(Commands::Type {
        index: None,
        target: ElementAddressArgs {
            selector: Some("#submit".to_string()),
            role: Some("button".to_string()),
            ..ElementAddressArgs::default()
        },
        clear: false,
        text_flag: None,
        text: Some("hello".to_string()),
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli)
        .expect_err("ambiguous locator should be rejected before bootstrap")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("Target is ambiguous"), "{error}");
}

#[test]
fn type_request_allows_focused_text_target_without_locator_before_bootstrap() {
    let cli = cli_with(Commands::Type {
        index: None,
        target: ElementAddressArgs::default(),
        clear: false,
        text_flag: None,
        text: Some("hello".to_string()),
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("focused target typing should build");
    assert_eq!(request.command, "type");
    assert_eq!(request.args["text"], "hello");
    assert_eq!(request.args["clear"], false);
    assert!(request.args["index"].is_null());
    assert!(request.args["selector"].is_null());
    assert!(request.args["target_text"].is_null());
}

#[test]
fn type_request_accepts_text_flag_alias_before_bootstrap() {
    let cli = cli_with(Commands::Type {
        index: None,
        target: ElementAddressArgs {
            label: Some("Email".to_string()),
            ..ElementAddressArgs::default()
        },
        clear: false,
        text_flag: Some("hello@example.com".to_string()),
        text: None,
        wait_after: WaitAfterArgs::default(),
    });

    let request = build_request(&cli).expect("type request should build from --text alias");
    assert_eq!(request.command, "type");
    assert_eq!(request.args["label"], "Email");
    assert_eq!(request.args["text"], "hello@example.com");
}

#[test]
fn type_request_rejects_ambiguous_text_sources_before_bootstrap() {
    let cli = cli_with(Commands::Type {
        index: None,
        target: ElementAddressArgs {
            label: Some("Email".to_string()),
            ..ElementAddressArgs::default()
        },
        clear: false,
        text_flag: Some("hello@example.com".to_string()),
        text: Some("duplicate".to_string()),
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli)
        .expect_err("type request should reject both positional text and --text")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(
        error.message.contains("either positional TEXT or `--text`"),
        "{error}"
    );
}

#[test]
fn hover_request_rejects_match_selection_with_index_before_bootstrap() {
    let cli = cli_with(Commands::Hover {
        index: Some(0),
        target: ElementAddressArgs {
            first: true,
            ..ElementAddressArgs::default()
        },
        wait_after: WaitAfterArgs::default(),
    });

    let error = build_request(&cli)
        .expect_err("index plus selection should be rejected before bootstrap")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("index addressing"), "{error}");
}

#[test]
fn wait_request_rejects_text_and_locator_before_bootstrap() {
    let cli = cli_with(Commands::Wait {
        selector: Some("#content".to_string()),
        target_text: None,
        role: None,
        label: None,
        testid: None,
        text: Some("Loaded".to_string()),
        description_contains: None,
        url_contains: None,
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 5_000,
        state: "visible".to_string(),
    });

    let error = build_request(&cli)
        .expect_err("conflicting wait probes should be rejected before bootstrap")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("Wait probe is ambiguous"), "{error}");
}

#[test]
fn wait_request_projects_url_contains_probe() {
    let cli = cli_with(Commands::Wait {
        selector: None,
        target_text: None,
        role: None,
        label: None,
        testid: None,
        text: None,
        description_contains: None,
        url_contains: Some("/activate".to_string()),
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 7_500,
        state: "visible".to_string(),
    });

    let request = build_request(&cli).expect("url wait should build");
    assert_eq!(request.command, "wait");
    assert_eq!(request.args["url_contains"], "/activate");
    assert_eq!(request.args["timeout_ms"], 7_500);
}

#[test]
fn wait_request_projects_interactable_state() {
    let cli = cli_with(Commands::Wait {
        selector: Some("#composer".to_string()),
        target_text: None,
        role: None,
        label: None,
        testid: None,
        text: None,
        description_contains: None,
        url_contains: None,
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 4_000,
        state: "interactable".to_string(),
    });

    let request = build_request(&cli).expect("interactable wait should build");
    assert_eq!(request.command, "wait");
    assert_eq!(request.args["selector"], "#composer");
    assert_eq!(request.args["state"], "interactable");
}

#[test]
fn wait_request_projects_description_contains_probe() {
    let cli = cli_with(Commands::Wait {
        selector: None,
        target_text: None,
        role: None,
        label: Some("Email".to_string()),
        testid: None,
        text: None,
        description_contains: Some("We will email you to confirm".to_string()),
        url_contains: None,
        title_contains: None,
        first: false,
        last: false,
        nth: None,
        timeout: 4_000,
        state: "visible".to_string(),
    });

    let request = build_request(&cli).expect("description wait should build");
    assert_eq!(request.command, "wait");
    assert_eq!(request.args["label"], "Email");
    assert_eq!(
        request.args["description_contains"],
        "We will email you to confirm"
    );
}

#[test]
fn click_request_rejects_ambiguous_wait_after_before_bootstrap() {
    let cli = cli_with(Commands::Click {
        index: Some(0),
        target: ElementAddressArgs::default(),
        xy: None,
        double: false,
        right: false,
        wait_after: WaitAfterArgs {
            selector: Some("#ready".to_string()),
            text: Some("Ready".to_string()),
            ..WaitAfterArgs::default()
        },
    });

    let error = build_request(&cli)
        .expect_err("ambiguous wait-after should be rejected before bootstrap")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("Wait probe is ambiguous"), "{error}");
}

#[test]
fn wait_after_rejects_selection_for_page_level_wait_probe() {
    let cli = cli_with(Commands::Click {
        index: Some(0),
        target: ElementAddressArgs::default(),
        xy: None,
        double: false,
        right: false,
        wait_after: WaitAfterArgs {
            title_contains: Some("Confirm".to_string()),
            first: true,
            ..WaitAfterArgs::default()
        },
    });

    let error = build_request(&cli)
        .expect_err("page-level wait-after plus selection should be rejected")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("page-level waits"), "{error}");
}

#[test]
fn click_request_projects_description_wait_after_probe() {
    let cli = cli_with(Commands::Click {
        index: Some(0),
        target: ElementAddressArgs::default(),
        xy: None,
        double: false,
        right: false,
        wait_after: WaitAfterArgs {
            label: Some("Email".to_string()),
            description_contains: Some("We will email you to confirm".to_string()),
            ..WaitAfterArgs::default()
        },
    });

    let request = build_request(&cli).expect("description wait-after should build");
    assert_eq!(request.args["wait_after"]["label"], "Email");
    assert_eq!(
        request.args["wait_after"]["description_contains"],
        "We will email you to confirm"
    );
}
