//! Win32 console/service shutdown source.

use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};

use crate::platform::signal::Signal;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn console_handler(kind: u32) -> i32 {
    if matches!(
        kind,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        SHUTDOWN.store(true, Ordering::SeqCst);
        TRUE
    } else {
        FALSE
    }
}

pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

pub struct SignalSource;
impl SignalSource {
    pub fn new() -> Option<Self> {
        // SCM processes may not have a console. Console registration is
        // best-effort, but the source must still exist so SCM controls queued
        // through `request_shutdown` are observed by the manager loop.
        unsafe {
            SetConsoleCtrlHandler(Some(console_handler), TRUE);
        }
        Some(Self)
    }

    pub fn read(&self) -> Vec<Signal> {
        if SHUTDOWN.swap(false, Ordering::SeqCst) {
            vec![Signal::SIGTERM]
        } else {
            Vec::new()
        }
    }
}
