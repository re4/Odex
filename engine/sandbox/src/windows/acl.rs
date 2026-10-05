//! Inheritable ACE grants on writable roots.

use std::collections::HashSet;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, AddMandatoryAce, GetAce, GetAclInformation, InitializeAcl, WinLowLabelSid, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    INHERIT_ONLY_ACE, LABEL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, SYSTEM_MANDATORY_LABEL_ACE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};

use super::sid::OwnedSid;

/// Read, write, execute and delete (no WRITE_DAC / WRITE_OWNER).
pub(crate) const WRITE_MASK: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;
/// Read and execute.
pub(crate) const READ_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
const SYSTEM_MANDATORY_LABEL_NO_WRITE_UP: u32 = 0x1;
/// RID of the Low mandatory level (`S-1-16-4096`).
const LOW_INTEGRITY_RID: u32 = 0x1000;

fn granted_cache() -> &'static Mutex<HashSet<(PathBuf, String, u32)>> {
    // (path, SID or "label", mask)
    static CACHE: OnceLock<Mutex<HashSet<(PathBuf, String, u32)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Refuses grants that would propagate across huge or system-owned trees.
fn too_broad(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    let same = |var: &str| {
        std::env::var_os(var).map(|v| {
            let v = PathBuf::from(v);
            v.as_os_str().to_string_lossy().eq_ignore_ascii_case(&path.as_os_str().to_string_lossy())
        })
    };
    for var in ["USERPROFILE", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "PUBLIC"] {
        if same(var) == Some(true) {
            return true;
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        if let Some(users) = Path::new(&profile).parent() {
            if users.as_os_str().to_string_lossy().eq_ignore_ascii_case(&path.as_os_str().to_string_lossy()) {
                return true;
            }
        }
    }
    false
}

/// Ensures `path` carries an inheritable access-allowed ACE for `sid` with at least `mask`
/// (explicit or inherited). Adds one (propagating to existing children) when missing. Results
/// are cached per process so the DACL is only inspected once per (path, sid, mask).
pub(crate) fn ensure_grant(path: &Path, sid: &OwnedSid, mask: u32) -> io::Result<()> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let key = (abs.clone(), sid.to_sddl(), mask);
    if granted_cache().lock().map(|c| c.contains(&key)).unwrap_or(false) {
        return Ok(());
    }
    if too_broad(&abs) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing to grant sandbox access on a broad system location: {}", abs.display()),
        ));
    }
    let name = wide(&abs);
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `name` is NUL-terminated; outputs are freed with LocalFree below.
    let err = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let _sd_guard = LocalGuard(sd);

    // SAFETY: `dacl` is null or points into `sd`, which is alive.
    if unsafe { has_grant(dacl, sid, mask) } {
        tracing::debug!(path = %abs.display(), "sandbox ACE already present");
        if let Ok(mut c) = granted_cache().lock() {
            c.insert(key);
        }
        return Ok(());
    }

    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_psid() as *mut u16,
        },
    };
    let mut new_dacl: *mut ACL = null_mut();
    // SAFETY: `access` references a live SID; `new_dacl` is freed with LocalFree.
    let err = unsafe { SetEntriesInAclW(1, &access, dacl, &mut new_dacl) };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let _dacl_guard = LocalGuard(new_dacl.cast());
    tracing::info!(path = %abs.display(), sid = %sid.to_sddl(), "granting sandbox access (inheritable ACE)");
    // SAFETY: valid path and ACL. This propagates the inheritable ACE to existing children.
    let err = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            new_dacl,
            null_mut(),
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    if let Ok(mut c) = granted_cache().lock() {
        c.insert(key);
    }
    Ok(())
}

