//! Win32 process creation and supervision.
//!
//! Each child is assigned to a Job Object configured with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, the Win32 analogue of rustemd's Linux
//! cgroup/process-group boundary. Child handles remain owned here until
//! `try_wait` observes exit, and reader threads forward captured output to the
//! manager without blocking its event loop.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};

use std::os::windows::process::CommandExt;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

use crate::platform::signal::Signal;
use crate::unit::{CgroupLimits, Rlimit, StdioTarget};

pub type ListenHandle = usize;

pub struct SpawnOptions {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub groups: Vec<u32>,
    pub nice: Option<i32>,
    pub umask: Option<u32>,
    pub rlimits: Vec<Rlimit>,
    pub stdout_target: StdioTarget,
    pub stderr_target: StdioTarget,
    pub stdin_null: bool,
    pub notify_socket: Option<PathBuf>,
    pub listen_fds: Vec<ListenHandle>,
    pub cgroup: Option<PathBuf>,
    pub limits: CgroupLimits,
    pub unit_name: String,
}

pub struct Spawned {
    pub pid: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    Exited(i32),
    Signaled(i32),
}

struct Job(HANDLE);
unsafe impl Send for Job {}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct ChildRecord {
    child: Child,
    _job: Job,
    termination_signal: Option<i32>,
}

type OutputEvent = (String, Vec<u8>);

struct OutputBus {
    sender: mpsc::Sender<OutputEvent>,
    receiver: Mutex<mpsc::Receiver<OutputEvent>>,
}

static CHILDREN: OnceLock<Mutex<HashMap<i32, ChildRecord>>> = OnceLock::new();
static OUTPUT: OnceLock<OutputBus> = OnceLock::new();

fn children() -> &'static Mutex<HashMap<i32, ChildRecord>> {
    CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn output_bus() -> &'static OutputBus {
    OUTPUT.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        OutputBus {
            sender,
            receiver: Mutex::new(receiver),
        }
    })
}

pub fn resolve_user(_name: &str) -> Option<(u32, u32, Vec<u32>)> {
    None
}
pub fn resolve_group(_name: &str) -> Option<u32> {
    None
}

pub fn expand_env_argv(argv: &[String], env: &HashMap<String, String>) -> Vec<String> {
    argv.iter()
        .map(|token| expand_env_token(token, env))
        .collect()
}

pub fn expand_env_token(token: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        if chars.peek().is_some_and(|(_, c)| *c == '{') {
            chars.next();
            let mut name = String::new();
            let mut closed = false;
            for (_, c) in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                name.push(c);
            }
            if !closed {
                out.push_str("${");
                out.push_str(&name);
                break;
            }
            out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
        } else {
            let mut name = String::new();
            while let Some((_, c)) = chars.peek() {
                if !(c.is_ascii_alphanumeric() || *c == '_') {
                    break;
                }
                name.push(*c);
                chars.next();
            }
            if name.is_empty() {
                out.push('$');
            } else {
                out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
            }
        }
    }
    out
}

pub fn spawn(opts: &SpawnOptions) -> std::io::Result<Spawned> {
    if opts.argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty command",
        ));
    }
    if opts.uid.is_some() || opts.gid.is_some() || !opts.groups.is_empty() {
        return Err(unsupported("User=/Group="));
    }
    if opts.nice.is_some() || opts.umask.is_some() || !opts.rlimits.is_empty() {
        return Err(unsupported("Nice=/UMask=/Limit*="));
    }
    if opts.notify_socket.is_some() {
        return Err(unsupported("Type=notify"));
    }

    let mut command = Command::new(&opts.argv[0]);
    command.args(&opts.argv[1..]);
    command.envs(
        opts.env
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    if let Some(cwd) = &opts.cwd {
        command.current_dir(cwd);
    }
    if opts.stdin_null {
        command.stdin(Stdio::null());
    }
    configure_stdio(&mut command, &opts.stdout_target, true)?;
    configure_stdio(&mut command, &opts.stderr_target, false)?;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);

    let mut child = command.spawn()?;
    let pid = child.id() as i32;
    let job = match create_job(&opts.limits) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            return Err(error);
        }
    };
    let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
    if assigned == 0 {
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        return Err(error);
    }
    if let Err(error) = resume_primary_thread(pid as u32) {
        let _ = child.kill();
        return Err(error);
    }

    if let Some(stdout) = child.stdout.take() {
        forward_output(opts.unit_name.clone(), stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        forward_output(opts.unit_name.clone(), stderr);
    }
    children().lock().unwrap().insert(
        pid,
        ChildRecord {
            child,
            _job: job,
            termination_signal: None,
        },
    );
    Ok(Spawned { pid })
}

