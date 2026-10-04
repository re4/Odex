//! Windows backends.
//!
//! # Process model (all policies)
//!
//! Children are created with raw `CreateProcessW` / `CreateProcessAsUserW` using
//! `CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT`, anonymous pipes for
//! stdin/stdout/stderr (only those three handles are inherited, via
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), then assigned to a fresh Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` before the main thread is resumed. Killing = terminating
//! the job, which takes down every descendant; dropping the handle does the same. Sandboxed jobs
//! also get UI restrictions (no clipboard, desktop switching, system parameter changes, logoff).
//! The program is resolved against the child's `PATH`/`PATHEXT` (never the working directory);
//! batch files run through `cmd.exe /d /s /c`. For `cmd.exe /c` the command string is passed
//! verbatim; other arguments are quoted per the MSVCRT rules.
//!
//! # Restricted-token backend (default)
//!
//! * A stable Odex "write" SID `S-1-9-<hash of "odex.sandbox.write">` is used (authority 9 =
//!   resource manager, so it never matches a real account; capability SIDs are rejected by
//!   `CreateRestrictedToken` as restricting SIDs).
//! * Each writable root (and the private sandbox temp dir) gets an inheritable
//!   (`OBJECT_INHERIT | CONTAINER_INHERIT`) allow ACE for that SID with read/write/execute/delete
//!   (no `WRITE_DAC`/`WRITE_OWNER`). The DACL is inspected first and only rewritten when the ACE
//!   is missing; results are cached per process. Drive roots, the profile directory and system
//!   directories are refused.
//! * The child runs with a **write-restricted** token (see [`token::create_write_restricted_token`]):
//!   reads are checked against the user's normal SIDs, but every write must also be granted to
//!   one of the restricting SIDs — the Odex SID, the logon SID, or `RESTRICTED`. Ordinary user
//!   files never grant those, so writes outside the roots fail with "Access is denied".
//! * `TEMP`/`TMP` point at the sandbox temp dir, and PowerShell's module analysis cache is
//!   redirected there.
//! * Network is **not** isolated; the engine gates network commands through approvals.
//!
//! # AppContainer backend
//!
//! Uses the SID of the AppContainer name `odex.sandbox` (derived with
//! `DeriveAppContainerSidFromAppContainerName`; no profile is registered) and launches with
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` and **no** capabilities, so the network is
//! isolated (including loopback). The container SID is granted read/write on the writable roots
//! and temp dir and read/execute on the working directory. Everything else is only readable when
//! it grants `ALL APPLICATION PACKAGES` (system directories do; the user profile does not), so
//! toolchains installed in the profile (cargo, nvm, scoop, ...) generally fail to start.

mod acl;
mod cmdline;
mod sid;
mod spawn;
mod token;

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Globalization::{GetOEMCP, MultiByteToWideChar};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{FreeSid, PSID};

use crate::process::Launched;
use crate::{existing_roots, Backend, ExecRequest, SandboxError, SandboxStatus};
use cmdline::EnvMap;
use sid::OwnedSid;

const WRITE_SID_NAME: &str = "odex.sandbox.write";
const APPCONTAINER_NAME: &str = "odex.sandbox";

const RESTRICTED_NETWORK_WARNING: &str =
    "Network is not isolated by the restricted-token backend; network commands are gated by approvals.";
const APPCONTAINER_WARNING: &str = "AppContainer sandbox: the network is isolated, but programs cannot read files outside system locations and the workspace (toolchains in your user profile may fail to start).";

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_os(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The stable SID that the restricted token needs for writes.
fn write_sid() -> Result<&'static OwnedSid, SandboxError> {
    static SID: OnceLock<Result<OwnedSid, String>> = OnceLock::new();
    SID.get_or_init(|| named_sid(WRITE_SID_NAME).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| SandboxError::Unavailable(format!("could not build the Odex sandbox SID: {e}")))
}

