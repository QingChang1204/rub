mod budget;
pub(crate) mod helpers;
mod subcommands;

use crate::commands::{Commands, EffectiveCli, ElementAddressArgs};
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::protocol::IpcRequest;
use serde_json::Value;

pub(crate) use self::budget::WAIT_IPC_BUFFER_MS;
use self::budget::{command_timeout_ms, humanize_budget_ms_for_command_args};
use self::helpers::{
    WaitProbeArgs, element_address_args, input_path_reference_state, merge_json_objects,
    mutating_request, observation_projection_args, observation_scope_args,
    optional_element_address_args, parse_indexed_operand, resolve_cli_path,
    resolve_inspect_list_spec_source, resolve_json_spec_source, resolve_pipe_spec,
    wait_command_args, with_wait_after,
};
use self::subcommands::{
    build_cookies_request, build_dialog_request, build_download_request, build_get_request,
    build_handoff_request, build_history_request, build_inspect_request, build_intercept_request,
    build_interference_request, build_orchestration_request, build_runtime_request,
    build_storage_request, build_takeover_request, build_trigger_request,
};

fn local_only_command_projection_error(command: &Commands) -> RubError {
    let surface = command
        .local_projection_surface()
        .unwrap_or_else(|| command.canonical_name());
    RubError::domain(
        ErrorCode::InternalError,
        format!("{surface} must be handled locally before IPC request projection"),
    )
}

pub(crate) fn align_embedded_timeout_authority(request: &mut IpcRequest) {
    let embedded_timeout_ms = match request.command.as_str() {
        "wait" => Some(request.timeout_ms.saturating_sub(WAIT_IPC_BUFFER_MS)),
        "inspect"
            if request
                .args
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "network")
                && request
                    .args
                    .get("wait")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false) =>
        {
            Some(request.timeout_ms.saturating_sub(WAIT_IPC_BUFFER_MS))
        }
        "download"
            if request
                .args
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "wait" || sub == "save") =>
        {
            Some(request.timeout_ms.saturating_sub(WAIT_IPC_BUFFER_MS))
        }
        _ => None,
    };

    if let Some(timeout_ms) = embedded_timeout_ms
        && let Some(object) = request.args.as_object_mut()
        && object.contains_key("timeout_ms")
    {
        object.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
    }
}

