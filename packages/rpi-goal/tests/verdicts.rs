//! Completion verification, the narrow form: the plugin records verifier
//! verdicts (`_verify-result`), the seven statuses stay untouched, and the
//! existing continuation worker dispatches a rejected goal's next action.
//!
//! Acceptance: a goal that claims completion with work missing keeps going.
//!
//! The verdict edges are load-bearing because a rejection starts a paid model
//! run: a pass must never wake one, a newer queued action must survive an older
//! run's fold-back, a real `goal_wait` must be woken by the verdict it awaited,
//! and a late verdict must not undo an explicit user stop.
mod support;

use serde_json::json;
use std::time::Duration;

use rpi_plugin_sdk::EventTag;

const TIMEOUT: Duration = Duration::from_secs(3);

fn start_goal(title: &str) {
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command(title);
    support::wait_sends_mentioning(title, 1, TIMEOUT);
}

#[test]
fn a_rejected_completion_claim_keeps_the_goal_going() {
    let _guard = support::serial();
    start_goal("Objective with hidden requirements");

    // The model claims completion: the goal archives and the loop stops.
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::tool(
        "goal_complete",
        json!({"goal_id": id, "summary": "everything is done"}),
    )
    .expect("the completion claim must be accepted");
    assert!(support::active_goal().is_none(), "the goal archived");
    support::sleep(120);
    let sends_after_complete = support::calls_named("SendMessage").len();

    // The host's verifier rejects the claim. The narrow contract: record the
    // verdict, reopen the goal, queue the next action.
    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"tests were never run","nextAction":"Run the test suite and fix the failures"}}"#
    ));
    let text = verdict["text"].as_str().unwrap();
    assert!(text.contains("not satisfied"), "{text}");

    let goal = support::active_goal().expect("the goal must be reopened");
    assert_eq!(goal["status"], json!("active"), "{goal}");
    assert_ne!(goal["id"].as_str().unwrap(), id, "the id rotates on reopen");
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        goal["pendingNextAction"].as_str().unwrap(),
        "Run the test suite and fix the failures"
    );

    // The existing worker dispatches the verifier's next action — exactly once.
    let sends = support::wait_sends_mentioning("Run the test suite", 1, TIMEOUT);
    assert_eq!(sends.len(), 1, "the next action must dispatch exactly once");
    let prompt = sends[0]["text"].as_str().unwrap();
    assert!(prompt.contains("rejected"), "{prompt}");
    assert!(prompt.contains("tests were never run"), "{prompt}");
    assert!(prompt.contains("<next_action>"), "{prompt}");
    assert!(
        prompt.contains("Run the test suite and fix the failures"),
        "{prompt}"
    );
    assert!(prompt.contains("<goal_objective>"), "{prompt}");
    assert!(
        prompt.contains(goal["id"].as_str().unwrap()),
        "the dispatched prompt carries the fresh goal id"
    );

    // The verdict continuation replaced the generic continuation prompt: no
    // second send raced out with the plain "continue" text.
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Run the test suite").len(),
        1,
        "the queued next action must not dispatch twice"
    );
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_after_complete + 1,
        "no unrelated dispatch may follow"
    );
}

#[test]
fn a_pass_verdict_records_and_leaves_the_goal_archived() {
    let _guard = support::serial();
    start_goal("Pass verdict objective");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::tool(
        "goal_complete",
        json!({"goal_id": id, "summary": "done for real"}),
    )
    .expect("the completion claim must be accepted");
    support::sleep(120);
    let sends_after_complete = support::calls_named("SendMessage").len();

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"pass","reason":"all acceptance checks green"}}"#
    ));
    assert!(
        verdict["text"].as_str().unwrap().contains("pass"),
        "{verdict}"
    );
    assert!(
        support::active_goal().is_none(),
        "a pass verdict never resurrects the goal"
    );
    let archived = support::goal_state().unwrap()["archived"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(archived["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        archived["verifications"][0]["outcome"],
        json!("pass"),
        "{archived}"
    );
    support::sleep(150);
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_after_complete,
        "a pass verdict must not start a run"
    );
}

