//! Raw process creation: pipes, Job Objects, CreateProcess(AsUser)W.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{PSID, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::process::KillTree;

const PIPE_BUFFER: u32 = 64 * 1024;

/// Serializes the short window in which our child-side pipe ends are inheritable.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

/// An anonymous pipe: (read end, write end), both non-inheritable.
pub(crate) fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: out-pointers are valid; the handles are owned below.
    if unsafe { CreatePipe(&mut read, &mut write, null(), PIPE_BUFFER) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: freshly created handles.
    unsafe { Ok((OwnedHandle::from_raw_handle(read as _), OwnedHandle::from_raw_handle(write as _))) }
}

/// A Job Object that kills every member when its last handle closes.
pub(crate) struct Job {
    handle: OwnedHandle,
}

impl Job {
    pub(crate) fn new(sandboxed: bool) -> io::Result<Job> {
        // SAFETY: anonymous job with default security.
        let h = unsafe { CreateJobObjectW(null(), null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: freshly created handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(h as _) };
        // SAFETY: zero is a valid bit pattern for this plain C struct.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        // SAFETY: correct struct and size for the info class.
        let ok = unsafe {
            SetInformationJobObject(
                raw(&handle),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if sandboxed {
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
            };
            // SAFETY: correct struct and size for the info class. Best effort.
            let ok = unsafe {
                SetInformationJobObject(
                    raw(&handle),
                    JobObjectBasicUIRestrictions,
                    (&ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
                    std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                )
            };
            if ok == 0 {
                tracing::debug!(error = %io::Error::last_os_error(), "could not apply job UI restrictions");
            }
        }
        Ok(Job { handle })
    }
}

impl KillTree for Job {
    fn kill_tree(&self) {
        // SAFETY: valid job handle. Failure (e.g. already empty) is harmless.
        unsafe { TerminateJobObject(raw(&self.handle), 1) };
    }
}

/// Everything CreateProcess needs.
pub(crate) struct ProcessSpec<'a> {
    pub application: &'a [u16],
    pub command_line: Vec<u16>,
    pub environment: &'a [u16],
    pub cwd: &'a [u16],
    /// Primary token for CreateProcessAsUserW (restricted-token backend).
    pub token: Option<&'a OwnedHandle>,
    /// AppContainer SID (AppContainer backend).
    pub appcontainer_sid: Option<PSID>,
    pub stdin: &'a OwnedHandle,
    pub stdout: &'a OwnedHandle,
    pub stderr: &'a OwnedHandle,
}

/// A started (and resumed) process.
pub(crate) struct Started {
    pub process: OwnedHandle,
    pub pid: u32,
}

struct AttributeList {
    buf: Vec<u64>,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<AttributeList> {
        let mut size = 0usize;
        // SAFETY: size query.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u64; size.div_ceil(8)];
        // SAFETY: the buffer holds `size` bytes.
        if unsafe { InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(AttributeList { buf })
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must stay alive and unmoved until the process has been created.
    unsafe fn set(&mut self, attribute: u32, value: *const core::ffi::c_void, size: usize) -> io::Result<()> {
        if UpdateProcThreadAttribute(self.ptr(), 0, attribute as usize, value, size, null_mut(), null()) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

/// Creates the process suspended, assigns it to `job`, then resumes it. Only the three stdio
/// handles are inherited (PROC_THREAD_ATTRIBUTE_HANDLE_LIST).
pub(crate) fn create_process(mut spec: ProcessSpec<'_>, job: &Job) -> io::Result<Started> {
    let inherit = [raw(spec.stdin), raw(spec.stdout), raw(spec.stderr)];
    let attr_count = if spec.appcontainer_sid.is_some() { 2 } else { 1 };
    let mut attrs = AttributeList::new(attr_count)?;
    let mut caps = SECURITY_CAPABILITIES {
        AppContainerSid: null_mut(),
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    // SAFETY: `inherit` and `caps` live until after CreateProcess returns.
    unsafe {
        attrs.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherit.as_ptr().cast(), std::mem::size_of_val(&inherit))?;
        if let Some(sid) = spec.appcontainer_sid {
            caps.AppContainerSid = sid;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                (&caps as *const SECURITY_CAPABILITIES).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
            )?;
        }
    }

    // SAFETY: zero is a valid bit pattern for STARTUPINFOEXW.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = inherit[0];
    si.StartupInfo.hStdOutput = inherit[1];
    si.StartupInfo.hStdError = inherit[2];
    si.lpAttributeList = attrs.ptr();

    // SAFETY: zero is a valid bit pattern for PROCESS_INFORMATION.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT;

    let created = {
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_inheritable(&inherit, true)?;
        // SAFETY: every pointer references a live, NUL-terminated buffer; `si` is a valid
        // STARTUPINFOEXW (EXTENDED_STARTUPINFO_PRESENT); the command line buffer is mutable.
        let ok = unsafe {
            match spec.token {
                Some(token) => CreateProcessAsUserW(
                    raw(token),
                    spec.application.as_ptr(),
                    spec.command_line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    flags,
                    spec.environment.as_ptr().cast(),
                    spec.cwd.as_ptr(),
                    &si.StartupInfo,
                    &mut pi,
                ),
                None => CreateProcessW(
                    spec.application.as_ptr(),
                    spec.command_line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    flags,
                    spec.environment.as_ptr().cast(),
                    spec.cwd.as_ptr(),
                    &si.StartupInfo,
                    &mut pi,
                ),
            }
        };
        let err = io::Error::last_os_error();
        let _ = set_inheritable(&inherit, false);
        if ok == 0 {
            Err(err)
        } else {
            Ok(())
        }
    };
    created?;
    drop(attrs);

    // SAFETY: CreateProcess returned fresh handles that we now own.
    let process = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };
    // SAFETY: as above.
    let thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };

    // SAFETY: valid job and process handles.
    if unsafe { AssignProcessToJobObject(raw(&job.handle), raw(&process)) } == 0 {
        let err = io::Error::last_os_error();
        // SAFETY: valid process handle; the process never ran.
        unsafe { TerminateProcess(raw(&process), 1) };
        return Err(io::Error::new(err.kind(), format!("failed to assign process to job object: {err}")));
    }
    // SAFETY: valid thread handle of the suspended main thread.
    if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
        let err = io::Error::last_os_error();
        job.kill_tree();
        return Err(err);
    }
    Ok(Started { process, pid: pi.dwProcessId })
}

fn set_inheritable(handles: &[HANDLE], inherit: bool) -> io::Result<()> {
    for &h in handles {
        let flags = if inherit { HANDLE_FLAG_INHERIT } else { 0 };
        // SAFETY: valid handles owned by the caller.
        if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, flags) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Blocks until `process` exits and returns its exit code.
pub(crate) fn wait_exit(process: &OwnedHandle) -> Option<i32> {
    // SAFETY: valid process handle.
    if unsafe { WaitForSingleObject(raw(process), INFINITE) } != WAIT_OBJECT_0 {
        return None;
    }
    let mut code = 0u32;
    // SAFETY: valid process handle and out-pointer.
    if unsafe { GetExitCodeProcess(raw(process), &mut code) } == 0 {
        return None;
    }
    Some(code as i32)
}