pub fn build_request(cli: &EffectiveCli) -> Result<IpcRequest, RubError> {
    let timeout = command_timeout_ms(cli);

    match &cli.command {
        Commands::Open {
            url,
            load_strategy,
            wait_after,
        } => Ok(mutating_request(
            "open",
            with_wait_after(
                serde_json::json!({
                    "url": url,
                    "load_strategy": load_strategy,
                }),
                wait_after,
            )?,
            timeout,
        )),
        Commands::State {
            limit,
            format,
            a11y,
            viewport,
            diff,
            listeners,
            scope,
            projection,
        } => Ok(IpcRequest::new(
            "state",
            merge_json_objects(
                serde_json::json!({
                    "limit": limit,
                    "format": format.map(|value| value.as_str()),
                    "a11y": a11y,
                    "viewport": viewport,
                    "diff": diff,
                    "listeners": listeners,
                }),
                merge_json_objects(
                    observation_scope_args(scope)?,
                    observation_projection_args(projection),
                ),
            ),
            timeout,
        )),
        Commands::Observe {
            path,
            full,
            limit,
            scope,
            projection,
        } => Ok(IpcRequest::new(
            "observe",
            merge_json_objects(
                serde_json::json!({
                    "path": path.as_deref().map(resolve_cli_path).map(|path| path.to_string_lossy().to_string()),
                    "path_state": path.as_deref().map(|_| {
                        input_path_reference_state(
                            "cli.observe.path",
                            "cli_observe_path_option",
                            "observe_output_file",
                        )
                    }),
                    "full": full,
                    "limit": limit,
                }),
                merge_json_objects(
                    observation_scope_args(scope)?,
                    observation_projection_args(projection),
                ),
            ),
            timeout,
        )),
        Commands::Find {
            target,
            content,
            limit,
        } => Ok(IpcRequest::new(
            "find",
            merge_json_objects(
                element_address_args(None, target)?,
                serde_json::json!({
                    "content": content,
                    "limit": limit
                }),
            ),
            timeout,
        )),
        Commands::Click {
            index,
            target,
            xy,
            double,
            right,
            wait_after,
        } => {
            validate_click_projection_inputs(*index, target, xy.as_deref())?;
            let gesture = if *double {
                Some("double")
            } else if *right {
                Some("right")
            } else {
                None
            };
            if let Some(coords) = xy {
                Ok(mutating_request(
                    "click",
                    with_wait_after(
                        merge_json_objects(
                            serde_json::json!({ "xy": coords }),
                            serde_json::json!({ "gesture": gesture }),
                        ),
                        wait_after,
                    )?,
                    timeout,
                ))
            } else {
                Ok(mutating_request(
                    "click",
                    with_wait_after(
                        merge_json_objects(
                            element_address_args(*index, target)?,
                            serde_json::json!({ "gesture": gesture }),
                        ),
                        wait_after,
                    )?,
                    timeout,
                ))
            }
        }
        Commands::Exec { code, raw } => Ok(mutating_request(
            "exec",
            serde_json::json!({
                "code": code,
                "raw": raw,
            }),
            timeout,
        )),
        Commands::Scroll {
            direction,
            amount,
            y,
        } => {
            validate_scroll_projection_inputs(direction, *y)?;
            let (direction, amount) = if let Some(delta_y) = y {
                let normalized_direction = if *delta_y < 0 { "up" } else { "down" };
                let normalized_amount = delta_y.unsigned_abs();
                (normalized_direction.to_string(), Some(normalized_amount))
            } else {
                (direction.clone(), *amount)
            };
            Ok(mutating_request(
                "scroll",
                serde_json::json!({
                    "direction": direction,
                    "amount": amount,
                }),
                timeout,
            ))
        }
        Commands::Back { wait_after } => Ok(mutating_request(
            "back",
            with_wait_after(serde_json::json!({}), wait_after)?,
            timeout,
        )),
        Commands::Forward { wait_after } => Ok(mutating_request(
            "forward",
            with_wait_after(serde_json::json!({}), wait_after)?,
            timeout,
        )),
        Commands::Reload {
            load_strategy,
            wait_after,
        } => Ok(mutating_request(
            "reload",
            with_wait_after(
                serde_json::json!({
                    "load_strategy": load_strategy,
                }),
                wait_after,
            )?,
            timeout,
        )),
        Commands::Screenshot {
            path,
            output_path,
            full,
            highlight,
        } => Ok(IpcRequest::new(
            "screenshot",
            serde_json::json!({
                "path": output_path
                    .as_deref()
                    .or(path.as_deref())
                    .map(resolve_cli_path)
                    .map(|path| path.to_string_lossy().to_string()),
                "path_state": output_path
                    .as_deref()
                    .or(path.as_deref())
                    .map(|_| {
                        input_path_reference_state(
                            "cli.screenshot.path",
                            "cli_screenshot_path_option",
                            "screenshot_output_file",
                        )
                    }),
                "full": full,
                "highlight": highlight,
            }),
            timeout,
        )),
        Commands::Close { all } => {
            if *all {
                Err(local_only_command_projection_error(&cli.command))
            } else {
                Ok(mutating_request("close", serde_json::json!({}), timeout))
            }
        }
        Commands::Cleanup => Err(local_only_command_projection_error(&cli.command)),
        Commands::Sessions => Err(local_only_command_projection_error(&cli.command)),
        Commands::History {
            last: _,
            from: _,
            to: _,
            export_pipe: _,
            export_script: _,
            include_observation: _,
            save_as: _,
            output: _,
        } => build_history_request(timeout, &cli.command),
        Commands::Downloads => Ok(IpcRequest::new("downloads", serde_json::json!({}), timeout)),
        Commands::Doctor => Ok(IpcRequest::new("doctor", serde_json::json!({}), timeout)),
        Commands::Frames => Ok(IpcRequest::new("frames", serde_json::json!({}), timeout)),
        Commands::Frame { index, name, top } => {
            let configured = index.is_some() as u8 + name.is_some() as u8 + (*top as u8);
            if configured != 1 {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "frame requires exactly one selector: <index>, --name, or --top",
                ));
            }
            Ok(mutating_request(
                "frame",
                serde_json::json!({
                    "index": index,
                    "name": name,
                    "top": top,
                }),
                timeout,
            ))
        }
        Commands::Runtime { subcommand } => build_runtime_request(timeout, subcommand),
        Commands::Trigger { subcommand } => build_trigger_request(timeout, subcommand),
        Commands::Orchestration { subcommand } => {
            build_orchestration_request(timeout, cli.rub_home.as_path(), subcommand)
        }
        Commands::Download { subcommand } => build_download_request(timeout, subcommand),
        Commands::Storage(subcommand) => build_storage_request(timeout, subcommand),
        Commands::Handoff { subcommand } => build_handoff_request(timeout, subcommand),
        Commands::Takeover { subcommand } => build_takeover_request(timeout, subcommand),
        Commands::Dialog { subcommand } => build_dialog_request(timeout, subcommand),
        Commands::Intercept { subcommand } => build_intercept_request(timeout, subcommand),
        Commands::Interference { subcommand } => build_interference_request(timeout, subcommand),
        Commands::Keys { keys, wait_after } => Ok(mutating_request(
            "keys",
            with_wait_after(serde_json::json!({ "keys": keys }), wait_after)?,
            timeout,
        )),
        Commands::Type {
            index,
            target,
            clear,
            text,
            wait_after,
        } => Ok(mutating_request(
            "type",
            with_wait_after(
                merge_json_objects(
                    optional_element_address_args(*index, target)?,
                    serde_json::json!({
                        "text": text,
                        "clear": clear,
                    }),
                ),
                wait_after,
            )?,
            timeout,
        )),
        Commands::Fill {
            spec,
            file,
            submit_index,
            submit_selector,
            submit_target_text,
            submit_ref,
            submit_role,
            submit_label,
            submit_testid,
            submit_first,
            submit_last,
            submit_nth,
            wait_after,
        } => {
            let (resolved_spec, spec_source) =
                resolve_json_spec_source("fill", spec.as_deref(), file.as_deref())?;
            let request_timeout = timeout.saturating_add(fill_workflow_budget_ms(
                &resolved_spec,
                cli.humanize,
                &cli.humanize_speed,
                submit_index.is_some()
                    || submit_selector.is_some()
                    || submit_target_text.is_some()
                    || submit_ref.is_some()
                    || submit_role.is_some()
                    || submit_label.is_some()
                    || submit_testid.is_some(),
            ));
            Ok(mutating_request(
                "fill",
                with_wait_after(
                    serde_json::json!({
                        "spec": resolved_spec,
                        "spec_source": spec_source,
                        "submit_index": submit_index,
                        "submit_selector": submit_selector,
                        "submit_target_text": submit_target_text,
                        "submit_ref": submit_ref,
                        "submit_role": submit_role,
                        "submit_label": submit_label,
                        "submit_testid": submit_testid,
                        "submit_first": submit_first,
                        "submit_last": submit_last,
                        "submit_nth": submit_nth,
                    }),
                    wait_after,
                )?,
                request_timeout,
            ))
        }
        Commands::Extract {
            spec,
            file,
            snapshot,
        } => {
            let (resolved_spec, spec_source) =
                resolve_json_spec_source("extract", spec.as_deref(), file.as_deref())?;
            Ok(IpcRequest::new(
                "extract",
                serde_json::json!({
                    "spec": resolved_spec,
                    "spec_source": spec_source,
                    "snapshot_id": snapshot,
                }),
                timeout,
            ))
        }
        Commands::Pipe {
            spec,
            file,
            workflow,
            list_workflows,
            vars,
            wait_after,
        } => {
            if *list_workflows {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "pipe --list-workflows is handled locally and should not build an IPC request",
                ));
            }
            let (resolved_spec, spec_source) = resolve_pipe_spec(
                spec.as_deref(),
                file.as_deref(),
                workflow.as_deref(),
                vars,
                &cli.rub_home,
            )?;
            let request_timeout = timeout.saturating_add(pipe_workflow_budget_ms(
                &resolved_spec,
                cli.humanize,
                &cli.humanize_speed,
            ));
            Ok(mutating_request(
                "pipe",
                with_wait_after(
                    serde_json::json!({
                        "spec": resolved_spec,
                        "spec_source": spec_source,
                    }),
                    wait_after,
                )?,
                request_timeout,
            ))
        }
        Commands::Wait {
            selector,
            target_text,
            role,
            label,
            testid,
            text,
            first,
            last,
            nth,
            timeout: wait_timeout,
            state,
        } => {
            let args = wait_command_args(
                WaitProbeArgs {
                    selector: selector.as_deref(),
                    target_text: target_text.as_deref(),
                    role: role.as_deref(),
                    label: label.as_deref(),
                    testid: testid.as_deref(),
                    text: text.as_deref(),
                    first: *first,
                    last: *last,
                    nth: *nth,
                },
                *wait_timeout,
                state,
            )?;
            Ok(IpcRequest::new(
                "wait",
                args,
                wait_timeout.saturating_add(WAIT_IPC_BUFFER_MS),
            ))
        }
        Commands::Tabs => Ok(IpcRequest::new("tabs", serde_json::json!({}), timeout)),
        Commands::Switch { index, wait_after } => Ok(mutating_request(
            "switch",
            with_wait_after(serde_json::json!({ "index": index }), wait_after)?,
            timeout,
        )),
        Commands::CloseTab { index } => Ok(mutating_request(
            "close-tab",
            serde_json::json!({ "index": index }),
            timeout,
        )),
        Commands::Get(sub) => build_get_request(timeout, sub),
        Commands::Inspect(sub) => build_inspect_request(timeout, sub),
        Commands::Hover {
            index,
            target,
            wait_after,
        } => Ok(mutating_request(
            "hover",
            with_wait_after(element_address_args(*index, target)?, wait_after)?,
            timeout,
        )),
        Commands::Cookies(sub) => build_cookies_request(timeout, sub),
        Commands::Upload {
            operands,
            target,
            wait_after,
        } => {
            let (index, path) = parse_indexed_operand(operands, "upload", "path")?;
            let abs = resolve_cli_path(&path);
            Ok(mutating_request(
                "upload",
                with_wait_after(
                    merge_json_objects(
                        element_address_args(index, target)?,
                        serde_json::json!({
                            "path": abs.to_string_lossy(),
                            "path_state": input_path_reference_state(
                                "cli.upload.path",
                                "cli_upload_operand",
                                "upload_input_file",
                            ),
                        }),
                    ),
                    wait_after,
                )?,
                timeout,
            ))
        }
        Commands::Select {
            operands,
            value,
            target,
            wait_after,
        } => {
            let (index, value) = if let Some(value) = value {
                if operands.is_empty() {
                    (None, value.clone())
                } else {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        "select accepts either positional operands or --value, but not both",
                    ));
                }
            } else {
                parse_indexed_operand(operands, "select", "value")?
            };
            Ok(mutating_request(
                "select",
                with_wait_after(
                    merge_json_objects(
                        element_address_args(index, target)?,
                        serde_json::json!({
                            "value": value,
                        }),
                    ),
                    wait_after,
                )?,
                timeout,
            ))
        }
        Commands::InternalDaemon => Err(local_only_command_projection_error(&cli.command)),
    }
}

