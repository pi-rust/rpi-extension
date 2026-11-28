use rpi_abi_test_support::{tool_registrar, ToolCallbacks, ToolRegistration, ToolSchema};
use rpi_mcp_adapter::rpi_plugin_register;
use rpi_plugin_sdk::{callbacks, *};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct Tool(ToolExecuteFn, ToolPollFn, ToolCancelFn, ToolDestroyFn);

type Lifecycle = (
    extern "C" fn(*mut c_void),
    extern "C" fn(*mut c_void) -> i32,
    *mut c_void,
);
type LifecycleBegin = extern "C" fn(*mut c_void);
type LifecycleFinish = extern "C" fn(*mut c_void) -> i32;

#[derive(Default)]
struct Callbacks {
    tools: BTreeMap<String, Tool>,
    events: Vec<(EventTag, EventHandlerFn, *mut c_void)>,
    lifecycle: Option<Lifecycle>,
}

type Messages = RefCell<Vec<Value>>;

thread_local! {
    static MESSAGES: RefCell<BTreeMap<usize, Weak<Messages>>> = RefCell::default();
    static REGISTERING: RefCell<Callbacks> = RefCell::default();
}

struct ToolRegistrar;

impl ToolRegistration for ToolRegistrar {
    fn register_tool(schema: ToolSchema, callbacks: ToolCallbacks) -> i32 {
        let ToolCallbacks {
            execute,
            poll,
            cancel,
            destroy,
        } = callbacks;
        REGISTERING.with_borrow_mut(|host| {
            host.tools
                .insert(schema.name, Tool(execute, poll, cancel, destroy));
        });
        0
    }
}

extern "C" fn event(tag: EventTag, callback: EventHandlerFn, cookie: *mut c_void) -> i32 {
    REGISTERING.with_borrow_mut(|host| host.events.push((tag, callback, cookie)));
    0
}

extern "C" fn lifecycle(
    begin: LifecycleBegin,
    finish: LifecycleFinish,
    cookie: *mut c_void,
) -> i32 {
    REGISTERING.with_borrow_mut(|host| host.lifecycle = Some((begin, finish, cookie)));
    0
}

rpi_plugin_sdk::string_handler! {
fn action(id, args, out, owner) {
    match RuntimeActionId::try_from(id) {
        Ok(RuntimeActionId::GetExecutionContext) => {
            out.write(json!({"sessionId":"duplicate"}).to_string());
        }
        Ok(RuntimeActionId::AppendEntry) => {
            let Some(messages) = MESSAGES.with_borrow(|owners| {
                owners.get(&(owner as usize)).and_then(Weak::upgrade)
            }) else {
                return 1;
            };
            let Ok(message) = serde_json::from_str(args) else {
                return 1;
            };
            messages.borrow_mut().push(message);
        }
        _ => return 1,
    }
    0
}
}

struct Host {
    callbacks: Callbacks,
    messages: Rc<Messages>,
}

impl Host {
    fn new(lifecycle_tail: bool) -> Self {
        let messages = Rc::<Messages>::default();
        let owner = Rc::as_ptr(&messages);
        MESSAGES.with_borrow_mut(|owners| owners.insert(owner as usize, Rc::downgrade(&messages)));
        let api = PluginApi {
            abi_version: RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: if lifecycle_tail {
                size_of::<PluginApi>() as u32
            } else {
                std::mem::offset_of!(PluginApi, register_streaming_provider) as u32
            },
            free_string: callbacks::free_string,
            register_tool: Some(tool_registrar::<ToolRegistrar>()),
            register_command: None,
            register_shortcut: None,
            register_flag: None,
            register_provider: None,
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: Some(event),
            register_resources_discover: None,
            runtime_action: action,
            dispatch_event: None,
            user_data: owner.cast_mut().cast(),
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
            register_streaming_provider: None,
            register_tool_with_wake: None,
            register_plugin_lifecycle: lifecycle_tail.then_some(lifecycle),
            register_tool_prompt: None,
            register_oauth_provider_cancellable: None,
            register_tool_result: None,
            register_autocomplete_provider: None,
            register_command_completions: None,
            register_argument_suggester: None,
        };
        let rc = rpi_plugin_register(&api);
        assert_eq!(rc, 0);
        let callbacks = REGISTERING.with_borrow_mut(std::mem::take);
        assert_eq!(callbacks.lifecycle.is_some(), lifecycle_tail);
        Self {
            callbacks,
            messages,
        }
    }

    fn event(&self, tag: EventTag) {
        let &(_, handler, cookie) = self
            .callbacks
            .events
            .iter()
            .find(|entry| entry.0 == tag)
            .unwrap();
        assert_eq!(handler(StablePluginEvent::empty(tag), cookie), 0);
    }

