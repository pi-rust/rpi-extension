//! Continuation dispatcher.
//!
//! One worker per registration serializes that session’s automatic runs:
//!
//! * `AgentSettled` only *wakes* the worker. Sending from the dispatch thread
//!   would block the event fan-out, and `SendMessage` blocks for the whole run.
//! * The worker re-reads state after each dispatch, so a pause, replacement, or
//!   completion that happened during the run wins over the stale intent.
//! * `SendMessage {triggerTurn: true}` is the reservation primitive: an idle lane
//!   starts a run and blocks, a busy lane queues the message into the live run.
//!   rpi has no `isIdle()` equivalent, and it does not need one.
//! * State is only read and queued under [`state::lock`]. Host calls happen after
//!   the lock is released, so a re-entrant callback cannot deadlock.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rpi_plugin_sdk::RuntimeActionId;
use serde_json::json;

use crate::inject;
use crate::policy;
use crate::prompts;
use crate::runtime;
use crate::settings;
use crate::state::{self, Goal, Goals, Status};

/// Cancel the in-flight run so a stop takes effect at the current boundary.
pub fn request_abort() {
    if crate::is_subagent() || !crate::events::is_run_active() {
        return;
    }
    let Some(current) = runtime::current() else {
        return;
    };
    let session = current.session.clone();
    let binding = session.registration_binding().unwrap_or(current);
    let mut inner = session
        .worker
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if inner.shutdown {
        return;
    }
    let mut index = 0;
    while index < inner.aborts.len() {
        if inner.aborts[index].is_finished() {
            let _ = inner.aborts.swap_remove(index).join();
        } else {
            index += 1;
        }
    }
    let spawned = std::thread::Builder::new()
        .name("rpi-goal-abort".into())
        .spawn(move || {
            let _scope = runtime::enter(Some(binding));
            if let Err(error) = crate::call_runtime_raw(crate::raw_actions::ABORT, json!({})) {
                if !runtime::state().worker.abort_warned.swap(true, Ordering::SeqCst) {
                    eprintln!(
                        "rpi-goal: this host cannot abort the current run ({error}); a stop takes effect at the next turn boundary"
                    );
                }
            }
        });
    if let Ok(thread) = spawned {
        inner.aborts.push(thread);
    }
}

pub fn is_automatic() -> bool {
    runtime::state().worker.automatic.load(Ordering::SeqCst)
}

#[derive(Default)]
pub(crate) struct Worker {
    inner: Mutex<Inner>,
    ready: Condvar,
    automatic: AtomicBool,
    abort_warned: AtomicBool,
}

#[derive(Default)]
struct Inner {
    // Level-triggered wake counter. Repeated signals can be coalesced.
    seq: u64,
    shutdown: bool,
    thread: Option<JoinHandle<()>>,
    aborts: Vec<JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn is_stopping(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .shutdown
    }

    pub(crate) fn stop(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.shutdown = true;
        self.ready.notify_all();
    }

    pub(crate) fn finish(&self) {
        self.stop();
        let (thread, aborts) = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            (inner.thread.take(), std::mem::take(&mut inner.aborts))
        };
        for thread in thread.into_iter().chain(aborts) {
            let _ = thread.join();
        }
    }
}

/// Start this registration's worker on its first live use.
pub fn ensure_started() {
    if crate::is_subagent() || runtime::snapshot().is_none() {
        return;
    }
    let session = runtime::state();
    let mut inner = session
        .worker
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if inner.shutdown || inner.thread.is_some() {
        return;
    }
    let owned = session.clone();
    match std::thread::Builder::new()
        .name("rpi-goal-worker".into())
        .spawn(move || run(owned))
    {
        Ok(thread) => inner.thread = Some(thread),
        Err(error) => eprintln!("rpi-goal: unable to start the goal worker: {error}"),
    }
}