/// 64-bit FNV-1a; stable across Rust versions (unlike `DefaultHasher`).
fn fnv1a(seed: u64, data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `S-1-9-<4 words derived from name>`: authority 9 is reserved for resource managers, so the
/// SID cannot collide with accounts or groups. (Capability SIDs are not accepted as restricting
/// SIDs by `CreateRestrictedToken`, so one is not used here.)
fn named_sid(name: &str) -> io::Result<OwnedSid> {
    let a = fnv1a(1, name.as_bytes());
    let b = fnv1a(2, name.as_bytes());
    let subs = [(a >> 32) as u32, a as u32, (b >> 32) as u32, b as u32];
    OwnedSid::from_parts([0, 0, 0, 0, 0, 9], &subs)
}

fn appcontainer_sid() -> Result<&'static OwnedSid, SandboxError> {
    static SID: OnceLock<Result<OwnedSid, String>> = OnceLock::new();
    SID.get_or_init(|| {
        let name = wide_str(APPCONTAINER_NAME);
        let display = wide_str("Odex Sandbox");
        // Registering the profile creates the container's kernel object namespace
        // (\Sessions\..\AppContainerNamedObjects\<sid>); without it CreateProcess fails. It is
        // persistent and idempotent — ERROR_ALREADY_EXISTS (0x800700B7) just means it exists.
        let mut psid: PSID = null_mut();
        // SAFETY: valid NUL-terminated names; the SID is freed with FreeSid.
        unsafe {
            let hr =
                CreateAppContainerProfile(name.as_ptr(), display.as_ptr(), display.as_ptr(), null_mut(), 0, &mut psid);
            const ALREADY_EXISTS: i32 = 0x8007_00B7u32 as i32;
            if hr >= 0 && !psid.is_null() {
                let sid = OwnedSid::from_psid(psid).map_err(|e| e.to_string());
                FreeSid(psid);
                return sid;
            }
            if hr != ALREADY_EXISTS {
                tracing::debug!("CreateAppContainerProfile returned {hr:#x}; deriving the SID instead");
            }
            let hr = DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut psid);
            if hr < 0 || psid.is_null() {
                return Err(format!("DeriveAppContainerSidFromAppContainerName failed (HRESULT {hr:#x})"));
            }
            let sid = OwnedSid::from_psid(psid).map_err(|e| e.to_string());
            FreeSid(psid);
            sid
        }
    })
    .as_ref()
    .map_err(|e| SandboxError::Unavailable(format!("AppContainer unavailable: {e}")))
}

fn restricted_token_probe() -> &'static Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE.get_or_init(|| {
        let sid = write_sid().map_err(|e| e.to_string())?;
        token::create_write_restricted_token(sid).map(|_| ()).map_err(|e| format!("CreateRestrictedToken failed: {e}"))
    })
}

pub(crate) fn status(backend: Backend) -> SandboxStatus {
    match backend {
        Backend::AppContainer => match appcontainer_sid() {
            Ok(_) => SandboxStatus {
                backend: "appcontainer".into(),
                available: true,
                network_isolated: true,
                warning: Some(APPCONTAINER_WARNING.into()),
            },
            Err(e) => SandboxStatus {
                backend: "appcontainer".into(),
                available: false,
                network_isolated: false,
                warning: Some(format!("{e}. Commands require approval.")),
            },
        },
        _ => match restricted_token_probe() {
            Ok(()) => SandboxStatus {
                backend: "restricted-token".into(),
                available: true,
                network_isolated: false,
                warning: Some(RESTRICTED_NETWORK_WARNING.into()),
            },
            Err(e) => SandboxStatus {
                backend: "restricted-token".into(),
                available: false,
                network_isolated: false,
                warning: Some(format!("Restricted-token sandbox unavailable ({e}); commands require approval.")),
            },
        },
    }
}

enum Mode {
    Unsandboxed,
    Restricted,
    AppContainer,
}

