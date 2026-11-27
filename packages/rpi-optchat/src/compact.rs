//! Model-written summary lines, the way upstream OptChat compresses its tree.
//!
//! A plugin cannot call a model directly: the ABI has no model entrypoint. The
//! engine here is a child `rpi -p` process with no session, no extensions and no
//! tools, the same shape `rpi-orchestrator` uses for its subagents. It is
//! optional and off unless asked for, because every line costs a model call:
//!
//! - `RPI_OPTCHAT_COMPACT=auto` enables it for a session, `/optchat compact on|off`
//!   toggles it at runtime and persists the choice with the session;
//! - `RPI_OPTCHAT_COMPACT_TIMEOUT` bounds one call in seconds (default 60);
//! - `RPI_OPTCHAT_RPI_BIN` overrides the executable (tests, unusual installs).
//!
//! When a call fails, times out, or the host cannot report the session's model
//! binding, the extractive line already in the view stays. The log is the source
//! of truth; a compressed line is a cache that can always be rebuilt or dropped.

use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Session custom-entry type holding one model-written line.
pub(crate) const NODE_TYPE: &str = "optchat-node";
/// Session custom-entry type holding plugin settings.
pub(crate) const CONFIG_TYPE: &str = "optchat-config";
/// Bytes one line may occupy; a longer reply is cut, never refused.
const LINE: usize = super::memory::NODE;
/// Jobs waiting at once. A full queue drops new work instead of delaying a turn.
const DEPTH: usize = 8;

struct Job {
    generation: u64,
    l: u32,
    i: u64,
    source: String,
    binding: Binding,
}

pub(crate) struct Done {
    pub generation: u64,
    pub l: u32,
    pub i: u64,
    pub text: String,
}

#[derive(Clone, Default)]
pub(crate) struct Binding {
    provider: Option<String>,
    model: Option<String>,
    cwd: Option<PathBuf>,
}

impl Binding {
    pub(crate) fn from_context(context: &Value) -> Option<Self> {
        let string = |value: &Value| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        };
        let provider = string(&context["model"]["provider"]);
        let model = string(&context["model"]["id"]);
        let cwd = string(&context["cwd"])
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        (provider.is_some() && model.is_some()).then_some(Self {
            provider,
            model,
            cwd,
        })
    }
}

pub(crate) struct Engine {
    queue: SyncSender<Job>,
    done: Receiver<Done>,
    // Includes finished jobs until they are drained, so the completion queue
    // is bounded by the same admission budget as the work queue.
    in_flight: HashSet<(u64, u32, u64)>,
    worker: JoinHandle<()>,
}

impl Engine {
    pub(crate) fn new(
        stopping: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
    ) -> std::io::Result<Self> {
        let (queue, jobs) = sync_channel(DEPTH);
        let (sender, done) = channel();
        let worker = thread::Builder::new()
            .name("rpi-optchat-compactor".into())
            .spawn(move || worker(jobs, sender, stopping, generation))?;
        Ok(Self {
            queue,
            done,
            in_flight: HashSet::new(),
            worker,
        })
    }