/// Ask this worker to re-evaluate a settle boundary or a goal mutation.
pub fn wake() {
    let session = runtime::state();
    let worker = &session.worker;
    let mut inner = worker.inner.lock().unwrap_or_else(|p| p.into_inner());
    inner.seq = inner.seq.wrapping_add(1);
    worker.ready.notify_all();
}

pub fn shutdown() {
    runtime::state().worker.stop();
}

fn run(session: Arc<runtime::Session>) {
    let _scope = runtime::enter(session.registration_binding());
    let worker = &session.worker;
    let mut seen = 0u64;
    let mut deadline: Option<Instant> = None;
    loop {
        let mut inner = worker.inner.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if inner.shutdown {
                return;
            }
            if inner.seq != seen {
                seen = inner.seq;
                break;
            }
            match deadline {
                None => {
                    inner = worker.ready.wait(inner).unwrap_or_else(|p| p.into_inner());
                }
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        break;
                    }
                    let (guard, timeout) = worker
                        .ready
                        .wait_timeout(inner, at.saturating_duration_since(now))
                        .unwrap_or_else(|p| p.into_inner());
                    inner = guard;
                    if timeout.timed_out() {
                        break;
                    }
                }
            }
        }
        drop(inner);
        deadline = step();
    }
}

enum Plan {
    Idle,
    Wait(Instant),
    /// The prompt to send. `Some((action, token))` overrides the standard
    /// continuation with the verifier-directed next action queued by
    /// `_verify-result`; the token lets fold-back clear only the action this run
    /// actually served.
    Send(Box<Goal>, u64, Option<(String, u64)>),
    Stop(Status, String, Option<&'static str>),
}

/// One evaluation pass. Returns the next absolute deadline the worker should
/// wake itself for (a `goal_wait` safety wake), if any.
fn step() -> Option<Instant> {
    if runtime::snapshot().is_none()
        || crate::is_subagent()
        || runtime::state().worker.is_stopping()
    {
        return None;
    }
    // Read host-backed state and settings before taking the mutation lock. A
    // mutation that lands while we are reading (or before the lock below) makes
    // this snapshot stale; the next wake will retry from fresh state.
    let before = state::revision();
    let goals = state::read_lenient();
    let after = state::revision();
    if before != after {
        return None;
    }
    let settings = settings::load();
    let tools_available = required_tools_available();
    let plan = {
        let session = crate::runtime::state();
        let _guard = state::lock(&session);
        if state::revision() != after {
            return None;
        }
        evaluate(goals, settings, tools_available)
    };
    match plan {
        Plan::Idle => None,
        Plan::Wait(at) => Some(at),
        Plan::Stop(status, reason, cause) => {
            stop(status, &reason, cause);
            None
        }
        Plan::Send(goal, revision, verdict_prompt) => {
            // A pause or replacement may have committed after `evaluate`
            // released the state lock. Check again before touching the host so
            // a stale plan cannot start a run for a stopped or replaced goal.
            if !dispatch_is_current(&goal, revision) {
                return None;
            }
            state::flush();
            inject::flush();
            if !dispatch_is_current(&goal, revision) {
                return None;
            }
            dispatch(&goal, revision, verdict_prompt);
            None
        }
    }
}

/// Decide what to do next from a snapshot read before the state lock. It only
/// mutates the in-memory snapshot and queues the write; host calls stay outside.
fn evaluate(goals: Goals, settings: settings::Settings, tools_available: bool) -> Plan {
    let Some(mut goal) = goals.active.clone() else {
        return Plan::Idle;
    };
    let now = state::now_ms();

    if !goal.status.is_active() {
        return Plan::Idle;
    }

    // Waiting: only an elapsed safety deadline wakes the goal. External input is
    // handled by `BeforeAgentStart`, which clears the wait.
    if let Some(waiting) = goal.waiting.clone() {
        match waiting.deadline_ms {
            Some(deadline) if now >= deadline => {}
            Some(deadline) => {
                let remaining = Duration::from_millis((deadline - now).max(0) as u64);
                return Plan::Wait(Instant::now() + remaining);
            }
            None => return Plan::Idle,
        }
    }

    if let Some(reason) = policy::exhausted_budget(&goal, now) {
        return Plan::Stop(Status::BudgetLimited, reason, None);
    }

    if let Some(reason) = policy::safety_pause(&goal, &settings) {
        let cause = if reason.starts_with("no progress") {
            "no_progress"
        } else {
            "continuation_limit"
        };
        return Plan::Stop(Status::Paused, reason, Some(cause));
    }

    if !tools_available {
        return Plan::Stop(
            Status::Paused,
            "goal_complete is not in the active tool set".into(),
            None,
        );
    }

    // Never inject into a live run: wait for its settled boundary.
    if crate::events::is_run_active() {
        return Plan::Idle;
    }

    goal.waiting = None;
    goal.iteration = goal.iteration.saturating_add(1);
    goal.updated_at = now;
    goal.last_outcome = None;
    if goal.active_since.is_none() {
        goal.start_clock(now);
    }
    // A queued verifier next action replaces the generic continuation prompt.
    // It stays in state (with its token) until the run's fold-back consumes
    // exactly that version, so a newer verdict is never dropped by an older run.
    let verdict_prompt = goal
        .pending_next_action
        .clone()
        .map(|action| (action, goal.pending_next_action_seq));
    let mut next = goals.clone();
    next.active = Some(goal.clone());
    let revision = state::queue(&next);
    Plan::Send(Box::new(goal), revision, verdict_prompt)
}

/// Whether a plan created by `evaluate` is still the current intent. The
/// revision check distinguishes a pause/replacement from the state snapshot
/// that produced the plan; the id/status check keeps a stale plan harmless when
/// a host read only exposes committed state.
fn dispatch_is_current(goal: &Goal, revision: u64) -> bool {
    let goals = state::read_lenient();
    goals
        .active
        .as_ref()
        .is_some_and(|active| active.id == goal.id && active.status == Status::Active)
        && goals.revision == revision
}

/// Send one continuation and fold the outcome back into state. The call blocks
/// until the run ends, so the settlement that follows is already queued.
fn dispatch(goal: &Goal, revision: u64, verdict_prompt: Option<(String, u64)>) {
    if !dispatch_is_current(goal, revision) {
        return;
    }
    let first = goal.iteration <= 1;
    let served_verdict = verdict_prompt.clone();
    let prompt = match verdict_prompt {
        Some((action, _token)) => {
            let verdict =
                goal.verifications
                    .last()
                    .cloned()
                    .unwrap_or(crate::state::GoalVerification {
                        outcome: crate::state::VerdictOutcome::NotSatisfied,
                        reason: None,
                        next_action: Some(action),
                        at_ms: state::now_ms(),
                    });
            prompts::verdict_continuation(goal, &verdict)
        }
        None if first => prompts::kickoff(goal),
        None => prompts::continuation(goal, goal.iteration),
    };
    let session = runtime::state();
    if session.worker.is_stopping() {
        return;
    }
    session.worker.automatic.store(true, Ordering::SeqCst);
    let result = crate::call_runtime(
        RuntimeActionId::SendMessage,
        json!({"text": prompt, "triggerTurn": true}),
    );
    session.worker.automatic.store(false, Ordering::SeqCst);
    if session.worker.is_stopping() {
        return;
    }

    // A stop (pause, budget, safety, usage limit) may have aborted this run while
    // it was in flight; that state is already committed and needs no fold-back.
    if !state::read_lenient()
        .active
        .as_ref()
        .map(|active| active.id == goal.id && active.status == Status::Active)
        .unwrap_or(false)
    {
        return;
    }

    match result {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match status.as_str() {
                "aborted" => stop_current(&goal.id, Status::Paused, "the run was aborted", None),
                "error" | "suspended" => {
                    let reason = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("the provider request failed");
                    wait_current(&goal.id, policy::provider_waiting(reason));
                }
                other => {
                    // Read-modify-write in one critical section: a stale clone
                    // carried across two sections could overwrite a newer state.
                    let queued = {
                        let session = crate::runtime::state();
                        let _guard = state::lock(&session);
                        let mut goals = state::read_lenient();
                        match goals.active.clone() {
                            Some(active) if active.id == goal.id => {
                                let mut active = active;
                                active.last_outcome = Some(other.to_string());
                                active.updated_at = state::now_ms();
                                // The verifier-directed run has been served; a
                                // completed/aborted/errored run keeps it queued
                                // so `/goal resume` re-runs it. Clear only the
                                // exact version this run served — a newer verdict
                                // that arrived during the run must survive.
                                if let Some((_, token)) = &served_verdict {
                                    active.clear_pending_next_action_if_served(*token);
                                }
                                goals.active = Some(active);
                                state::queue(&goals);
                                true
                            }
                            _ => false,
                        }
                    };
                    if queued {
                        state::flush();
                    }
                }
            }
        }
        Err(error) => {
            // The bridge itself failed. Do not spin: wait for the next signal.
            eprintln!("rpi-goal: continuation failed: {error}");
        }
    }
}