impl Mode {
    fn name(&self) -> &'static str {
        match self {
            Mode::Unsandboxed => "none",
            Mode::Restricted => "restricted-token",
            Mode::AppContainer => "appcontainer",
        }
    }
}

fn grant_or_warn(path: &Path, sid: &OwnedSid, mask: u32) {
    if let Err(e) = acl::ensure_grant(path, sid, mask) {
        tracing::warn!(path = %path.display(), error = %e, "could not grant sandbox access");
    }
}

fn label_or_warn(path: &Path) {
    if let Err(e) = acl::ensure_low_label(path) {
        tracing::warn!(path = %path.display(), error = %e, "could not label sandbox writable root");
    }
}

/// Creates the private temp dir and returns it.
fn prepare_temp(req: &ExecRequest) -> Result<PathBuf, SandboxError> {
    let temp = req.effective_sandbox_temp();
    std::fs::create_dir_all(&temp)
        .map_err(|e| SandboxError::Other(format!("could not create sandbox temp dir {}: {e}", temp.display())))?;
    Ok(std::path::absolute(&temp).unwrap_or(temp))
}

pub(crate) fn launch(req: &ExecRequest) -> Result<Launched, SandboxError> {
    let mode = if !req.policy.is_sandboxed() {
        Mode::Unsandboxed
    } else {
        match req.backend {
            Backend::None => {
                return Err(SandboxError::Unavailable("sandbox backend is disabled (`none`)".into()));
            }
            Backend::AppContainer => Mode::AppContainer,
            Backend::Auto | Backend::RestrictedToken => Mode::Restricted,
        }
    };
    let sandboxed = !matches!(mode, Mode::Unsandboxed);
    let cwd = std::path::absolute(&req.cwd).unwrap_or_else(|_| req.cwd.clone());

    let mut env = EnvMap::from_current();
    for (k, v) in &req.env {
        env.set(k, v);
    }

    let mut token = None;
    let mut container_sid: Option<&OwnedSid> = None;
    if sandboxed {
        let temp = prepare_temp(req)?;
        let roots = existing_roots(req.policy.writable_roots(), &cwd);
        let sid = match mode {
            Mode::AppContainer => appcontainer_sid()?,
            _ => write_sid()?,
        };
        grant_or_warn(&temp, sid, acl::WRITE_MASK);
        for root in &roots {
            grant_or_warn(root, sid, acl::WRITE_MASK);
        }
        match mode {
            Mode::AppContainer => {
                if !roots.iter().any(|r| cwd.starts_with(r)) {
                    grant_or_warn(&cwd, sid, acl::READ_MASK);
                }
                container_sid = Some(sid);
            }
            _ => {
                label_or_warn(&temp);
                for root in &roots {
                    label_or_warn(root);
                }
                let t = token::create_write_restricted_token(sid)
                    .map_err(|e| SandboxError::Unavailable(format!("could not create restricted token: {e}")))?;
                token = Some(t);
            }
        }
        env.set("TEMP", temp.as_os_str());
        env.set("TMP", temp.as_os_str());
        env.set("PSModuleAnalysisCachePath", temp.join("PSModuleAnalysisCache").as_os_str());
        env.set("POWERSHELL_TELEMETRY_OPTOUT", "1");
        env.set("POWERSHELL_UPDATECHECK", "Off");
        env.set("ODEX_SANDBOX", mode.name());
    }

    // Resolve the program with the child's PATH.
    let program =
        cmdline::resolve_program(&req.argv[0], &cwd, env.get("PATH"), env.get("PATHEXT")).ok_or_else(|| {
            SandboxError::Spawn(io::Error::new(io::ErrorKind::NotFound, format!("program not found: {}", req.argv[0])))
        })?;
    let (application, command_line) = if cmdline::is_batch(&program) {
        let cmd = cmdline::resolve_program("cmd.exe", &cwd, env.get("PATH"), None)
            .or_else(|| std::env::var_os("ComSpec").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        let line = cmdline::batch_command_line(&cmd, &program, &req.argv[1..]);
        (cmd, line)
    } else {
        let line = cmdline::build_command_line(&program, &req.argv[1..]);
        (program, line)
    };
    tracing::debug!(mode = mode.name(), command_line = %command_line, "spawning");

    let application = wide_os(application.as_os_str());
    let command_line: Vec<u16> = command_line.encode_utf16().chain(std::iter::once(0)).collect();
    let environment = env.to_block();
    let cwd_w = wide_os(cwd.as_os_str());

    let job = spawn::Job::new(sandboxed).map_err(SandboxError::Spawn)?;
    let (stdin_r, stdin_w) = spawn::pipe().map_err(SandboxError::Spawn)?;
    let (stdout_r, stdout_w) = spawn::pipe().map_err(SandboxError::Spawn)?;
    let (stderr_r, stderr_w) = spawn::pipe().map_err(SandboxError::Spawn)?;

    let started = spawn::create_process(
        spawn::ProcessSpec {
            application: &application,
            command_line,
            environment: &environment,
            cwd: &cwd_w,
            token: token.as_ref(),
            appcontainer_sid: container_sid.map(|s| s.as_psid()),
            stdin: &stdin_r,
            stdout: &stdout_w,
            stderr: &stderr_w,
        },
        &job,
    )
    .map_err(SandboxError::Spawn)?;
    // The child owns its ends now; closing ours lets EOF propagate.
    drop((stdin_r, stdout_w, stderr_w));

    let process = started.process;
    Ok(Launched {
        pid: started.pid,
        sandboxed,
        stdin: Box::new(File::from(stdin_w)),
        stdout: Box::new(File::from(stdout_r)),
        stderr: Box::new(File::from(stderr_r)),
        waiter: Box::new(move || spawn::wait_exit(&process)),
        killer: Arc::new(job),
    })
}

/// Decodes console output in the active OEM code page (used when output is not valid UTF-8).
pub(crate) fn decode_oem(bytes: &[u8]) -> Option<String> {
    // SAFETY: trivial query.
    let cp = unsafe { GetOEMCP() };
    if cp == 65001 {
        return None;
    }
    if let Some(enc) = encoding_for_codepage(cp) {
        let (text, _) = enc.decode_without_bom_handling(bytes);
        return Some(text.into_owned());
    }
    multibyte_to_string(cp, bytes)
}

fn encoding_for_codepage(cp: u32) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match cp {
        866 => IBM866,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        20866 => KOI8_R,
        21866 => KOI8_U,
        54936 => GB18030,
        _ => return None,
    })
}

/// Code pages encoding_rs does not cover (437, 850, 852, ...) go through the OS converter.
fn multibyte_to_string(cp: u32, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    let len = i32::try_from(bytes.len()).ok()?;
    // SAFETY: size query, then conversion into a buffer of the reported size.
    unsafe {
        let needed = MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, null_mut(), 0);
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed);
        if written <= 0 {
            return None;
        }
        wide.truncate(written as usize);
        Some(String::from_utf16_lossy(&wide))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sids_derive() {
        let w = write_sid().expect("write sid");
        assert!(w.to_sddl().starts_with("S-1-9-"), "{}", w.to_sddl());
        assert_eq!(w.to_sddl(), named_sid(WRITE_SID_NAME).unwrap().to_sddl());
        let a = appcontainer_sid().expect("appcontainer sid");
        assert!(a.to_sddl().starts_with("S-1-15-2-"), "{}", a.to_sddl());
    }

    #[test]
    fn oem_decoding_produces_text() {
        // 0x82 is "é" in CP437/CP850 and "‚" in Windows-1252; either way not a replacement char.
        let s = decode_oem(&[b'a', 0x82, b'b']).unwrap_or_default();
        assert!(s.starts_with('a') && s.ends_with('b'), "{s:?}");
    }
}
