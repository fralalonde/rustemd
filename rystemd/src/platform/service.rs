//! Native Windows Service Control Manager hosting and installation.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::{
    OnceLock,
    atomic::{AtomicPtr, Ordering},
};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GetLastError};
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW,
    RegisterServiceCtrlHandlerExW, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE,
    SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
    SetServiceStatus, StartServiceCtrlDispatcherW,
};

static SERVICE_NAME: OnceLock<String> = OnceLock::new();
static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub fn service_image_path(executable: &Path) -> String {
    format!("\"{}\" service run", executable.display())
}

fn service_image_path_for(executable: &Path, name: &str) -> String {
    format!("{} --name {name}", service_image_path(executable))
}

pub fn install(name: &str, display_name: &str, manual: bool) -> Result<(), String> {
    validate_name(name)?;
    let paths = crate::paths::Paths::system();
    for directory in paths
        .unit_path
        .iter()
        .chain([&paths.config_dir, &paths.runtime_dir])
    {
        std::fs::create_dir_all(directory).map_err(|error| {
            format!(
                "create Windows manager directory {}: {error}",
                directory.display()
            )
        })?;
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let image = wide(service_image_path_for(&executable, name));
    let name_w = wide(name);
    let display_w = wide(display_name);
    let manager = unsafe {
        OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CREATE_SERVICE,
        )
    };
    if manager.is_null() {
        return Err(last_error("OpenSCManagerW"));
    }
    let service = unsafe {
        CreateServiceW(
            manager,
            name_w.as_ptr(),
            display_w.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            if manual {
                SERVICE_DEMAND_START
            } else {
                SERVICE_AUTO_START
            },
            SERVICE_ERROR_NORMAL,
            image.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    unsafe {
        CloseServiceHandle(manager);
    }
    if service.is_null() {
        return Err(last_error("CreateServiceW"));
    }
    unsafe {
        CloseServiceHandle(service);
    }
    Ok(())
}

pub fn uninstall(name: &str) -> Result<(), String> {
    validate_name(name)?;
    let name_w = wide(name);
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(last_error("OpenSCManagerW"));
    }
    let service = unsafe { OpenServiceW(manager, name_w.as_ptr(), DELETE) };
    unsafe {
        CloseServiceHandle(manager);
    }
    if service.is_null() {
        return Err(last_error("OpenServiceW"));
    }
    let deleted = unsafe { DeleteService(service) };
    unsafe {
        CloseServiceHandle(service);
    }
    if deleted == 0 {
        return Err(last_error("DeleteService"));
    }
    Ok(())
}

pub fn run_dispatcher(name: &str) -> Result<(), String> {
    validate_name(name)?;
    SERVICE_NAME
        .set(name.to_string())
        .map_err(|_| "service dispatcher already initialized".to_string())?;
    let mut name_w = wide(name);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name_w.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if ok == 0 {
        return Err(last_error("StartServiceCtrlDispatcherW"));
    }
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let name = SERVICE_NAME.get().map(String::as_str).unwrap_or("rystemd");
    let name_w = wide(name);
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name_w.as_ptr(), Some(control_handler), std::ptr::null())
    };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle, Ordering::SeqCst);
    report(handle, SERVICE_START_PENDING, 0, 5_000);
    let code = crate::daemon::run_daemon_with_ready(false, false, || {
        report(
            handle,
            SERVICE_RUNNING,
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            0,
        );
    });
    let mut stopped = status(SERVICE_STOPPED, 0, 0);
    if code != 0 {
        stopped.dwWin32ExitCode = code as u32;
    }
    unsafe {
        SetServiceStatus(handle, &stopped);
    }
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        crate::platform::signals::request_shutdown();
        let handle = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
        if !handle.is_null() {
            report(handle, SERVICE_STOP_PENDING, 0, 30_000);
        }
    }
    ERROR_SUCCESS
}

fn report(handle: SERVICE_STATUS_HANDLE, state: u32, accepted: u32, wait_hint: u32) {
    let status = status(state, accepted, wait_hint);
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

fn status(state: u32, accepted: u32, wait_hint: u32) -> SERVICE_STATUS {
    SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: ERROR_SUCCESS,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: wait_hint,
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        Err("service name must contain only ASCII letters, digits, '.', '_' or '-'".into())
    } else {
        Ok(())
    }
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error(operation: &str) -> String {
    let code = unsafe { GetLastError() };
    format!(
        "{operation} failed: {} (win32 error {code})",
        std::io::Error::from_raw_os_error(code as i32)
    )
}
