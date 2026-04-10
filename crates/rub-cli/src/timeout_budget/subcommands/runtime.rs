use crate::commands::{
    Commands, DialogSubcommand, DownloadSubcommand, HandoffSubcommand, RuntimeSubcommand,
    StorageAreaArg, StorageSubcommand, TakeoverSubcommand,
};
use crate::timeout_budget::helpers::input_path_reference_state;
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::protocol::IpcRequest;

use super::super::{WAIT_IPC_BUFFER_MS, mutating_request, resolve_cli_path};

pub(crate) fn build_history_request(
    timeout: u64,
    command: &Commands,
) -> Result<IpcRequest, RubError> {
    let Commands::History {
        last,
        from,
        to,
        export_pipe,
        export_script,
        include_observation,
        save_as,
        output,
    } = command
    else {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "history request builder requires a history command",
        ));
    };
    if *include_observation && !(*export_pipe || *export_script) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "--include-observation requires --export-pipe or --export-script",
        ));
    }
    if (save_as.is_some() || output.is_some()) && !(*export_pipe || *export_script) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "--save-as/--output require --export-pipe or --export-script",
        ));
    }
    if save_as.is_some() && !*export_pipe {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "--save-as is only supported with --export-pipe",
        ));
    }
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "--from cannot be greater than --to",
        ));
    }
    Ok(IpcRequest::new(
        "history",
        serde_json::json!({
            "last": last,
            "from": from,
            "to": to,
            "export_pipe": export_pipe,
            "export_script": export_script,
            "include_observation": include_observation,
        }),
        timeout,
    ))
}

pub(crate) fn build_runtime_request(
    timeout: u64,
    subcommand: &Option<RuntimeSubcommand>,
) -> Result<IpcRequest, RubError> {
    let sub = match subcommand.as_ref().unwrap_or(&RuntimeSubcommand::Summary) {
        RuntimeSubcommand::Summary => "summary",
        RuntimeSubcommand::Dialog => "dialog",
        RuntimeSubcommand::Frame => "frame",
        RuntimeSubcommand::Integration => "integration",
        RuntimeSubcommand::Interference => "interference",
        RuntimeSubcommand::Observatory => "observatory",
        RuntimeSubcommand::StateInspector => "state-inspector",
        RuntimeSubcommand::Readiness => "readiness",
        RuntimeSubcommand::Handoff => "handoff",
        RuntimeSubcommand::Downloads => "downloads",
        RuntimeSubcommand::Storage => "storage",
        RuntimeSubcommand::Takeover => "takeover",
        RuntimeSubcommand::Orchestration => "orchestration",
        RuntimeSubcommand::Trigger => "trigger",
    };
    Ok(IpcRequest::new(
        "runtime",
        serde_json::json!({ "sub": sub }),
        timeout,
    ))
}

pub(crate) fn build_download_request(
    timeout: u64,
    subcommand: &DownloadSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        DownloadSubcommand::Wait { id, state } => Ok(IpcRequest::new(
            "download",
            serde_json::json!({
                "sub": "wait",
                "id": id,
                "state": state.as_str(),
                "timeout_ms": timeout,
            }),
            timeout.saturating_add(WAIT_IPC_BUFFER_MS),
        )),
        DownloadSubcommand::Cancel { id } => Ok(mutating_request(
            "download",
            serde_json::json!({
                "sub": "cancel",
                "id": id,
            }),
            timeout,
        )),
        DownloadSubcommand::Save {
            file,
            output_dir,
            input_field,
            url_field,
            name_field,
            base_url,
            cookie_url,
            limit,
            concurrency,
            overwrite,
        } => {
            let file = resolve_cli_path(file);
            let output_dir = resolve_cli_path(output_dir);
            Ok(mutating_request(
                "download",
                serde_json::json!({
                    "sub": "save",
                    "file": file.to_string_lossy(),
                    "file_state": input_path_reference_state(
                        "cli.download.save.file",
                        "cli_download_save_file_option",
                        "download_save_input_file",
                    ),
                    "output_dir": output_dir.to_string_lossy(),
                    "output_dir_state": input_path_reference_state(
                        "cli.download.save.output_dir",
                        "cli_download_save_output_dir_option",
                        "download_save_output_directory",
                    ),
                    "input_field": input_field,
                    "url_field": url_field,
                    "name_field": name_field,
                    "base_url": base_url,
                    "cookie_url": cookie_url,
                    "limit": limit,
                    "concurrency": concurrency,
                    "overwrite": overwrite,
                    "timeout_ms": timeout,
                }),
                timeout.saturating_add(WAIT_IPC_BUFFER_MS),
            ))
        }
    }
}

