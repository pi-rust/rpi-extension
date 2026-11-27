use rpi_mcp_adapter::rpi_plugin_register;
use rpi_plugin_sdk::{callbacks, *};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
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
type RegisterLifecycle = extern "C" fn(LifecycleBegin, LifecycleFinish, *mut c_void) -> i32;
type LifecycleBegin = extern "C" fn(*mut c_void);
type LifecycleFinish = extern "C" fn(*mut c_void) -> i32;

#[repr(C)]
struct HostApi {
    prefix: PluginApi,
    streaming_and_wake_slots: [usize; 2],
    lifecycle: Option<RegisterLifecycle>,
}

#[derive(Default)]
struct Callbacks {
    tools: BTreeMap<String, Tool>,
    events: Vec<(EventTag, EventHandlerFn, *mut c_void)>,
    lifecycle: Option<Lifecycle>,
}

thread_local! {
    static REGISTERING: RefCell<Callbacks> = RefCell::default();
}

extern "C" fn tool(
    schema: *const StableToolSchema,
    exec: ToolExecuteFn,
    poll: ToolPollFn,
    cancel: ToolCancelFn,
    destroy: ToolDestroyFn,
    free: FreeStringFn,
) -> i32 {
    // SAFETY: the plugin passes a live schema with three owned strings.
    let schema = unsafe { &*schema };
    let name = schema.name.to_string_lossy();
    for value in [schema.name, schema.description, schema.parameters] {
        free(value);
    }
    REGISTERING.with_borrow_mut(|host| host.tools.insert(name, Tool(exec, poll, cancel, destroy)));
    0
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

extern "C" fn action(id: u32, args: StbStringRef, out: *mut StbString, owner: *mut c_void) -> i32 {
    if id == 23 {
        // SAFETY: this callback runs synchronously with a live output pointer.
        unsafe {
            out.write(StbString::from_string(
                json!({"sessionId":"duplicate"}).to_string(),
            ))
        };
    } else if id == RuntimeActionId::AppendEntry as u32 {
        // SAFETY: Host keeps the boxed owner at a stable address and calls events serially.
        let messages = unsafe { &*owner.cast::<RefCell<Vec<Value>>>() };
        // SAFETY: the plugin lends a JSON string for this callback.
        let Ok(message) = serde_json::from_str(unsafe { args.as_str() }) else {
            return 1;
        };
        messages.borrow_mut().push(message);
    } else {
        return 1;
    }
    0
}

struct Host {
    callbacks: Callbacks,
    messages: Box<RefCell<Vec<Value>>>,
}

impl Host {
    fn new(lifecycle_tail: bool) -> Self {
        let messages = Box::<RefCell<Vec<Value>>>::default();
        let prefix = PluginApi {
            abi_version: RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: size_of::<PluginApi>() as u32,
            free_string: callbacks::free_string,
            register_tool: Some(tool),
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
            user_data: std::ptr::from_ref(messages.as_ref()).cast_mut().cast(),
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
        };
        let rc = if lifecycle_tail {
            let api = HostApi {
                prefix: PluginApi {
                    struct_size: size_of::<HostApi>() as u32,
                    ..prefix
                },
                streaming_and_wake_slots: [0; 2],
                lifecycle: Some(lifecycle),
            };
            rpi_plugin_register(std::ptr::from_ref(&api).cast())
        } else {
            rpi_plugin_register(&prefix)
        };
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

    fn start(&self, name: &str, mut params: Value) -> Step {
        params["__rpi"] = json!({"sessionId":"duplicate"});
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
    }
}

struct Step(Tool, StepHandle);
impl Step {
    fn result(&self) -> Result<Value, String> {
        let start = Instant::now();
        loop {
            match take_step_result!((self.0 .1)(self.1, None, std::ptr::null_mut())) {
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