    fn start(&self, name: &str, params: Value) -> Step {
        self.start_with_context(name, params, json!({"sessionId":"duplicate"}))
    }

    fn start_with_context(&self, name: &str, mut params: Value, context: Value) -> Step {
        params["__rpi"] = context;
        let tool = self.callbacks.tools[name];
        let handle = (tool.0)(
            StbStringRef::from_str("test"),
            StbString::from_string(params.to_string()),
            Some(callbacks::free_string),
        );
        Step(tool, handle)
    }

    fn run(&self, name: &str, params: Value) -> Value {
        self.start(name, params).result().unwrap()
    }

    fn finish(&mut self) {
        if let Some((begin, finish, cookie)) = self.callbacks.lifecycle.take() {
            begin(cookie);
            assert_eq!(finish(cookie), 0);
        }
    }

    fn receive_update(&self) -> String {
        let start = Instant::now();
        loop {
            self.event(EventTag::TurnEnd);
            if let Some(entry) = self.messages.borrow_mut().pop() {
                return entry["message"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned();
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "missing resource update"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.event(EventTag::SessionShutdown);
        self.finish();
        MESSAGES.with_borrow_mut(|owners| owners.remove(&(Rc::as_ptr(&self.messages) as usize)));
    }
}

struct Step(Tool, StepHandle);
impl Step {
    fn result(&self) -> Result<Value, String> {
        let start = Instant::now();
        loop {
            match take_step!((self.0 .1)(self.1, None, std::ptr::null_mut())) {
                callbacks::Step::Done(value) => return Ok(serde_json::from_str(&value).unwrap()),
                callbacks::Step::Err(error) => return Err(error),
                _ => {}
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "tool did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Step {
    fn drop(&mut self) {
        (self.0 .2)(self.1);
        (self.0 .3)(self.1);
    }
}

struct Config(PathBuf);
impl Config {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mcp-slot-test-{}.json", std::process::id()));
        let server = env!("CARGO_BIN_EXE_fake_mcp_server");
        std::fs::write(&path, json!({"mcpServers":{
            "fake":{"command":server,"args":["--paged","--stateful-resources","--notify-early","--delivery-values"]},
            "hang":{"command":server,"args":["--hang-call"]}
        }}).to_string()).unwrap();
        Self(path)
    }
}
impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn counter(host: &Host, path: &Path) -> (String, u32) {
    let result = host.run(
        "mcp_call",
        json!({"configPath":path,"server":"fake","tool":"counter"}),
    );
    let (pid, count) = result["content"][0]["text"]
        .as_str()
        .unwrap()
        .split_once(':')
        .unwrap();
    (pid.to_owned(), count.parse().unwrap())
}

#[test]
fn duplicate_sessions_have_independent_tools_events_and_cleanup() {
    let config = Config::new();
    let path = &config.0;
    let mut a = Host::new(true);
    let b = Host::new(true);
    let (pid_a, count_a) = counter(&a, path);
    let (pid_b, count_b) = counter(&b, path);
    assert_ne!(pid_a, pid_b);
    assert_eq!((count_a, count_b), (1, 1));
    for (host, label) in [(&a, "first owner"), (&b, "second owner")] {
        host.run("mcp_call", json!({"configPath":path,"server":"fake","tool":"echo","arguments":{"value":label,"quiet":true}}));
        host.run(
            "mcp_resource",
            json!({"configPath":path,"server":"fake","action":"subscribe","uri":"mem://state"}),
        );
    }
    let update = a.receive_update();
    assert!(
        update.contains("first owner") && !update.contains("second owner"),
        "{update}"
    );

    let busy = a.start(
        "mcp_call",
        json!({"configPath":path,"server":"hang","tool":"echo"}),
    );
    a.event(EventTag::SessionShutdown);
    assert!(busy.result().unwrap_err().contains("cancelled"));
    drop(busy);
    a.finish();

    // The old callback cookie stays retired if a new registration reuses its execute slot.
    let legacy = Host::new(false);
    let (_, count) = counter(&legacy, path);
    assert_eq!(count, 1);
    a.event(EventTag::SessionShutdown);
    a.event(EventTag::TurnEnd);
    let (_, count) = counter(&legacy, path);
    assert_eq!(count, 2);

    let update = b.receive_update();
    assert!(
        update.contains("second owner") && !update.contains("first owner"),
        "{update}"
    );
    // The initial counter call and the intervening echo each advanced the server.
    assert_eq!(counter(&b, path), (pid_b, count_b + 2));
}

#[test]
fn compact_discovery_validation_and_policy_cross_plugin_abi() {
    struct Workspace(PathBuf);
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let workspace =
        Workspace(std::env::temp_dir().join(format!("mcp-discovery-{}", std::process::id())));
    let project = workspace.0.join(".rpi");
    std::fs::create_dir_all(&project).unwrap();
    let log = workspace.0.join("requests.jsonl");
    let denied_log = workspace.0.join("denied.jsonl");
    let executable = env!("CARGO_BIN_EXE_fake_mcp_server");
    std::fs::write(project.join("mcp.json"), json!({"mcpServers": {
        "dbx": {
            "command": executable,
            "args": ["--discovery-catalog", format!("--ownership-log={}", log.display())],
        },
        "bad": {
            "command": executable,
            "args": ["--hang"],
            "timeout": 1,
        },
        "guarded": {
            "command": executable,
            "args": ["--discovery-catalog", format!("--ownership-log={}", denied_log.display())],
            "toolPolicy": {"readOnly":["dbx_*"],"deny":["dbx_execute_*"],"preset":"database"}
        }
    }}).to_string()).unwrap();

    let host = Host::new(true);
    let run = |name, mut params: Value| {
        params["configPath"] = json!(".rpi/mcp.json");
        host.start_with_context(
            name,
            params,
            json!({"sessionId":"discovery","cwd":workspace.0}),
        )
        .result()
    };
    let listing = |params| {
        let result = run("mcp_list", params).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        (text.len(), serde_json::from_str::<Value>(text).unwrap())
    };
    let (_, listed) = listing(json!({}));
    assert_eq!(listed["servers"].as_array().unwrap().len(), 3);
    assert!(!log.exists());
    let error = run(
        "mcp_call",
        json!({"server":"guarded","tool":"dbx_execute_query"}),
    )
    .unwrap_err();
    assert!(error.contains("denied by toolPolicy"), "{error}");
    assert!(!denied_log.exists());

    let result = run(
        "mcp_call",
        json!({"server":"dbx","tool":"dbx_list_connections"}),
    )
    .unwrap();
    assert_eq!(result["content"][0]["text"], "[fixture-db]");
    let (compact_bytes, compact) = listing(json!({"server":"dbx"}));
    let (full_bytes, _) = listing(json!({"server":"dbx","detail":"full"}));
    assert_eq!(compact["tools"].as_array().unwrap().len(), 23);
    assert_eq!(compact["tools"][1]["required"], json!(["table"]));
    assert!(
        compact_bytes * 8 < full_bytes,
        "{compact_bytes} vs {full_bytes}"
    );
    assert!(compact["tools"][1].get("inputSchema").is_none());

    let (_, selected) = listing(json!({"server":"dbx","tools":["dbx_describe_table"]}));
    assert_eq!(selected["tools"].as_array().unwrap().len(), 1);
    let tool = &selected["tools"][0];
    assert_eq!(
        tool["inputSchema"]["properties"]["format"]["enum"],
        json!(["brief", "full"])
    );
    assert!(tool["inputSchema"].get("$schema").is_none());
    assert!(tool.to_string().len() <= 360);

    let calls = || {
        std::fs::read_to_string(&log)
            .unwrap()
            .matches("tools/call")
            .count()
    };
    for (arguments, field) in [
        (json!({"table_name":"inventory"}), "table"),
        (json!({"table":false}), "table"),
        (json!({"table":"inventory","format":"xml"}), "format"),
        (json!({"table":"inventory","unknown":true}), "unknown"),
    ] {
        let error = run(
            "mcp_call",
            json!({"server":"dbx","tool":"dbx_describe_table","arguments":arguments}),
        )
        .unwrap_err();
        let error: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(error["code"], "invalid_tool_arguments");
        assert_eq!(error["field"], field);
        assert_eq!(
            error["signature"]["inputSchema"]["required"],
            json!(["table"])
        );
        assert_eq!(calls(), 1);
    }

    let (_, probed) = listing(json!({"probe":true,"timeoutSeconds":2}));
    let servers = probed["servers"].as_array().unwrap();
    assert_eq!(servers[0]["server"], "bad");
    assert_eq!(servers[0]["status"], "failed");
    assert!(servers[0]["error"].as_str().unwrap().contains("timed out"));
    assert!(servers[0]["stderr"]
        .as_str()
        .unwrap()
        .contains("deliberately blocked"));
    assert_eq!(servers[1]["toolCount"], 23);
    assert_eq!(servers[1]["status"], "ready");
    let after_probe = run(
        "mcp_call",
        json!({"server":"dbx","tool":"dbx_list_connections"}),
    )
    .unwrap();
    assert_eq!(after_probe["content"][0]["text"], "[fixture-db]");
    let events: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let starts: Vec<_> = events
        .iter()
        .filter(|entry| entry["event"] == "start")
        .collect();
    let calls: Vec<_> = events
        .iter()
        .filter(|entry| entry["details"]["method"] == "tools/call")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_ne!(starts[0]["pid"], starts[1]["pid"]);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["pid"], calls[1]["pid"]);
    assert_eq!(calls[0]["pid"], starts[0]["pid"]);
}