fn validate_click_projection_inputs(
    index: Option<u32>,
    target: &ElementAddressArgs,
    xy: Option<&[f64]>,
) -> Result<(), RubError> {
    if xy.is_none() {
        return Ok(());
    }
    let has_locator_target = index.is_some()
        || target.snapshot.is_some()
        || target.element_ref.is_some()
        || target.selector.is_some()
        || target.target_text.is_some()
        || target.role.is_some()
        || target.label.is_some()
        || target.testid.is_some()
        || target.first
        || target.last
        || target.nth.is_some();
    if has_locator_target {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "`click --xy` cannot be combined with index, ref, selector, target-text, role, label, testid, snapshot, or match-selection targeting",
        ));
    }
    Ok(())
}

fn validate_scroll_projection_inputs(direction: &str, y: Option<i32>) -> Result<(), RubError> {
    if y.is_some() && !direction.eq_ignore_ascii_case("down") {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "`scroll --y` cannot be combined with an explicit direction argument",
        ));
    }
    Ok(())
}

fn fill_workflow_budget_ms(
    resolved_spec: &str,
    humanize: bool,
    humanize_speed: &str,
    has_submit: bool,
) -> u64 {
    let mut extra = rub_core::automation_timeout::fill_workflow_additional_timeout_ms(
        resolved_spec,
        has_submit,
    );
    let Some(steps) = serde_json::from_str::<Value>(resolved_spec)
        .ok()
        .and_then(|value| value.as_array().cloned())
    else {
        return humanize_budget_ms_for_command_args(
            "click",
            &serde_json::json!({}),
            humanize && has_submit,
            humanize_speed,
        );
    };

    for step in steps {
        if let Some(value) = step.get("value").and_then(Value::as_str) {
            extra = extra.saturating_add(humanize_budget_ms_for_command_args(
                "type",
                &serde_json::json!({ "text": value }),
                humanize,
                humanize_speed,
            ));
        } else if step
            .get("activate")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            extra = extra.saturating_add(humanize_budget_ms_for_command_args(
                "click",
                &serde_json::json!({}),
                humanize,
                humanize_speed,
            ));
        }
    }

    if has_submit {
        extra = extra.saturating_add(humanize_budget_ms_for_command_args(
            "click",
            &serde_json::json!({}),
            humanize,
            humanize_speed,
        ));
    }

    extra
}