#[test]
fn a_verdict_on_a_live_run_waits_for_the_settled_boundary() {
    let _guard = support::serial();
    start_goal("Mid-run verdict objective");

    // Simulate a live user run on top of the goal.
    support::block_sends(true);
    support::fire(EventTag::AgentStart);
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Recheck the requirement list"}}"#
    ));
    let text = verdict["text"].as_str().unwrap();
    assert!(!text.contains("⚠️"), "{text}");

    support::sleep(120);
    assert!(
        support::sends_mentioning("Recheck the requirement list").is_empty(),
        "no continuation may be injected into a live run"
    );

    // The settled boundary dispatches the queued next action.
    support::fire(EventTag::AgentEnd);
    support::fire_settled();
    let sends = support::wait_sends_mentioning("Recheck the requirement list", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
    support::release_sends();
}

#[test]
fn a_rejection_on_a_stopped_goal_stays_queued_until_resume() {
    let _guard = support::serial();
    start_goal("Stopped verdict objective");
    support::command("pause");
    support::sleep(120);
    assert_eq!(support::status(), "paused");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Continue after the pause"}}"#
    ));
    support::sleep(120);
    let goal = support::active_goal().unwrap();
    assert_eq!(
        goal["status"],
        json!("paused"),
        "a paused goal stays paused"
    );
    assert_eq!(
        goal["pendingNextAction"].as_str().unwrap(),
        "Continue after the pause"
    );
    assert!(
        support::sends_mentioning("Continue after the pause").is_empty(),
        "nothing may run while the goal is stopped"
    );

    // Resume dispatches the queued next action (and rotates the id).
    support::command("resume");
    let sends = support::wait_sends_mentioning("Continue after the pause", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
}

#[test]
fn malformed_verdicts_fail_instead_of_silently_doing_nothing() {
    let _guard = support::serial();
    start_goal("Malformed verdict objective");
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    // A rejection without a next action would strand the reopened goal.
    let text = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied"}}"#
    ))["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("⚠️"), "{text}");
    assert!(text.contains("next_action"), "{text}");

    // Unknown outcome / bad JSON / stale goal_id are all explicit errors.
    for args in [
        r#"{"outcome":"maybe"}"#.to_string(),
        "not json".to_string(),
        r#"{"goalId":"deadbeefdeadbeef","outcome":"pass"}"#.to_string(),
    ] {
        let text = support::command(&format!("_verify-result {args}"))["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("⚠️"), "{args} -> {text}");
    }

    // Nothing was recorded and nothing was dispatched.
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 0);
    support::sleep(120);
    assert_eq!(
        support::sends_mentioning("Malformed verdict objective").len(),
        1
    );
}

#[test]
fn verdict_rows_are_bounded_in_persisted_state() {
    let _guard = support::serial();
    start_goal("Bounded verdicts objective");
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    // Keep the worker parked so the active goal's ordinary loop cannot race the
    // verdict recording; verdicts are recorded through the command thread.
    support::block_sends(true);
    // Drive 51 pass verdicts through the host face; the 51st evicts the oldest.
    for _ in 0..51 {
        support::command(&format!(
            r#"_verify-result {{"goalId":"{id}","outcome":"pass"}}"#
        ));
    }
    let goal = support::active_goal().unwrap();
    let rows = goal["verifications"].as_array().unwrap();
    assert_eq!(rows.len(), rpi_goal::state::MAX_VERIFICATIONS);
    support::release_sends();
}

#[test]
fn legacy_goal_tool_completion_can_be_rejected_and_reopened() {
    let _guard = support::serial();
    start_goal("Legacy tool completion review");
    support::settle_pause();
    let id = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    support::tool("goal", json!({"action": "complete"})).unwrap();
    assert!(support::active_goal().is_none());
    let state = support::goal_state().unwrap();
    let archived = state["archived"].as_array().unwrap().last().unwrap();
    assert_eq!(archived["terminatedBy"], json!("modelCompleted"));

    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Recheck the legacy completion claim"}}"#
    ));
    let sends = support::wait_sends_mentioning("Recheck the legacy completion claim", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"));
    assert_ne!(goal["id"], json!(id));
    assert!(goal["terminatedBy"].is_null());
}

