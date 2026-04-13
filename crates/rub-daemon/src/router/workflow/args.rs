use crate::router::request_args::{
    LocatorParseOptions, LocatorRequestArgs, canonical_locator_json, locator_json,
    parse_canonical_locator,
};
use rub_core::json_spec::NormalizedJsonSpec;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FillStepSpec {
    pub(super) index: Option<u32>,
    #[serde(rename = "ref")]
    pub(super) element_ref: Option<String>,
    pub(super) selector: Option<String>,
    pub(super) target_text: Option<String>,
    pub(super) role: Option<String>,
    pub(super) label: Option<String>,
    pub(super) testid: Option<String>,
    #[serde(default)]
    pub(super) first: bool,
    #[serde(default)]
    pub(super) last: bool,
    pub(super) nth: Option<u32>,
    pub(super) value: Option<String>,
    pub(super) activate: Option<bool>,
    pub(super) clear: Option<bool>,
    #[serde(default)]
    pub(super) wait_after: Option<StepWaitSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StepWaitSpec {
    pub(super) selector: Option<String>,
    pub(super) target_text: Option<String>,
    pub(super) role: Option<String>,
    pub(super) label: Option<String>,
    pub(super) testid: Option<String>,
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) first: bool,
    #[serde(default)]
    pub(super) last: bool,
    pub(super) nth: Option<u32>,
    pub(super) timeout_ms: Option<u64>,
    pub(super) state: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PipeStepSpec {
    pub(super) command: String,
    #[serde(default)]
    pub(super) args: serde_json::Value,
    pub(super) label: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PipeWorkflowAssetSpec {
    #[serde(default)]
    pub(super) steps: Vec<PipeStepSpec>,
    #[serde(default)]
    pub(super) orchestrations: Vec<PipeEmbeddedOrchestrationSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PipeEmbeddedOrchestrationSpec {
    #[serde(default)]
    pub(super) label: Option<String>,
    pub(super) spec: serde_json::Value,
}

#[derive(Debug)]
pub(super) struct ParsedPipeWorkflowSpec {
    pub(super) steps: Vec<PipeStepSpec>,
    pub(super) orchestrations: Vec<PipeEmbeddedOrchestrationSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FillArgs {
    pub(super) spec: NormalizedJsonSpec,
    #[serde(default, rename = "spec_source")]
    pub(super) _spec_source: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) atomic: bool,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
    #[serde(flatten)]
    pub(super) submit: SubmitLocatorArgs,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PipeArgs {
    pub(super) spec: NormalizedJsonSpec,
    #[serde(default)]
    pub(super) spec_source: Option<serde_json::Value>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(super) struct SubmitLocatorArgs {
    #[serde(rename = "submit_index")]
    pub(super) index: Option<u32>,
    #[serde(rename = "submit_ref")]
    pub(super) element_ref: Option<String>,
    #[serde(rename = "submit_selector")]
    pub(super) selector: Option<String>,
    #[serde(rename = "submit_target_text")]
    pub(super) target_text: Option<String>,
    #[serde(rename = "submit_role")]
    pub(super) role: Option<String>,
    #[serde(rename = "submit_label")]
    pub(super) label: Option<String>,
    #[serde(rename = "submit_testid")]
    pub(super) testid: Option<String>,
    #[serde(rename = "submit_first")]
    pub(super) first: bool,
    #[serde(rename = "submit_last")]
    pub(super) last: bool,
    #[serde(rename = "submit_nth")]
    pub(super) nth: Option<u32>,
}

impl SubmitLocatorArgs {
    fn locator_args(&self) -> LocatorRequestArgs {
        LocatorRequestArgs {
            index: self.index,
            element_ref: self.element_ref.clone(),
            selector: self.selector.clone(),
            target_text: self.target_text.clone(),
            role: self.role.clone(),
            label: self.label.clone(),
            testid: self.testid.clone(),
            visible: false,
            prefer_enabled: false,
            topmost: false,
            first: self.first,
            last: self.last,
            nth: self.nth,
        }
    }
}

pub(super) fn submit_args(args: &SubmitLocatorArgs) -> Option<serde_json::Value> {
    let locator = args.locator_args();
    if !locator.is_requested() {
        return None;
    }

    parse_canonical_locator(
        &locator_json(locator),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )
    .ok()
    .flatten()
    .map(|locator| canonical_locator_json(&locator))
}

#[cfg(test)]
mod tests {
    use super::{FillArgs, PipeArgs};

    #[test]
    fn fill_args_accept_string_and_structured_spec() {
        let parsed = serde_json::from_value::<FillArgs>(serde_json::json!({
            "spec": "[]"
        }))
        .expect("stringified fill spec should parse");
        assert_eq!(parsed.spec.as_value(), &serde_json::json!([]));

        let structured = serde_json::from_value::<FillArgs>(serde_json::json!({
            "spec": []
        }))
        .expect("structured fill spec should parse");
        assert_eq!(structured.spec.as_value(), &serde_json::json!([]));
    }

    #[test]
    fn pipe_args_accept_string_and_structured_spec() {
        let parsed = serde_json::from_value::<PipeArgs>(serde_json::json!({
            "spec": "{\"steps\":[]}"
        }))
        .expect("stringified pipe spec should parse");
        assert_eq!(parsed.spec.as_value(), &serde_json::json!({ "steps": [] }));

        let structured = serde_json::from_value::<PipeArgs>(serde_json::json!({
            "spec": { "steps": [] }
        }))
        .expect("structured pipe spec should parse");
        assert_eq!(
            structured.spec.as_value(),
            &serde_json::json!({ "steps": [] })
        );
    }
}
