mod automation;
mod query;
mod runtime;

use rub_core::error::RubError;
use rub_ipc::protocol::IpcRequest;

pub(crate) use automation::{
    build_intercept_request, build_interference_request, build_orchestration_request,
    build_trigger_request,
};
pub(crate) use query::{build_cookies_request, build_get_request, build_inspect_request};
pub(crate) use runtime::{
    build_dialog_request, build_download_request, build_handoff_request, build_history_request,
    build_runtime_request, build_storage_request, build_takeover_request,
};

fn checked_request(request: IpcRequest) -> Result<IpcRequest, RubError> {
    request.validate_contract().map_err(RubError::Domain)?;
    Ok(request)
}