#[test]
fn rejection_after_pausing_a_wait_keeps_the_goal_stopped() {
    let _guard = support::serial();
    start_goal("Paused wait review");
    support::settle_pause();
    let id = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    support::tool(
        "goal_wait",
        json!({"goal_id": id, "reason": "await review"}),
    )
    .unwrap();
    support::command("pause");
    let paused = support::active_goal().unwrap();
    assert!(
        paused["waiting"].is_null(),
        "explicit pause cancels the wait"
    );
    let before = support::calls_named("SendMessage").len();
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Review after explicit resume"}}"#
    ));
    support::settle_pause();
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"));
    assert!(goal["waiting"].is_null());
    assert!(goal["activeSince"].is_null());
    assert_eq!(
        goal["pendingNextAction"],
        json!("Review after explicit resume")
    );
    assert_eq!(support::calls_named("SendMessage").len(), before);

    support::command("resume");
    assert_eq!(
        support::wait_sends_mentioning("Review after explicit resume", 1, TIMEOUT).len(),
        1
    );
}

#[test]
fn pass_supersedes_a_queued_rejection_without_waking_a_paused_goal() {
    let _guard = support::serial();
    start_goal("Superseded rejection review");
    support::settle_pause();
    support::command("pause");
    let id = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"OBSOLETE_REVIEW_ACTION"}}"#
    ));
    support::settle_pause();
    let queued = support::active_goal().unwrap();
    assert_eq!(queued["pendingNextAction"], json!("OBSOLETE_REVIEW_ACTION"));
    let before = support::calls_named("SendMessage").len();
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"pass"}}"#
    ));
    support::settle_pause();
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"));
    assert!(goal["pendingNextAction"].is_null());
    assert!(
        goal["pendingNextActionSeq"].as_u64().unwrap()
            > queued["pendingNextActionSeq"].as_u64().unwrap()
    );
    assert_eq!(support::calls_named("SendMessage").len(), before);
    support::command("resume");
    support::settle_pause();
    assert!(support::sends_mentioning("OBSOLETE_REVIEW_ACTION").is_empty());
}

/// Reviewer reproduction 1: a pass on a *still active* goal must record only.
/// The old `finish()` on every outcome woke the worker, which sent a paid
/// generic continuation (SendMessage count 1 -> 2) without any new settle.
#[test]
fn an_active_goal_pass_records_without_starting_a_run() {
    let _guard = support::serial();
    start_goal("Active pass objective");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    // Wait out the kickoff run's fold-back so the baseline is stable.
    support::settle_pause();
    let sends_before = support::calls_named("SendMessage").len();
    assert_eq!(sends_before, 1, "only the kickoff ran before the verdict");

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"pass","reason":"external check green"}}"#
    ));
    assert!(
        verdict["text"].as_str().unwrap().contains("pass"),
        "{verdict}"
    );

    // No new AgentSettled, no wake: nothing may be dispatched.
    support::sleep(200);
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_before,
        "a pass on an active goal must not start a run"
    );
    let goal = support::active_goal().expect("the goal stays active");
    assert_eq!(goal["status"], json!("active"));
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(goal["verifications"][0]["outcome"], json!("pass"));
    assert!(
        goal["pendingNextAction"].is_null(),
        "a pass clears a queued rejection direction: {goal}"
    );
}

