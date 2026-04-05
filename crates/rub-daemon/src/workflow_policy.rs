#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCaptureClass {
    Workflow,
    Observation,
    Administrative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowStepPolicy {
    pub command: &'static str,
    pub capture_class: WorkflowCaptureClass,
    pub workflow_allowed: bool,
}

const POLICIES: &[WorkflowStepPolicy] = &[
    WorkflowStepPolicy {
        command: "open",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "state",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "observe",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "click",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "exec",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "scroll",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "back",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "keys",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "type",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "wait",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "tabs",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "switch",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "close-tab",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "get",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "hover",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "upload",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "select",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "fill",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "extract",
        capture_class: WorkflowCaptureClass::Workflow,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "screenshot",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: true,
    },
    WorkflowStepPolicy {
        command: "find",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "history",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "doctor",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "runtime",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "downloads",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "inspect",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "frames",
        capture_class: WorkflowCaptureClass::Observation,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "close",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "cleanup",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "sessions",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "handoff",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "dialog",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "intercept",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "interference",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "storage",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "download",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "frame",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "cookies",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
    WorkflowStepPolicy {
        command: "pipe",
        capture_class: WorkflowCaptureClass::Administrative,
        workflow_allowed: false,
    },
];

pub fn workflow_step_policy(command: &str) -> Option<&'static WorkflowStepPolicy> {
    POLICIES.iter().find(|policy| policy.command == command)
}

pub fn workflow_request_policy(command: &str, args: &serde_json::Value) -> WorkflowStepPolicy {
    if command == "orchestration" {
        return orchestration_request_policy(args);
    }

    workflow_step_policy(command)
        .copied()
        .unwrap_or(WorkflowStepPolicy {
            command: "",
            capture_class: WorkflowCaptureClass::Administrative,
            workflow_allowed: false,
        })
}

pub fn workflow_command_allowed(command: &str) -> bool {
    workflow_step_policy(command)
        .map(|policy| policy.workflow_allowed)
        .unwrap_or(false)
}

pub fn workflow_allowed_commands() -> Vec<&'static str> {
    POLICIES
        .iter()
        .filter(|policy| policy.workflow_allowed)
        .map(|policy| policy.command)
        .collect()
}

pub fn workflow_capture_class(command: &str) -> WorkflowCaptureClass {
    workflow_step_policy(command)
        .map(|policy| policy.capture_class)
        .unwrap_or(WorkflowCaptureClass::Workflow)
}

pub fn workflow_request_allowed(command: &str, args: &serde_json::Value) -> bool {
    workflow_request_policy(command, args).workflow_allowed
}

pub fn workflow_request_capture_class(
    command: &str,
    args: &serde_json::Value,
) -> WorkflowCaptureClass {
    workflow_request_policy(command, args).capture_class
}

pub fn workflow_allowed_step_descriptions() -> Vec<String> {
    let mut allowed = workflow_allowed_commands()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    allowed.push("orchestration:add|pause|resume|remove|execute".to_string());
    allowed
}

fn orchestration_request_policy(args: &serde_json::Value) -> WorkflowStepPolicy {
    match args
        .get("sub")
        .and_then(|value| value.as_str())
        .unwrap_or("list")
    {
        "add" | "pause" | "resume" | "remove" | "execute" => WorkflowStepPolicy {
            command: "orchestration",
            capture_class: WorkflowCaptureClass::Workflow,
            workflow_allowed: true,
        },
        "list" | "trace" => WorkflowStepPolicy {
            command: "orchestration",
            capture_class: WorkflowCaptureClass::Observation,
            workflow_allowed: false,
        },
        _ => WorkflowStepPolicy {
            command: "orchestration",
            capture_class: WorkflowCaptureClass::Administrative,
            workflow_allowed: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkflowCaptureClass, workflow_allowed_commands, workflow_allowed_step_descriptions,
        workflow_capture_class, workflow_command_allowed, workflow_request_allowed,
        workflow_request_capture_class, workflow_step_policy,
    };

    #[test]
    fn workflow_allowed_commands_match_policy_guard() {
        for command in workflow_allowed_commands() {
            assert!(workflow_command_allowed(command));
        }
        assert!(!workflow_command_allowed("pipe"));
        assert!(!workflow_command_allowed("history"));
    }

    #[test]
    fn observe_and_screenshot_are_admitted_as_bounded_observation_steps() {
        let observe = workflow_step_policy("observe").expect("observe policy");
        let screenshot = workflow_step_policy("screenshot").expect("screenshot policy");
        assert!(observe.workflow_allowed);
        assert!(screenshot.workflow_allowed);
        assert_eq!(observe.capture_class, WorkflowCaptureClass::Observation);
        assert_eq!(screenshot.capture_class, WorkflowCaptureClass::Observation);
    }

    #[test]
    fn capture_class_uses_same_registry_authority() {
        assert_eq!(
            workflow_capture_class("open"),
            WorkflowCaptureClass::Workflow
        );
        assert_eq!(
            workflow_capture_class("observe"),
            WorkflowCaptureClass::Observation
        );
        assert_eq!(
            workflow_capture_class("cleanup"),
            WorkflowCaptureClass::Administrative
        );
    }

    #[test]
    fn orchestration_request_policy_distinguishes_manage_and_observe_subcommands() {
        assert!(workflow_request_allowed(
            "orchestration",
            &serde_json::json!({ "sub": "add" })
        ));
        assert!(workflow_request_allowed(
            "orchestration",
            &serde_json::json!({ "sub": "execute" })
        ));
        assert!(!workflow_request_allowed(
            "orchestration",
            &serde_json::json!({ "sub": "list" })
        ));
        assert_eq!(
            workflow_request_capture_class("orchestration", &serde_json::json!({ "sub": "trace" })),
            WorkflowCaptureClass::Observation
        );
        assert_eq!(
            workflow_request_capture_class(
                "orchestration",
                &serde_json::json!({ "sub": "remove" })
            ),
            WorkflowCaptureClass::Workflow
        );
    }

    #[test]
    fn allowed_step_descriptions_include_orchestration_manage_surface() {
        let allowed = workflow_allowed_step_descriptions();
        assert!(
            allowed
                .iter()
                .any(|entry| entry == "orchestration:add|pause|resume|remove|execute")
        );
    }
}
