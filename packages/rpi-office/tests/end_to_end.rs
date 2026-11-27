//! End-to-end: a pane drives an `office_*` tool through the real tool drive,
//! and a pane-typed prompt streams deltas + a final reply back to that pane.
//!
//! This is the path the add-in actually uses, exercised through the registered
//! ABI callbacks (`execute`/`poll`) and the host event handlers.

// The harness plays the host here; `take_step!` keeps the union-arm read and the
// reclaim inside the SDK, so this file needs no `unsafe` of its own.
#![forbid(unsafe_code)]

mod support;

use futures_util::{SinkExt, StreamExt};
use rpi_plugin_sdk::{EventTag, StbString, StbStringRef, StepHandle, StepResultTag};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16) -> Ws {
    let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket connect");
    ws
}

async fn send_frame(ws: &mut Ws, value: Value) {
    ws.send(Message::text(value.to_string()))
        .await
        .expect("send frame");
}

async fn recv_frame(ws: &mut Ws) -> Value {
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a server frame")
        .expect("stream ended")
        .expect("websocket error");
    match next {
        Message::Text(text) => serde_json::from_str(&text).expect("server frame is JSON"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

fn hello() -> Value {
    json!({
        "type": "hello",
        "protocolVersion": 3,
        "host": "excel",
        "clientName": "pi-for-office-e2e",
        "paneId": "pane-e2e",
        "document": {"id": "id-1", "name": "Book1.xlsx", "source": "document.url"},
        "ops": ["excel.read_range", "excel.get_overview"],
        "catalogVersion": 3,
    })
}

/// Run one tool call through the registered `execute`/`poll` callbacks.
fn run_tool(name: &str, params: Value) -> Result<Value, String> {
    let (_, _, callbacks) = support::tool(name).unwrap_or_else(|| panic!("{name} is registered"));
    finish_tool(callbacks, start_tool(callbacks, params))
}

fn start_tool(callbacks: support::RegisteredTool, params: Value) -> StepHandle {
    let handle = (callbacks.execute)(
        StbStringRef::from_str("call-1"),
        StbString::from_string(params.to_string()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    assert!(!handle.is_null(), "tool returned a null handle");
    handle
}

fn finish_tool(callbacks: support::RegisteredTool, handle: StepHandle) -> Result<Value, String> {
    let mut polls = 0;
    let result = loop {
        let result = (callbacks.poll)(handle, None, std::ptr::null_mut());
        match result.tag {
            StepResultTag::Pending => {
                polls += 1;
                assert!(polls < 2000, "tool never finished");
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => break result,
        }
    };
    (callbacks.destroy)(handle);
    match rpi_plugin_sdk::take_step!(result) {
        rpi_plugin_sdk::callbacks::Step::Done(text) => {
            serde_json::from_str(&text).map_err(|error| format!("invalid result JSON: {error}"))
        }
        rpi_plugin_sdk::callbacks::Step::Err(message) => Err(message),
        rpi_plugin_sdk::callbacks::Step::Pending(_) => unreachable!(),
    }
}

#[tokio::test]
async fn pane_drives_a_tool_and_receives_streamed_replies() {
    support::register_plugin_for(11);
    // A print-mode session never dispatches SessionStart, so AgentStart must be
    // enough to resolve flags and reconcile the active tool set.
    support::run_empty_event(EventTag::AgentStart);

    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));

    let mut ws = connect(port).await;
    send_frame(&mut ws, hello()).await;
    let welcome = recv_frame(&mut ws).await;
    assert_eq!(welcome["type"], json!("welcome"));

    support::wait_until("the pane to attach", || {
        support::health(port)["panes"].as_array().unwrap().len() == 1
    });
    // Attaching a pane activates exactly the ops it advertises.
    support::wait_until("the advertised tools to activate", || {
        let active = support::active_tools();
        active.contains(&"office_excel_read_range".to_string())
            && active.contains(&"office_list_documents".to_string())
            && !active.contains(&"office_excel_charts".to_string())
    });

    // ── `#document` completion offers the attached document ─────────────
    let mentions = support::run_autocomplete("#", "#").expect("mention items");
    let items = mentions["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "{mentions}");
    assert_eq!(items[0]["text"], json!("#Book1.xlsx"));
    assert_eq!(items[0]["description"], json!("Excel"));
    assert_eq!(items[0]["insertSpace"], json!(false));
    // Mid-word `#` and non-matching prefixes fall through to the host.
    assert!(support::run_autocomplete("a#", "a#").is_none());
    assert!(support::run_autocomplete("#zzz", "#zzz").is_none());
    // The inserted mention is what the server already resolves as a target.
    let by_mention = run_tool("office_list_documents", json!({"host": "excel"})).expect("listing");
    assert!(by_mention["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Book1.xlsx"));

    // ── tool proxy: execute → pane → tool_result → poll ────────────────
    let (_, _, callbacks) = support::tool("office_excel_read_range").expect("registered");
    let handle = (callbacks.execute)(
        StbStringRef::from_str("call-1"),
        StbString::from_string(
            json!({"range": "A1:B2", "document": "id-1", "__rpi": {"cwd": "/tmp", "sessionId": "s"}})
                .to_string(),
        ),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    let call = recv_frame(&mut ws).await;
    assert_eq!(call["type"], json!("tool_call"));
    assert_eq!(call["tool"], json!("excel.read_range"));
    assert_eq!(call["documentId"], json!("id-1"));
    assert!(call["args"].get("document").is_none());
    assert!(
        call["args"].get("__rpi").is_none(),
        "host metadata must never reach the pane"
    );
    assert_eq!(call["args"]["range"], json!("A1:B2"));

    let call_id = call["id"].as_str().unwrap().to_string();
    send_frame(
        &mut ws,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "A1=1 B1=2", "details": {"cells": 4}}),
    )
    .await;

    let mut polls = 0;
    let result = loop {
        let result = (callbacks.poll)(handle, None, std::ptr::null_mut());
        if result.tag != StepResultTag::Pending {
            break result;
        }
        polls += 1;
        assert!(polls < 2000, "the tool never finished");
        std::thread::sleep(Duration::from_millis(5));
    };
    (callbacks.destroy)(handle);
    assert_eq!(result.tag, StepResultTag::Done);
    let rpi_plugin_sdk::callbacks::Step::Done(text) = rpi_plugin_sdk::take_step!(result) else {
        panic!("the office tool must finish with Done");
    };
    let value: Value = serde_json::from_str(&text).expect("tool result JSON");
    assert_eq!(value["content"][0]["type"], json!("text"));
    assert_eq!(value["content"][0]["text"], json!("A1=1 B1=2"));
    assert_eq!(value["details"]["cells"], json!(4));

    // The discovery tool answers from the registry, not from the pane.
    let listing = run_tool("office_list_documents", json!({"host": "excel"})).expect("listing");
    let text = listing["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Book1.xlsx"), "{text}");
    assert!(text.contains("id-1"), "{text}");
    assert!(text.contains("| document | id | host | focused | windows |"));

    // ── pane prompt → host, tagged with the document ────────────────────
    send_frame(
        &mut ws,
        json!({"type": "user_message", "text": "  sum A1:A3 "}),
    )
    .await;
    support::wait_until("the pane prompt to reach the host", || {
        !support::sent_messages().is_empty()
    });
    let sent = support::sent_messages();
    assert_eq!(sent[0], "[office:excel \"Book1.xlsx\" id=id-1]\nsum A1:A3");

    // ── delivered turn streams deltas, then the final reply ─────────────
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": sent[0]}]}),
    );
    support::run_message_event(
        EventTag::MessageUpdate,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "部分"}}),
    );
    let delta = recv_frame(&mut ws).await;
    assert_eq!(delta["type"], json!("agent_message"));
    assert_eq!(delta["kind"], json!("delta"));
    assert_eq!(delta["text"], json!("部分"));
    let prompt_id = delta["promptId"].as_str().expect("prompt id").to_string();
    assert!(prompt_id.starts_with("prompt-"));

    support::run_message_event(
        EventTag::MessageEnd,
        json!({
            "role": "assistant",
            "id": "msg-1",
            "content": [{"type": "text", "text": "部分完成"}]
        }),
    );
    let final_message = recv_frame(&mut ws).await;
    assert_eq!(final_message["kind"], json!("final"));
    assert_eq!(final_message["text"], json!("部分完成"));
    assert_eq!(final_message["messageId"], json!("msg-1"));
    assert_eq!(final_message["promptId"], json!(prompt_id));

    // ── a TUI-initiated turn never reaches the pane ────────────────────
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": "a terminal question"}]}),
    );
    support::run_message_event(
        EventTag::MessageUpdate,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "terminal"}}),
    );
    support::run_message_event(
        EventTag::MessageEnd,
        json!({"role": "assistant", "content": [{"type": "text", "text": "terminal answer"}]}),
    );
    let quiet = tokio::time::timeout(Duration::from_millis(400), ws.next()).await;
    assert!(
        quiet.is_err(),
        "a terminal turn must not be forwarded to the pane: {quiet:?}"
    );

    // ── pane Stop aborts the run through the host action ────────────────
    send_frame(
        &mut ws,
        json!({"type": "user_message", "text": "stop this"}),
    )
    .await;
    support::wait_until("the second pane prompt", || {
        support::sent_messages().len() >= 2
    });
    let second_prompt = support::sent_messages()[1].clone();
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": second_prompt}]}),
    );
    send_frame(&mut ws, json!({"type": "abort"})).await;
    support::wait_until("the host Abort action", || support::abort_count() >= 1);
    // The aborted turn must not leak a late final into the stopped pane.
    support::run_message_event(
        EventTag::MessageEnd,
        json!({"role": "assistant", "content": [{"type": "text", "text": "late"}]}),
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        quiet.is_err(),
        "an aborted turn must not send a final to the pane: {quiet:?}"
    );

    // ── pane detach deactivates its tools again ─────────────────────────
    drop(ws);
    support::wait_until("the pane to detach", || {
        support::health(port)["panes"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    support::wait_until("the office tools to deactivate", || {
        !support::active_tools()
            .iter()
            .any(|name| name == "office_excel_read_range")
    });
    // The bridge stays on and keeps the discovery tool available.
    assert!(support::active_tools().contains(&"office_list_documents".to_string()));

    support::run_command("office", "close");

    // ── actionable errors when nothing can answer ───────────────────────
    // Bridge off: the tool reports the offline hint instead of hanging.
    let error = run_tool("office_excel_read_range", json!({"range": "A1"})).expect_err("offline");
    assert!(error.contains("/office connect"), "{error}");

    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));
    // Bridge on, no pane of that host: the error names the add-in.
    let error = run_tool("office_excel_read_range", json!({"range": "A1"})).expect_err("no pane");
    assert!(
        error.contains("no excel workbook/document is attached"),
        "{error}"
    );

    support::run_command("office", "close");

    // ── reload safety: begin/finish shutdown join both plugin threads ───
    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));
    assert_eq!(support::health(port)["ok"], json!(true));
    support::run_begin_shutdown();
    support::wait_until("the bridge to stop listening", || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
    });
    assert_eq!(support::run_finish_shutdown(), 0);
    assert_eq!(
        support::run_finish_shutdown(),
        0,
        "finish_shutdown is idempotent"
    );
    assert!(
        support::run_command("office", &format!("connect {port}"))["text"]
            .as_str()
            .unwrap()
            .contains("shutting down")
    );

    // A new registration can claim the port and start its own host worker.
    let (old_begin, old_finish, old_context) = support::lifecycle_hooks().unwrap();
    let sent_count = support::sent_messages().len();
    assert_eq!(support::sent_message_owners(), vec![11; sent_count]);
    support::register_plugin_for(22);
    old_begin(old_context as *mut std::ffi::c_void);
    assert_eq!(old_finish(old_context as *mut std::ffi::c_void), 0);
    support::run_empty_event(EventTag::AgentStart);
    support::run_command("office", &format!("connect {port}"));
    let mut ws = connect(port).await;
    send_frame(&mut ws, hello()).await;
    assert_eq!(recv_frame(&mut ws).await["type"], json!("welcome"));
    send_frame(
        &mut ws,
        json!({"type": "user_message", "text": "prompt after reload"}),
    )
    .await;
    support::wait_until("the new worker to deliver the prompt", || {
        support::sent_messages().len() == sent_count + 1
    });
    assert_eq!(support::sent_message_owners().last(), Some(&22));
    support::run_begin_shutdown();
    assert_eq!(support::run_finish_shutdown(), 0);

    shared_registrations_interleave().await;
}