/// Reviewer reproduction 2: the fold-back of run A must consume only the
/// pending action version A actually served. A verdict B that arrives while A
/// is in flight survives A's `completed` return and is the one dispatched.
#[test]
fn a_newer_next_action_survives_an_older_runs_fold_back() {
    let _guard = support::serial();
    start_goal("Overlapping verdicts objective");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    // Park the worker inside the first verdict run, like a slow model turn.
    support::block_sends(true);
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"first pass","nextAction":"Fix the first finding"}}"#
    ));
    let first = support::wait_sends_mentioning("Fix the first finding", 1, TIMEOUT);
    assert_eq!(first.len(), 1, "the first verdict run must be in flight");

    // Still blocked in SendMessage: a newer verdict arrives and queues B.
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"second pass","nextAction":"Fix the second finding"}}"#
    ));
    let goal = support::active_goal().unwrap();
    assert_eq!(
        goal["pendingNextAction"].as_str().unwrap(),
        "Fix the second finding"
    );

    // Release the first run; its fold-back must not clear the newer action.
    // The wake B recorded while A was in flight then dispatches B itself.
    support::release_sends();
    let second = support::wait_sends_mentioning("Fix the second finding", 1, TIMEOUT);
    assert_eq!(
        second.len(),
        1,
        "the newer action must be the one dispatched"
    );
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Fix the first finding").len(),
        1,
        "the older action must not run again"
    );
    assert_eq!(
        support::sends_mentioning("Fix the second finding").len(),
        1,
        "the newer action must dispatch exactly once"
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 2);
    assert!(
        goal["pendingNextAction"].is_null(),
        "B's own fold-back consumes B"
    );
}

/// Reviewer reproduction 3: `goal_wait` keeps the goal `active` and stores
/// `waiting` (no deadline) — the rejection verdict is exactly the external
/// input it waited for, so it must clear the wait and dispatch the next action.
#[test]
fn a_verdict_wakes_a_real_goal_wait() {
    let _guard = support::serial();
    start_goal("Waiting for external input");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::tool(
        "goal_wait",
        json!({"goal_id": id, "reason": "waiting for the verifier"}),
    )
    .expect("the wait must be accepted");
    support::sleep(120);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"), "goal_wait keeps active");
    assert_eq!(goal["waiting"]["reason"], json!("waiting for the verifier"));
    assert!(goal["activeSince"].is_null(), "the wait stops the clock");
    let sends_before = support::calls_named("SendMessage").len();
    assert_eq!(
        sends_before, 1,
        "a dateless wait parks the goal: no continuation may run"
    );

    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"found gaps","nextAction":"Close the gaps the verifier found"}}"#
    ));
    let sends = support::wait_sends_mentioning("Close the gaps the verifier found", 1, TIMEOUT);
    assert_eq!(sends.len(), 1, "the verdict must wake the wait");
    let goal = support::active_goal().unwrap();
    assert!(
        goal["waiting"].is_null(),
        "the verdict is the external input the goal was waiting for: {goal}"
    );
    assert!(
        !goal["activeSince"].is_null(),
        "the active clock restarts after the wait is released"
    );
}

/// Reviewer reproduction 4: `/goal stop` is the user's decision. A late
/// verifier rejection for that goal id is recorded as history but must not
/// reopen it or start a run.
#[test]
fn a_late_verdict_does_not_reopen_a_user_stop() {
    let _guard = support::serial();
    start_goal("User-stopped objective");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::command("stop");
    support::sleep(120);
    assert!(
        support::active_goal().is_none(),
        "the stop archived the goal"
    );
    let sends_after_stop = support::calls_named("SendMessage").len();

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"late finding","nextAction":"Fix it after the stop"}}"#
    ));
    assert!(
        verdict["text"]
            .as_str()
            .unwrap()
            .contains("Verification recorded"),
        "{verdict}"
    );
    support::sleep(200);
    assert!(
        support::active_goal().is_none(),
        "a late verdict must not reopen a user-stopped goal"
    );
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_after_stop,
        "a late verdict on a user stop must not start a run"
    );

    // The verdict is kept as history on the archived row, with the original id.
    let archived = support::goal_state().unwrap()["archived"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(archived["id"].as_str().unwrap(), id);
    assert_eq!(archived["terminatedBy"], json!("userStop"));
    assert_eq!(archived["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        archived["verifications"][0]["outcome"],
        json!("notSatisfied")
    );

    // The transcript notice states the narrow contract.
    assert!(
        support::calls_named("AppendEntry")
            .iter()
            .any(|call| call.to_string().contains("not reopened")),
        "the notice must say the verdict was recorded but not reopened"
    );
}
