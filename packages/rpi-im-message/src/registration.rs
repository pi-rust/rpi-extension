use crate::{lifecycle, Command, ReplyProgress, Server, StartupContext};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) const SLOTS: usize = 128;
const LEGACY_SLOT: usize = SLOTS;
static REGISTRATIONS: Mutex<[Option<Arc<Registration>>; SLOTS + 1]> =
    Mutex::new([const { None }; SLOTS + 1]);

pub(crate) struct Registration {
    pub(crate) host: Option<StartupContext>,
    pub(crate) servers: Mutex<HashMap<String, Arc<Server>>>,
    pub(crate) reply_lock: Mutex<()>,
    pub(crate) active_progress: Mutex<Option<Arc<ReplyProgress>>>,
    stopping: AtomicBool,
    workers: Mutex<Vec<Arc<Worker>>>,
}

impl Registration {
    pub(crate) fn new(host: Option<StartupContext>) -> Self {
        Self {
            host,
            servers: Mutex::default(),
            reply_lock: Mutex::default(),
            active_progress: Mutex::default(),
            stopping: AtomicBool::new(false),
            workers: Mutex::default(),
        }
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(crate) fn spawn(
        &self,
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        run: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Worker>, String> {
        let mut workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        if self.is_stopping() {
            return Err("IM message registration is shutting down".into());
        }
        workers.retain(|worker| !worker.join_until(Instant::now()));
        let join = thread::Builder::new()
            .spawn(run)
            .map_err(|error| format!("failed to start IM server worker: {error}"))?;
        let worker = Arc::new(Worker {
            commands,
            join: Mutex::new(Some(join)),
        });
        workers.push(worker.clone());
        Ok(worker)
    }

    pub(crate) fn begin(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        // Serialize with worker admission. Send Stop through the provider's
        // normal command path, including for startup or detached tool stops.
        let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        for worker in workers.iter() {
            let (reply, _) = mpsc::channel();
            let _ = worker.commands.send(Command::Stop { reply });
        }
    }

    pub(crate) fn shutdown(&self, timeout: Duration) -> bool {
        self.begin();
        self.servers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let workers = self
            .workers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let deadline = Instant::now() + timeout;
        let mut complete = true;
        for worker in workers {
            complete &= worker.join_until(deadline);
        }
        complete
    }
}

pub(crate) struct Worker {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    pub(crate) fn join_until(&self, deadline: Instant) -> bool {
        loop {
            {
                let mut join = self.join.lock().unwrap_or_else(|p| p.into_inner());
                match join.as_ref() {
                    None => return true,
                    Some(handle) if handle.is_finished() => {
                        let _ = join.take().expect("worker handle exists").join();
                        return true;
                    }
                    Some(_) => {}
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}

fn registrations() -> std::sync::MutexGuard<'static, [Option<Arc<Registration>>; SLOTS + 1]> {
    REGISTRATIONS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn token(registration: &Arc<Registration>) -> *mut c_void {
    // Only an identity for lookups. The host round-trips it without reading it.
    Arc::as_ptr(registration).cast_mut().cast()
}

pub(crate) fn install(
    registration: Arc<Registration>,
    lifecycle: Option<lifecycle::Register>,
) -> Result<usize, i32> {
    let owner = token(&registration);
    let slot = {
        let mut entries = registrations();
        let range = if lifecycle.is_some() {
            0..SLOTS
        } else {
            LEGACY_SLOT..LEGACY_SLOT + 1
        };
        let slot = range
            .into_iter()
            .find(|&slot| entries[slot].is_none())
            .ok_or(1)?;
        entries[slot] = Some(registration);
        slot
    };
    if let Some(register) = lifecycle {
        let code = register(begin, finish, owner);
        if code != 0 {
            registrations()[slot] = None;
            return Err(code);
        }
    }
    Ok(slot)
}

pub(crate) fn discard_failed_legacy(slot: usize) {
    if slot == LEGACY_SLOT {
        registrations()[slot] = None;
    }
}

pub(crate) fn tool(slot: usize) -> Option<Arc<Registration>> {
    registrations()[slot].clone()
}

pub(crate) fn callback(owner: *mut c_void) -> Option<Arc<Registration>> {
    registrations()
        .iter()
        .flatten()
        .find(|item| token(item) == owner)
        .cloned()
}

extern "C" fn begin(owner: *mut c_void) {
    if let Some(registration) = callback(owner) {
        registration.begin();
    }
}

extern "C" fn finish(owner: *mut c_void) -> i32 {
    let Some(registration) = callback(owner) else {
        return 0;
    };
    // A timed-out tool stop can leave a worker draining its Tokio runtime.
    // The loader must retain this image and its context until process exit
    // if we cannot join all workers here.
    if !registration.shutdown(crate::RUNTIME_SHUTDOWN_TIMEOUT) {
        return 1;
    }
    let mut entries = registrations();
    if let Some(entry) = entries.iter_mut().find(|entry| {
        entry
            .as_ref()
            .is_some_and(|item| Arc::ptr_eq(item, &registration))
    }) {
        *entry = None;
    }
    0
}
