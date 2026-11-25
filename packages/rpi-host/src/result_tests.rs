use super::*;
use rpi_extcore::shared_host::protocol::*;

fn decode(details: Value, fallback: &Value) -> Value {
    let encoded = tool_result(details, fallback);
    assert!(encoded.len() <= MAX_TOOL_RESULT_BYTES);
    serde_json::from_str(&encoded).unwrap()
}

#[test]
fn envelope_preserves_success_and_all_business_failure_details() {
    for (status, error) in [
        ("reply_received", false),
        ("not_sent", true),
        ("query_failed", true),
        ("rejected", true),
        ("outcome_unknown", true),
        ("native_not_succeeded", true),
    ] {
        let details = json!({"error":error,"status":status,"data":{"kept":"文\\\""}});
        let report = decode(details.clone(), &control::failure("query_failed", "unused"));
        assert_eq!(report["$rpiToolResult"], 1);
        assert_eq!(report["isError"], error, "{status}");
        assert_eq!(report["result"]["details"], details);
        assert_eq!(report["result"]["terminate"], false);
        let text: Value =
            serde_json::from_str(report["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, details);
    }
}

#[test]
fn missing_or_malformed_local_error_flag_is_never_native_success() {
    for details in [json!({}), json!({"error":null}), json!({"error":"false"})] {
        assert_eq!(
            decode(details, &control::failure("query_failed", "unused"))["isError"],
            true
        );
    }
}

#[test]
fn oversized_query_reply_is_explicit_error_not_a_truncated_success() {
    let report = decode(
        json!({"error":false,"status":"reply_received","data":"x".repeat(MAX_TOOL_RESULT_BYTES / 2)}),
        &control::failure("query_failed", "worker panicked"),
    );
    assert_eq!(report["isError"], true);
    let details = &report["result"]["details"];
    assert_eq!(details["status"], "query_failed");
    assert!(details.get("data").is_none());
    assert!(details["message"].as_str().unwrap().contains("budget"));
    assert!(!details.to_string().contains("panicked"));
}

#[test]
fn oversized_mutation_reply_keeps_original_identity_and_full_reconciliation() {
    for command in [
        Command::Submit {
            agent_id: AgentId::new(),
            request_id: RequestId::new(),
            prompt: "private prompt must not be echoed".into(),
        },
        Command::AdministrativeClose {
            request: AdministrativeRequest {
                scope: ControlScopeId::new(),
                request_id: RequestId::new(),
                epoch: Epoch::new(),
                action: AdministrativeAction::Close {
                    agent_id: AgentId::new(),
                    revision: 7,
                },
            },
        },
    ] {
        let report = decode(
            json!({"error":false,"status":"reply_received","data":"x".repeat(MAX_TOOL_RESULT_BYTES / 2)}),
            &control::unknown(&command, "worker panicked"),
        );
        assert_eq!(report["isError"], true);
        let details = &report["result"]["details"];
        assert_eq!(details["status"], "outcome_unknown");
        assert_eq!(details["request_id"], json!(command.request_id()));
        assert_eq!(
            details["reconcile"]["command"],
            json!(control::reconciliation(&command))
        );
        assert!(!details.to_string().contains("private prompt"));
        assert!(!details.to_string().contains("panicked"));
    }
}

#[test]
fn actual_abi_local_failure_uses_structured_error_envelope() {
    let handle = HANDLES.insert(Drive::start(
        r#"{"command":{"op":"status"}}"#,
        &Err("operator configuration required"),
    ));
    let step = rpi_plugin_sdk::take_step_result!(poll(handle, None, std::ptr::null_mut()));
    destroy(handle);
    let rpi_plugin_sdk::callbacks::Step::Done(text) = step else {
        panic!("business error must retain the opt-in structured result")
    };
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["$rpiToolResult"], 1);
    assert_eq!(report["isError"], true);
    assert_eq!(report["result"]["details"]["status"], "not_sent");
}