/// Whether `goal_complete` is reachable. A host without `GetActiveTools` (or a
/// plugin-visible failure) must not stall the goal.
fn required_tools_available() -> bool {
    let Ok(value) = crate::call_runtime(RuntimeActionId::GetActiveTools, json!({})) else {
        return true;
    };
    let Some(tools) = value.get("tools").and_then(|v| v.as_array()) else {
        return true;
    };
    tools
        .iter()
        .filter_map(|tool| tool.as_str())
        .any(|tool| tool == "goal_complete")
}

/// Stop the goal at the boundary the worker just observed. Re-reads state so a
/// concurrent replacement wins.
pub fn stop(status: Status, reason: &str, cause: Option<&str>) {
    let goals = state::read_lenient();
    let Some(goal) = goals.active.clone() else {
        return;
    };
    stop_current(&goal.id, status, reason, cause);
}

/// Stop `expected_id` only. A stale id (the goal was replaced or cleared) is a
/// no-op, so a delayed continuation cannot pause a newer goal.
pub fn stop_current(expected_id: &str, status: Status, reason: &str, cause: Option<&str>) {
    let stopped = {
        let session = crate::runtime::state();
        let _guard = state::lock(&session);
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if goal.id != expected_id {
            return;
        }
        let now = state::now_ms();
        goal.status = status;
        goal.safety_pause_cause = cause.map(|c| c.to_string());
        goal.waiting = None;
        goal.updated_at = now;
        goal.stop_clock(now);
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    let notice = prompts::stop_notice(&stopped, reason);
    inject::queue(&stopped, Some(notice));
    state::flush();
    inject::flush();
    crate::status::publish(&stopped);
    // The stop is now committed: end the run that was still working instead of
    // letting it finish work nobody asked for.
    request_abort();
}

pub fn wait_current(expected_id: &str, waiting: crate::state::Waiting) {
    let goal = {
        let session = crate::runtime::state();
        let _guard = state::lock(&session);
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if goal.id != expected_id {
            return;
        }
        let now = state::now_ms();
        goal.updated_at = now;
        goal.waiting = Some(waiting.clone());
        goal.stop_clock(now);
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    let notice = prompts::waiting_notice(&goal, &waiting);
    inject::queue(&goal, Some(notice));
    state::flush();
    inject::flush();
    crate::status::publish(&goal);
}

/// Compact `active` view used by the tests and the status line.
pub fn is_running() -> bool {
    let session = crate::runtime::state();
    let _guard = state::lock(&session);
    let state: Goals = state::read_lenient();
    state.active.is_some()
}