fn unsupported(directive: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{directive} is not supported by the Windows manager MVP"),
    )
}

fn configure_stdio(
    command: &mut Command,
    target: &StdioTarget,
    stdout: bool,
) -> std::io::Result<()> {
    let stdio = match target {
        StdioTarget::Journal | StdioTarget::Inherit => Stdio::piped(),
        StdioTarget::Discard => Stdio::null(),
        StdioTarget::File(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            Stdio::from(file)
        }
    };
    if stdout {
        command.stdout(stdio);
    } else {
        command.stderr(stdio);
    }
    Ok(())
}

fn create_job(limits: &CgroupLimits) -> std::io::Result<Job> {
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if let Some(memory) = limits.memory_max.filter(|value| *value != u64::MAX) {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = memory as usize;
    }
    if let Some(tasks) = limits.tasks_max.filter(|value| *value != u64::MAX) {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = tasks.min(u32::MAX as u64) as u32;
    }
    let ok = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            CloseHandle(handle);
        }
        return Err(error);
    }
    Ok(Job(handle))
}

fn resume_primary_thread(pid: u32) -> std::io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = false;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, FALSE, entry.th32ThreadID) };
            if !thread.is_null() {
                let resumed = unsafe { ResumeThread(thread) };
                unsafe {
                    CloseHandle(thread);
                }
                if resumed != u32::MAX {
                    found = true;
                    break;
                }
            }
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    if found {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn forward_output(unit_name: String, mut reader: impl Read + Send + 'static) {
    let tx = output_bus().sender.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = tx.send((unit_name.clone(), buffer[..n].to_vec()));
                }
                Err(_) => break,
            }
        }
    });
}

pub fn drain_output() -> Vec<(String, Vec<u8>)> {
    let receiver = output_bus().receiver.lock().unwrap();
    receiver.try_iter().collect()
}

pub fn kill_group(group_pid: i32, signal: Signal) -> std::io::Result<()> {
    let mut children = children().lock().unwrap();
    let record = children.get_mut(&group_pid).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("process {group_pid} is not tracked"),
        )
    })?;
    let code = if signal == Signal::SIGKILL { 137 } else { 143 };
    let ok = unsafe { TerminateJobObject(record._job.0, code) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    record.termination_signal = Some(signal.as_raw());
    Ok(())
}

pub fn group_alive(group_pid: i32) -> bool {
    let mut children = children().lock().unwrap();
    children
        .get_mut(&group_pid)
        .is_some_and(|record| record.child.try_wait().ok().flatten().is_none())
}

pub fn reap_children() -> Vec<(i32, ChildExit)> {
    let mut children = children().lock().unwrap();
    let pids: Vec<i32> = children.keys().copied().collect();
    let mut exited = Vec::new();
    for pid in pids {
        let status = children
            .get_mut(&pid)
            .and_then(|record| record.child.try_wait().ok().flatten());
        if let Some(status) = status {
            let record = children.remove(&pid).unwrap();
            let exit = match record.termination_signal {
                Some(signal) => ChildExit::Signaled(signal),
                None => ChildExit::Exited(status.code().unwrap_or(1)),
            };
            exited.push((pid, exit));
        }
    }
    exited
}

pub fn set_subreaper() {}