fn pipe_workflow_budget_ms(resolved_spec: &str, humanize: bool, humanize_speed: &str) -> u64 {
    let mut extra =
        rub_core::automation_timeout::pipe_workflow_additional_timeout_ms(resolved_spec);
    let Some(workflow) = serde_json::from_str::<Value>(resolved_spec).ok() else {
        return 0;
    };
    let Some(steps) = workflow.get("steps").and_then(Value::as_array) else {
        return 0;
    };

    for step in steps {
        let command = step
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = step.get("args").unwrap_or(&Value::Null);

        extra = extra.saturating_add(humanize_budget_ms_for_command_args(
            command,
            args,
            humanize,
            humanize_speed,
        ));
        if command == "fill"
            && let Some(fill_spec) = args.get("spec").and_then(Value::as_str)
        {
            let has_submit = [
                "submit_index",
                "submit_selector",
                "submit_target_text",
                "submit_ref",
                "submit_role",
                "submit_label",
                "submit_testid",
            ]
            .into_iter()
            .any(|key| args.get(key).is_some_and(|value| !value.is_null()));
            extra = extra.saturating_add(fill_workflow_budget_ms(
                fill_spec,
                humanize,
                humanize_speed,
                has_submit,
            ));
            extra = extra.saturating_sub(
                rub_core::automation_timeout::fill_workflow_additional_timeout_ms(
                    fill_spec, has_submit,
                ),
            );
        }
    }

    extra
}

#[cfg(test)]
mod tests;