/// Ensures `path` carries an inheritable Low mandatory label (no-write-up), so Low-integrity
/// sandboxed processes may write there. Existing children inherit it (propagated by
/// `SetNamedSecurityInfoW`). Cached per process like [`ensure_grant`].
pub(crate) fn ensure_low_label(path: &Path) -> io::Result<()> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let key = (abs.clone(), String::from("label:low"), 0);
    if granted_cache().lock().map(|c| c.contains(&key)).unwrap_or(false) {
        return Ok(());
    }
    if too_broad(&abs) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing to relabel a broad system location: {}", abs.display()),
        ));
    }
    let name = wide(&abs);
    let mut sacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `name` is NUL-terminated; reading the label only needs READ_CONTROL.
    let err = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sacl,
            &mut sd,
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let _sd_guard = LocalGuard(sd);
    // SAFETY: `sacl` is null or points into `sd`.
    if unsafe { has_low_label(sacl) } {
        if let Ok(mut c) = granted_cache().lock() {
            c.insert(key);
        }
        return Ok(());
    }

    let low = OwnedSid::well_known(WinLowLabelSid)?;
    let size = std::mem::size_of::<ACL>() + std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() + low.len() as usize;
    let size = size.div_ceil(4) * 4;
    let mut buf = vec![0u32; size / 4];
    let acl = buf.as_mut_ptr() as *mut ACL;
    // SAFETY: `buf` is large enough for the header and one label ACE.
    unsafe {
        if InitializeAcl(acl, size as u32, ACL_REVISION) == 0 {
            return Err(io::Error::last_os_error());
        }
        if AddMandatoryAce(
            acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low.as_psid(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    tracing::info!(path = %abs.display(), "labelling sandbox writable root Low integrity (inheritable)");
    // SAFETY: valid path and SACL; setting a label at or below our own level needs WRITE_OWNER.
    let err = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            acl,
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    if let Ok(mut c) = granted_cache().lock() {
        c.insert(key);
    }
    Ok(())
}

/// Whether `sacl` holds an inheritable (OI|CI) mandatory label at or below Low.
///
/// # Safety
/// `sacl` must be null or point to a valid ACL.
unsafe fn has_low_label(sacl: *const ACL) -> bool {
    if sacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        sacl,
        (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return false;
    }
    let inherit = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
    for i in 0..info.AceCount {
        let mut ace: *mut core::ffi::c_void = null_mut();
        if GetAce(sacl, i, &mut ace) == 0 || ace.is_null() {
            continue;
        }
        let header = &*(ace as *const ACE_HEADER);
        if header.AceType != SYSTEM_MANDATORY_LABEL_ACE_TYPE
            || header.AceFlags & (INHERIT_ONLY_ACE as u8) != 0
            || header.AceFlags & inherit != inherit
        {
            continue;
        }
        let label = &*(ace as *const SYSTEM_MANDATORY_LABEL_ACE);
        // The SID is S-1-16-<level>: one sub-authority right after the 8-byte SID header.
        let sid = (&label.SidStart as *const u32) as *const u8;
        let sub_count = *sid.add(1);
        if sub_count != 1 {
            continue;
        }
        let rid = std::ptr::read_unaligned(sid.add(8) as *const u32);
        if rid <= LOW_INTEGRITY_RID {
            return true;
        }
    }
    false
}

/// Whether `dacl` allows `mask` to `sid` on the object itself and (for directories) inheritably.
///
/// # Safety
/// `dacl` must be null or point to a valid ACL.
unsafe fn has_grant(dacl: *const ACL, sid: &OwnedSid, mask: u32) -> bool {
    if dacl.is_null() {
        // A NULL DACL grants everything to everyone.
        return true;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        dacl,
        (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return false;
    }
    let inherit = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
    for i in 0..info.AceCount {
        let mut ace: *mut core::ffi::c_void = null_mut();
        if GetAce(dacl, i, &mut ace) == 0 || ace.is_null() {
            continue;
        }
        let header = &*(ace as *const ACE_HEADER);
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }
        if header.AceFlags & (INHERIT_ONLY_ACE as u8) != 0 || header.AceFlags & inherit != inherit {
            continue;
        }
        let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
        if allowed.Mask & mask != mask {
            continue;
        }
        let ace_sid = (&allowed.SidStart as *const u32) as *mut core::ffi::c_void;
        if sid.equals_raw(ace_sid) {
            return true;
        }
    }
    false
}

struct LocalGuard(*mut core::ffi::c_void);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by the security APIs with LocalAlloc.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}
