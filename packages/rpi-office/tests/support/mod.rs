//! Shared fake host for the plugin-level integration tests.
//!
//! Provides a `PluginApi` whose registrars record what the plugin registers, a
//! runtime-action double for the actions this extension uses, and helpers to
//! invoke the registered callbacks with their original host or plugin context.
#![allow(dead_code)]

use rpi_abi_test_support::{tool_registrar, ToolCallbacks, ToolRegistration, ToolSchema};
use rpi_plugin_sdk::{
    AutocompleteProviderFn, BeforeAgentStartFn, CommandArgumentCompletionFn, CommandHandlerFn,
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, PluginBeginShutdownFn,
    PluginFinishShutdownFn, RuntimeActionId, StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

pub const ABI: u32 = rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED;

/// A registered tool and its ABI callbacks.
pub type RegisteredTool = ToolCallbacks;

type Lifecycle = (PluginBeginShutdownFn, PluginFinishShutdownFn, usize);
type Tool = (String, String, Value, RegisteredTool);

#[derive(Default)]
struct Host {
    tools: Vec<Tool>,
    events: Vec<(EventTag, EventHandlerFn, usize)>,
    commands: Vec<(String, CommandHandlerFn)>,
    flags: Vec<String>,
    prompt: Option<(BeforeAgentStartFn, usize)>,
    active: Vec<String>,
    status: Option<String>,
    prompts: Vec<(String, Value)>,
    lifecycle: Option<Lifecycle>,
    aborts: u64,
    autocomplete: Option<(AutocompleteProviderFn, usize)>,
    command_completions: Vec<(String, CommandArgumentCompletionFn, usize)>,
}

static HOSTS: Mutex<Option<HashMap<usize, Host>>> = Mutex::new(None);
static SENT: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

thread_local! {
    // Registration APIs such as register_tool have no owner argument. Native
    // supplies their context on the registration thread as this harness does.
    static OWNER: Cell<usize> = const { Cell::new(0) };
}

pub fn select_host(owner: usize) {
    OWNER.set(owner);
}

fn host_for<R>(owner: usize, f: impl FnOnce(&mut Host) -> R) -> R {
    let mut hosts = HOSTS.lock().unwrap();
    f(hosts
        .get_or_insert_with(HashMap::new)
        .entry(owner)
        .or_default())
}

fn host<R>(f: impl FnOnce(&mut Host) -> R) -> R {
    host_for(OWNER.get(), f)
}

struct ToolRegistrar;

impl ToolRegistration for ToolRegistrar {
    fn register_tool(schema: ToolSchema, callbacks: ToolCallbacks) -> i32 {
        let ToolSchema {
            name,
            description,
            parameters,
        } = schema;
        let parameters = serde_json::from_str(&parameters).expect("valid tool schema");
        host(|host| {
            // A real host activates every registered tool by default.
            host.active.push(name.clone());
            host.tools.push((name, description, parameters, callbacks));
        });
        0
    }
}

extern "C" fn register_event(
    tag: EventTag,
    handler: EventHandlerFn,
    user_data: *mut c_void,
) -> i32 {
    host(|host| host.events.push((tag, handler, user_data as usize)));
    0
}

extern "C" fn register_command(
    name: StbStringRef,
    _description: StbStringRef,
    handler: CommandHandlerFn,
) -> i32 {
    let name = rpi_plugin_sdk::stb_text!(name);
    host(|host| host.commands.push((name, handler)));
    0
}

extern "C" fn register_flag(name: StbStringRef, _description: StbStringRef) -> i32 {
    let name = rpi_plugin_sdk::stb_text!(name);
    host(|host| host.flags.push(name));
    0
}

extern "C" fn register_tool_prompt(name: StbStringRef, metadata: StbStringRef) -> i32 {
    let name = rpi_plugin_sdk::stb_text!(name);
    let metadata: Value = serde_json::from_str(&rpi_plugin_sdk::stb_text!(metadata))
        .expect("prompt metadata is JSON");
    host(|host| host.prompts.push((name, metadata)));
    0
}

extern "C" fn register_plugin_lifecycle(
    begin: PluginBeginShutdownFn,
    finish: PluginFinishShutdownFn,
    user_data: *mut c_void,
) -> i32 {
    host(|host| host.lifecycle = Some((begin, finish, user_data as usize)));
    0
}

extern "C" fn register_autocomplete_provider(
    handler: AutocompleteProviderFn,
    _free: FreeStringFn,
    user_data: *mut c_void,
) -> i32 {
    host(|host| host.autocomplete = Some((handler, user_data as usize)));
    0
}

extern "C" fn register_command_completions(
    name: StbStringRef,
    handler: CommandArgumentCompletionFn,
    _free: FreeStringFn,
    user_data: *mut c_void,
) -> i32 {
    let name = rpi_plugin_sdk::stb_text!(name);
    host(|host| {
        host.command_completions
            .push((name, handler, user_data as usize))
    });
    0
}

extern "C" fn register_before_agent_start(
    handler: BeforeAgentStartFn,
    _free: FreeStringFn,
    user_data: *mut c_void,
) -> i32 {
    host(|host| host.prompt = Some((handler, user_data as usize)));
    0
}

rpi_plugin_sdk::string_handler! {
fn runtime_action(action, args, out, user_data) {
    let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let id = RuntimeActionId::try_from(action).expect("known runtime action");
    match id {
        RuntimeActionId::GetCliFlag => {
            out.write(json!({"value": Value::Null}).to_string());
        }
        RuntimeActionId::GetActiveTools => {
            let tools = host_for(user_data as usize, |host| host.active.clone());
            out.write(json!({"tools": tools}).to_string());
        }
        RuntimeActionId::SetActiveTools => {
            let tools: Vec<String> = args["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect();
            host_for(user_data as usize, |host| host.active = tools);
            out.write(json!({"ok": true}).to_string());
        }
        RuntimeActionId::SetStatus => {
            host_for(user_data as usize, |host| {
                host.status = args.get("value").and_then(Value::as_str).map(str::to_owned);
            });
            out.write(json!({"ok": true}).to_string());
        }
        RuntimeActionId::Abort => {
            host_for(user_data as usize, |host| host.aborts += 1);
            out.write(
                json!({"aborted": true, "runId": "run-test", "steer": [], "followUp": []})
                    .to_string(),
            );
        }
        RuntimeActionId::SendMessage | RuntimeActionId::SendUserMessage => {
            SENT
                .lock()
                .unwrap()
                .push((user_data as usize, args["text"].as_str().unwrap_or_default().to_string()));
            out.write(json!({"status": "queued"}).to_string());
        }
        other => panic!("unexpected runtime action: {other:?}"),
    }
    0
}
}

/// The fake host API.
pub fn api() -> PluginApi {
    PluginApi {
        abi_version: ABI,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        register_tool: Some(tool_registrar::<ToolRegistrar>()),
        register_command: Some(register_command),
        register_shortcut: None,
        register_flag: Some(register_flag),
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: Some(register_event),
        register_resources_discover: None,
        runtime_action,
        dispatch_event: None,
        user_data: std::ptr::null_mut(),
        declare: None,
        register_before_agent_start: Some(register_before_agent_start),
        register_oauth_provider: None,
        register_streaming_provider: None,
        register_tool_with_wake: None,
        register_plugin_lifecycle: Some(register_plugin_lifecycle),
        register_tool_prompt: Some(register_tool_prompt),
        register_oauth_provider_cancellable: None,
        register_tool_result: None,
        register_autocomplete_provider: Some(register_autocomplete_provider),
        register_command_completions: Some(register_command_completions),
        register_argument_suggester: None,
    }
}

/// Register the plugin against the fake host.
pub fn register_plugin() {
    register_plugin_for(0);
}

pub fn register_plugin_for(owner: usize) {
    select_host(owner);
    let mut api = api();
    api.user_data = owner as *mut c_void;
    assert_eq!(rpi_office::rpi_plugin_register(&api), 0);
}

/// `(name, description, parameters, callbacks)` for every registered tool.
pub fn tools() -> Vec<(String, String, Value, RegisteredTool)> {
    host(|host| host.tools.clone())
}

pub fn tool(name: &str) -> Option<(String, Value, RegisteredTool)> {
    host(|host| {
        host.tools
            .iter()
            .find(|(registered, _, _, _)| registered == name)
            .map(|(_, description, parameters, callbacks)| {
                (description.clone(), parameters.clone(), *callbacks)
            })
    })
}

/// Registered `(name, handler)` command pairs.
pub fn commands() -> Vec<(String, CommandHandlerFn)> {
    host(|host| host.commands.clone())
}

pub fn flags() -> Vec<String> {
    host(|host| host.flags.clone())
}

pub fn active_tools() -> Vec<String> {
    host(|host| host.active.clone())
}

pub fn status() -> Option<String> {
    host(|host| host.status.clone())
}

/// `(tool name, metadata)` pairs registered through `register_tool_prompt`.
pub fn prompts() -> Vec<(String, Value)> {
    host(|host| host.prompts.clone())
}

pub fn lifecycle_hooks() -> Option<Lifecycle> {
    host(|host| host.lifecycle)
}

/// Whether an autocomplete provider was registered.
pub fn autocomplete_registered() -> bool {
    host(|host| host.autocomplete.is_some())
}

/// Registered `(command name, handler)` argument-completion pairs.
pub fn command_completions() -> Vec<(String, CommandArgumentCompletionFn)> {
    host(|host| {
        host.command_completions
            .iter()
            .map(|(name, f, _)| (name.clone(), *f))
            .collect()
    })
}

/// Invoke the autocomplete provider with a fake editor query; returns `None`
/// when the provider declined (return code 1 / empty output).
pub fn run_autocomplete(text: &str, line_prefix: &str) -> Option<Value> {
    let (handler, cookie) = host(|host| host.autocomplete).expect("autocomplete registered");
    let query = json!({
        "text": text,
        "cursor": line_prefix.len(),
        "line": line_prefix,
        "linePrefix": line_prefix,
    })
    .to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&query),
        &mut out,
        cookie as *mut c_void,
    );
    // SAFETY: the provider produced `out`; read then free it exactly once.
    let text = rpi_plugin_sdk::stb_take!(
        out,
        Some(rpi_plugin_sdk::callbacks::free_string as rpi_plugin_sdk::FreeStringFn)
    );
    if rc != 0 || text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// Invoke a command's argument completions; returns `None` when it declines.
pub fn run_command_completions(command: &str, prefix: &str) -> Option<Value> {
    let (handler, cookie) = host(|host| {
        host.command_completions
            .iter()
            .find(|(name, _, _)| name == command)
            .map(|(_, handler, cookie)| (*handler, *cookie))
    })
    .unwrap_or_else(|| panic!("no argument completions for /{command}"));
    let args = json!({"argumentPrefix": prefix}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&args),
        &mut out,
        cookie as *mut c_void,
    );
    // SAFETY: the handler produced `out`; read then free it exactly once.
    let text = rpi_plugin_sdk::stb_take!(
        out,
        Some(rpi_plugin_sdk::callbacks::free_string as rpi_plugin_sdk::FreeStringFn)
    );
    if rc != 0 || text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// How many `RuntimeActionId::Abort` calls the plugin made.
pub fn abort_count() -> u64 {
    host(|host| host.aborts)
}

/// Invoke the registered `begin_shutdown` hook (non-blocking half).
pub fn run_begin_shutdown() {
    let (begin, _, context) = lifecycle_hooks().expect("lifecycle hooks registered");
    begin(context as *mut std::ffi::c_void);
}

/// Invoke the registered `finish_shutdown` hook and return its status.
pub fn run_finish_shutdown() -> i32 {
    let (_, finish, context) = lifecycle_hooks().expect("lifecycle hooks registered");
    finish(context as *mut std::ffi::c_void)
}

/// Texts injected into the session via `SendMessage`/`SendUserMessage`.
pub fn sent_messages() -> Vec<String> {
    SENT.lock()
        .unwrap()
        .iter()
        .map(|(_, text)| text.clone())
        .collect()
}

pub fn sent_message_owners() -> Vec<usize> {
    SENT.lock()
        .unwrap()
        .iter()
        .map(|(owner, _)| *owner)
        .collect()
}

/// Invoke a registered slash command and return its JSON response.
pub fn run_command(name: &str, args: &str) -> Value {
    let handler = host(|host| {
        host.commands
            .iter()
            .find(|(registered, _)| registered == name)
            .map(|(_, handler)| *handler)
    })
    .unwrap_or_else(|| panic!("command /{name} is not registered"));
    let input = json!({"command": name, "args": args}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut out,
        OWNER.get() as *mut c_void,
    );
    // SAFETY: the host produced `out`; read then free it exactly once.
    let text = rpi_plugin_sdk::stb_take!(
        out,
        Some(rpi_plugin_sdk::callbacks::free_string as rpi_plugin_sdk::FreeStringFn)
    );
    assert_eq!(rc, 0);
    serde_json::from_str(&text).expect("command returns JSON")
}

/// Dispatch an event to the handler registered for `tag`.
pub fn run_event(tag: EventTag, event: StablePluginEvent) -> i32 {
    let (handler, cookie) = host(|host| {
        host.events
            .iter()
            .find(|(registered, _, _)| *registered == tag)
            .map(|(_, handler, cookie)| (*handler, *cookie))
    })
    .unwrap_or_else(|| panic!("no handler for {tag:?}"));
    handler(event, cookie as *mut c_void)
}

/// Dispatch an empty-payload event (SessionStart, AgentSettled, …).
pub fn run_empty_event(tag: EventTag) {
    let rc = run_event(tag, StablePluginEvent::empty(tag));
    assert_eq!(rc, 0, "event handler {tag:?} failed");
}

/// Dispatch a message-carrying event (MessageStart/Update/End).
pub fn run_message_event(tag: EventTag, payload: Value) {
    let text = payload.to_string();
    let event = StablePluginEvent::message(tag, StbString::from_string(text));
    let rc = run_event(tag, event);
    assert_eq!(rc, 0, "event handler {tag:?} failed");
}

/// Run the registered `before_agent_start` transformer.
pub fn run_before_agent_start(system_prompt: &str) -> Option<String> {
    let (handler, cookie) = host(|host| host.prompt).expect("prompt transformer registered");
    let input = json!({"systemPrompt": system_prompt, "imageCount": 0}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut out,
        cookie as *mut c_void,
    );
    // SAFETY: the host produced `out`; read then free it exactly once.
    let text = rpi_plugin_sdk::stb_take!(
        out,
        Some(rpi_plugin_sdk::callbacks::free_string as rpi_plugin_sdk::FreeStringFn)
    );
    assert_eq!(rc, 0);
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `GET /health` against the live bridge.
pub fn health(port: u16) -> Value {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("write");
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let (_, body) = raw.split_once("\r\n\r\n").expect("body");
    serde_json::from_str(body).expect("json body")
}

/// An unused loopback port (bound and released).
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    listener.local_addr().unwrap().port()
}

/// Wait until `predicate` holds, or fail after ~3 s.
pub fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    for _ in 0..150 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}