async fn shared_registrations_interleave() {
    const FIRST: usize = 31;
    const SECOND: usize = 32;
    support::register_plugin_for(FIRST);
    support::run_empty_event(EventTag::SessionStart);
    let first_port = support::free_port();
    let first_status = support::run_command("office", &format!("connect {first_port}"));
    assert!(first_status["text"].as_str().unwrap().contains("listening"));
    let mut first = connect(first_port).await;
    send_frame(&mut first, hello()).await;
    assert_eq!(recv_frame(&mut first).await["type"], "welcome");
    let first_tool = support::tool("office_excel_read_range").unwrap().2;

    support::register_plugin_for(SECOND);
    support::run_empty_event(EventTag::AgentStart);
    assert!(support::active_tools().is_empty());
    let conflict = support::run_command("office", &format!("connect {first_port}"));
    assert!(conflict["text"]
        .as_str()
        .unwrap()
        .contains("already in use"));
    let second_port = support::free_port();
    let second_status = support::run_command("office", &format!("connect {second_port}"));
    assert!(second_status["text"]
        .as_str()
        .unwrap()
        .contains("listening"));
    let mut second = connect(second_port).await;
    let mut second_hello = hello();
    // Keep the same pane ID across registrations and use different documents.
    second_hello["document"]["id"] = json!("id-2");
    second_hello["document"]["name"] = json!("Book2.xlsx");
    second_hello["ops"] = json!(["excel.read_range"]);
    send_frame(&mut second, second_hello).await;
    assert_eq!(recv_frame(&mut second).await["type"], "welcome");
    let second_tool = support::tool("office_excel_read_range").unwrap().2;

    for (owner, name, other, port, overview) in [
        (FIRST, "Book1.xlsx", "Book2.xlsx", first_port, true),
        (SECOND, "Book2.xlsx", "Book1.xlsx", second_port, false),
    ] {
        support::select_host(owner);
        support::wait_until("the Office tool set to reconcile", || {
            support::active_tools().contains(&"office_excel_read_range".to_string())
        });
        assert_eq!(
            support::active_tools().contains(&"office_excel_get_overview".to_string()),
            overview
        );
        let status = support::run_command("office", "status");
        let status = status["text"].as_str().unwrap();
        assert!(
            status.contains(&port.to_string()) && status.contains(name) && !status.contains(other)
        );
        let prompt = support::run_before_agent_start("original").unwrap();
        assert!(prompt.contains(name) && !prompt.contains(other));
        let mentions = support::run_autocomplete("#", "#").unwrap();
        assert_eq!(mentions["items"].as_array().unwrap().len(), 1);
        assert_eq!(mentions["items"][0]["text"], format!("#{name}"));
        assert!(support::run_command_completions("office", "co").is_some());
        let listing = run_tool("office_list_documents", json!({})).unwrap();
        let listing = listing["content"][0]["text"].as_str().unwrap();
        assert!(listing.contains(name) && !listing.contains(other));
    }

    let first_call = start_tool(first_tool, json!({"range": "A1"}));
    let second_call = start_tool(second_tool, json!({"range": "B2"}));
    assert_eq!(recv_frame(&mut first).await["documentId"], "id-1");
    let second_frame = recv_frame(&mut second).await;
    assert_eq!(second_frame["documentId"], "id-2");
    (first_tool.cancel)(first_call);
    assert!(finish_tool(first_tool, first_call)
        .unwrap_err()
        .contains("aborted"));
    send_frame(
        &mut second,
        json!({"type":"tool_result", "id":second_frame["id"], "ok":true, "text":"second result"}),
    )
    .await;
    assert_eq!(
        finish_tool(second_tool, second_call).unwrap()["content"][0]["text"],
        "second result"
    );

    let sent_count = support::sent_messages().len();
    send_frame(
        &mut first,
        json!({"type":"user_message", "text":"first prompt"}),
    )
    .await;
    send_frame(
        &mut second,
        json!({"type":"user_message", "text":"second prompt"}),
    )
    .await;
    support::wait_until("both pane prompts to reach the host", || {
        support::sent_messages().len() == sent_count + 2
    });
    let messages = support::sent_messages();
    let owners = support::sent_message_owners();
    for (owner, text) in [(FIRST, "first prompt"), (SECOND, "second prompt")] {
        let i = messages.iter().position(|m| m.ends_with(text)).unwrap();
        assert_eq!(owners[i], owner);
        support::select_host(owner);
        support::run_message_event(
            EventTag::MessageStart,
            json!({"role":"user", "content":messages[i]}),
        );
    }
    support::select_host(FIRST);
    support::run_empty_event(EventTag::SessionShutdown);
    let (first_begin, first_finish, first_cookie) = support::lifecycle_hooks().unwrap();
    support::run_begin_shutdown();
    assert_eq!(support::run_finish_shutdown(), 0);
    assert!(std::net::TcpListener::bind(("127.0.0.1", first_port)).is_ok());
    assert!(support::status().is_none());
    assert!(support::run_autocomplete("#", "#").is_none());
    assert!(support::run_command_completions("office", "co").is_none());
    assert!(run_tool("office_list_documents", json!({})).is_err());

    // Reuse the released slot and port while SECOND still has a bound prompt.
    support::register_plugin_for(33);
    support::run_empty_event(EventTag::SessionStart);
    first_begin(first_cookie as *mut std::ffi::c_void);
    assert_eq!(first_finish(first_cookie as *mut std::ffi::c_void), 0);
    let restarted = support::run_command("office", &format!("connect {first_port}"));
    assert!(restarted["text"].as_str().unwrap().contains("listening"));
    support::select_host(FIRST);
    support::run_empty_event(EventTag::SessionShutdown); // retired callback cookie
    assert_eq!(support::health(first_port)["ok"], true);

    support::select_host(SECOND);
    support::run_message_event(
        EventTag::MessageEnd,
        json!({"role":"assistant", "content":"second reply"}),
    );
    let reply = recv_frame(&mut second).await;
    assert_eq!(reply["kind"], "final");
    assert_eq!(reply["text"], "second reply");
    assert_eq!(support::health(second_port)["ok"], true);
    support::run_begin_shutdown();
    assert_eq!(support::run_finish_shutdown(), 0);
    support::select_host(33);
    support::run_begin_shutdown();
    assert_eq!(support::run_finish_shutdown(), 0);
}