pub(crate) fn build_storage_request(
    timeout: u64,
    subcommand: &StorageSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        StorageSubcommand::Get { key, area } => Ok(IpcRequest::new(
            "storage",
            serde_json::json!({
                "sub": "get",
                "key": key,
                "area": area.map(|value| value.as_str()),
            }),
            timeout,
        )),
        StorageSubcommand::Set { key, value, area } => Ok(mutating_request(
            "storage",
            serde_json::json!({
                "sub": "set",
                "key": key,
                "value": value,
                "area": area.unwrap_or(StorageAreaArg::Local).as_str(),
            }),
            timeout,
        )),
        StorageSubcommand::Remove { key, area } => Ok(mutating_request(
            "storage",
            serde_json::json!({
                "sub": "remove",
                "key": key,
                "area": area.map(|value| value.as_str()),
            }),
            timeout,
        )),
        StorageSubcommand::Clear { area } => Ok(mutating_request(
            "storage",
            serde_json::json!({
                "sub": "clear",
                "area": area.map(|value| value.as_str()),
            }),
            timeout,
        )),
        StorageSubcommand::Export { path } => Ok(IpcRequest::new(
            "storage",
            serde_json::json!({
                "sub": "export",
                "path": path.as_deref().map(resolve_cli_path).map(|path| path.to_string_lossy().to_string()),
                "path_state": path.as_deref().map(|_| {
                    input_path_reference_state(
                        "cli.storage.export.path",
                        "cli_storage_export_option",
                        "storage_export_file",
                    )
                }),
            }),
            timeout,
        )),
        StorageSubcommand::Import { path } => Ok(mutating_request(
            "storage",
            serde_json::json!({
                "sub": "import",
                "path": resolve_cli_path(path).to_string_lossy(),
                "path_state": input_path_reference_state(
                    "cli.storage.import.path",
                    "cli_storage_import_option",
                    "storage_import_file",
                ),
            }),
            timeout,
        )),
    }
}

pub(crate) fn build_handoff_request(
    timeout: u64,
    subcommand: &Option<HandoffSubcommand>,
) -> Result<IpcRequest, RubError> {
    match subcommand.as_ref().unwrap_or(&HandoffSubcommand::Status) {
        HandoffSubcommand::Status => Ok(IpcRequest::new(
            "handoff",
            serde_json::json!({ "sub": "status" }),
            timeout,
        )),
        HandoffSubcommand::Start => Ok(mutating_request(
            "handoff",
            serde_json::json!({ "sub": "start" }),
            timeout,
        )),
        HandoffSubcommand::Complete => Ok(mutating_request(
            "handoff",
            serde_json::json!({ "sub": "complete" }),
            timeout,
        )),
    }
}

pub(crate) fn build_takeover_request(
    timeout: u64,
    subcommand: &Option<TakeoverSubcommand>,
) -> Result<IpcRequest, RubError> {
    match subcommand.as_ref().unwrap_or(&TakeoverSubcommand::Status) {
        TakeoverSubcommand::Status => Ok(IpcRequest::new(
            "takeover",
            serde_json::json!({ "sub": "status" }),
            timeout,
        )),
        TakeoverSubcommand::Start => Ok(mutating_request(
            "takeover",
            serde_json::json!({ "sub": "start" }),
            timeout,
        )),
        TakeoverSubcommand::Elevate => Ok(mutating_request(
            "takeover",
            serde_json::json!({ "sub": "elevate" }),
            timeout,
        )),
        TakeoverSubcommand::Resume => Ok(mutating_request(
            "takeover",
            serde_json::json!({ "sub": "resume" }),
            timeout,
        )),
    }
}

pub(crate) fn build_dialog_request(
    timeout: u64,
    subcommand: &Option<DialogSubcommand>,
) -> Result<IpcRequest, RubError> {
    match subcommand.as_ref().unwrap_or(&DialogSubcommand::Status) {
        DialogSubcommand::Status => Ok(IpcRequest::new(
            "dialog",
            serde_json::json!({ "sub": "status" }),
            timeout,
        )),
        DialogSubcommand::Accept { prompt_text } => Ok(mutating_request(
            "dialog",
            serde_json::json!({
                "sub": "accept",
                "prompt_text": prompt_text,
            }),
            timeout,
        )),
        DialogSubcommand::Dismiss => Ok(mutating_request(
            "dialog",
            serde_json::json!({ "sub": "dismiss" }),
            timeout,
        )),
    }
}