    pub(crate) fn enqueue(
        &mut self,
        generation: u64,
        l: u32,
        i: u64,
        source: String,
        binding: Binding,
    ) {
        let key = (generation, l, i);
        if self.in_flight.len() <= DEPTH
            && !self.in_flight.contains(&key)
            && self
                .queue
                .try_send(Job {
                    generation,
                    l,
                    i,
                    source,
                    binding,
                })
                .is_ok()
        {
            self.in_flight.insert(key);
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<Done> {
        let done: Vec<_> = self.done.try_iter().collect();
        for result in &done {
            self.in_flight
                .remove(&(result.generation, result.l, result.i));
        }
        done
    }

    pub(crate) fn finish(self) {
        drop(self.queue);
        let _ = self.worker.join();
    }
}

/// Whether the user enables model compaction. `RPI_OPTCHAT_COMPACT` is the launch
/// default; the session's own setting wins once it exists.
pub(crate) fn env_enabled() -> bool {
    matches!(
        std::env::var("RPI_OPTCHAT_COMPACT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "auto" | "on" | "yes" | "1" | "true"
    )
}

fn timeout() -> Duration {
    let seconds = std::env::var("RPI_OPTCHAT_COMPACT_TIMEOUT")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| (1..=600).contains(seconds))
        .unwrap_or(60);
    Duration::from_secs(seconds)
}

fn executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RPI_OPTCHAT_RPI_BIN") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let name = if cfg!(windows) { "rpi.exe" } else { "rpi" };
    let path = std::env::current_exe().ok()?;
    // `current_exe` is the host binary itself when the plugin runs in-process.
    if path.file_name().and_then(|value| value.to_str()) == Some(name) && path.is_file() {
        return Some(path);
    }
    which(name)
}

/// `PATH` lookup without spawning a helper process.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The prompt one call answers: merge two lines into one, under the byte ceiling.
fn prompt(job: &Job) -> String {
    format!(
        "You are compressing one step of a chat memory tree.\n\
         Merge the two adjacent summary lines below into ONE line of at most {LINE} bytes \
         (about 70 words).\n\
         Keep the user's decisions, corrections, names, numbers, ids, paths, errors and \
         outcomes; drop repetition and tool detail that the log can be zoomed for. Never \
         invent anything, never answer the content, and never narrate the compression.\n\
         Output only the line, without an id+n| head and without code fences.\n\n\
         <input>\n{}\n</input>",
        job.source
    )
}

/// Take the line out of a child reply: one paragraph, no head, within the ceiling.
fn parse_reply(stdout: &str) -> Option<String> {
    let mut text = String::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        // A reply that echoes the view keeps the head it was told to drop.
        let line = line
            .split_once('|')
            .filter(|(head, _)| {
                head.split_once('+')
                    .map(|(id, n)| {
                        id.chars().all(|c| c.is_ascii_digit())
                            && n.chars().all(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false)
            })
            .map(|(_, rest)| rest.trim())
            .unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(super::memory::clip(&super::memory::flat(&text), LINE))
}

fn command(exe: &Path, binding: &Binding) -> Command {
    let mut command = Command::new(exe);
    command.args(["-p", "--no-session", "--no-extensions", "--no-tools"]);
    if let Some(cwd) = &binding.cwd {
        command.current_dir(cwd);
    }
    if let Some(provider) = &binding.provider {
        command.arg("--provider").arg(provider);
    }
    if let Some(model) = &binding.model {
        command.arg("--model").arg(model);
    }
    command
}

/// Anonymous files keep child I/O from blocking shutdown, including when a
/// descendant keeps a standard stream open. Poll the child directly so the
/// library never installs a process-wide signal handler.
fn run(exe: &Path, job: &Job, cancelled: impl Fn() -> bool) -> Result<String, String> {
    let mut input = tempfile::tempfile().map_err(|error| error.to_string())?;
    input
        .write_all(prompt(job).as_bytes())
        .map_err(|error| error.to_string())?;
    input.rewind().map_err(|error| error.to_string())?;
    let mut output = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut errors = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut child = command(exe, &job.binding)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(
            output.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(
            errors.try_clone().map_err(|error| error.to_string())?,
        ))
        .spawn()
        .map_err(|error| format!("cannot start the compactor: {error}"))?;
    let limit = timeout();
    let deadline = Instant::now() + limit;
    let status = loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => break status,
            Err(error) => format!("cannot wait for the compactor: {error}"),
            Ok(None) if cancelled() => "compaction cancelled".to_string(),
            Ok(None) if Instant::now() >= deadline => {
                format!("compaction took longer than {}s", limit.as_secs())
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        if let Err(error) = child.kill() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                return Err(format!("{failure}; could not kill compactor: {error}"));
            }
        } else {
            let reap_deadline = Instant::now() + Duration::from_secs(2);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < reap_deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        return Err(failure);
    };
    let read = |file: &mut std::fs::File| -> Result<String, String> {
        let mut bytes = Vec::new();
        file.rewind()
            .and_then(|_| file.take(64 * 1024).read_to_end(&mut bytes))
            .map_err(|error| format!("cannot read the compactor output: {error}"))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    if !status.success() {
        let stderr = read(&mut errors)?;
        return Err(format!(
            "compactor exited with {status}: {}",
            stderr.trim().lines().next().unwrap_or("")
        ));
    }
    parse_reply(&read(&mut output)?).ok_or_else(|| "compactor returned no usable line".to_string())
}

fn worker(
    jobs: Receiver<Job>,
    done: Sender<Done>,
    stopping: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
) {
    let exe = executable().and_then(|path| path.canonicalize().ok());
    while let Ok(job) = jobs.recv() {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let cancelled = || {
            stopping.load(Ordering::Acquire) || generation.load(Ordering::Acquire) != job.generation
        };
        let text = match exe.as_ref().filter(|_| !cancelled()) {
            Some(exe) => run(exe, &job, cancelled).unwrap_or_else(|error| {
                if !cancelled() {
                    super::debug(&format!(
                        "compaction {}+{}: {error}",
                        job.i << job.l,
                        1 << job.l
                    ));
                }
                String::new()
            }),
            None => String::new(),
        };
        if done
            .send(Done {
                generation: job.generation,
                l: job.l,
                i: job.i,
                text,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(source: &str) -> Job {
        Job {
            generation: 0,
            binding: Binding::default(),
            l: 1,
            i: 0,
            source: source.to_string(),
        }
    }

    #[test]
    fn reply_keeps_one_line_and_drops_a_copied_head() {
        assert_eq!(
            parse_reply("0+2|merged words\n").as_deref(),
            Some("merged words")
        );
        assert_eq!(
            parse_reply("first part\nsecond part\n").as_deref(),
            Some("first part second part")
        );
        assert_eq!(parse_reply("```\na line\n```\n").as_deref(), Some("a line"));
        assert_eq!(parse_reply("   \n"), None);
        let long = "x".repeat(LINE * 2);
        let line = parse_reply(&long).expect("a line");
        assert!(line.len() <= LINE, "{}", line.len());
    }

    #[test]
    fn prompt_states_the_ceiling_and_the_input() {
        let text = prompt(&job("user: hello; talk: hi"));
        assert!(text.contains(&format!("at most {LINE} bytes")), "{text}");
        assert!(text.contains("user: hello; talk: hi"), "{text}");
    }

    #[test]
    fn command_is_ephemeral_and_tool_free() {
        let command = command(Path::new("/usr/bin/rpi"), &Binding::default());
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.starts_with(&["-p".to_string(), "--no-session".to_string()]));
        assert!(args.contains(&"--no-extensions".to_string()));
        assert!(args.contains(&"--no-tools".to_string()));
    }

    #[test]
    fn env_default_is_off_unless_asked_for() {
        let previous = std::env::var("RPI_OPTCHAT_COMPACT").ok();
        std::env::remove_var("RPI_OPTCHAT_COMPACT");
        assert!(!env_enabled());
        std::env::set_var("RPI_OPTCHAT_COMPACT", "auto");
        assert!(env_enabled());
        match previous {
            Some(value) => std::env::set_var("RPI_OPTCHAT_COMPACT", value),
            None => std::env::remove_var("RPI_OPTCHAT_COMPACT"),
        }
    }

    /// Write an executable stand-in for the host binary in its own directory.
    #[cfg(unix)]
    fn script(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("rpi");
        std::fs::write(&path, body).expect("script");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        path
    }

    /// Linux reports `ETXTBSY` when another test thread forks between this
    /// script's write and its exec. The script itself is valid, so a short
    /// bounded retry keeps the check deterministic without hiding real spawn
    /// failures: only the busy-exec error is retried.
    #[cfg(unix)]
    fn run_script(path: &PathBuf, job: &Job) -> Result<String, String> {
        let mut delay = std::time::Duration::from_millis(10);
        for _ in 0..40 {
            match run(path, job, || false) {
                Err(error) if error.contains("Text file busy") => {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(std::time::Duration::from_millis(200));
                }
                result => return result,
            }
        }
        run(path, job, || false)
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_answers_produces_a_line() {
        let script = script(
            "optchat-compact",
            "#!/bin/sh\ncat > /dev/null\necho '0+2|the merged line'\n",
        );
        let answer = run_script(&script, &job("two halves")).expect("a line");
        assert_eq!(answer, "the merged line");
        let _ = std::fs::remove_dir_all(script.parent().expect("parent"));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_fails_is_reported_not_panicked() {
        let script = script("optchat-fail", "#!/bin/sh\necho 'no key' >&2\nexit 3\n");
        let error = run_script(&script, &job("two halves")).expect_err("failure");
        assert!(error.contains("exited with"), "{error}");
        assert!(error.contains("no key"), "{error}");
        let _ = std::fs::remove_dir_all(script.parent().expect("parent"));
    }
}
