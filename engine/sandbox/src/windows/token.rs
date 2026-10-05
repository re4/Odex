//! Write-restricted token creation.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GENERIC_ALL, HANDLE};
use windows_sys::Win32::Security::{
    AddAccessAllowedAce, CreateRestrictedToken, GetTokenInformation, InitializeAcl, SetTokenInformation,
    TokenDefaultDacl, TokenGroups, TokenIntegrityLevel, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    WinLowLabelSid, WinRestrictedCodeSid, WinWorldSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DISABLE_MAX_PRIVILEGE,
    SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS,
    TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, WRITE_RESTRICTED,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::sid::OwnedSid;

/// `SE_GROUP_LOGON_ID` (winnt.h).
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
/// `SE_GROUP_INTEGRITY` (winnt.h).
const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;

fn open_process_token() -> io::Result<OwnedHandle> {
    let mut token: HANDLE = null_mut();
    let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT;
    // SAFETY: GetCurrentProcess returns a pseudo handle; `token` receives a new handle we own.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: freshly opened handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(token as _) })
}

/// Token information in an 8-byte aligned buffer.
fn token_information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: size query.
    unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `needed` bytes.
    let ok = unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), needed, &mut needed) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

struct TokenIdentity {
    user: OwnedSid,
    logon: Option<OwnedSid>,
    admins_present: bool,
}

fn identity(token: HANDLE) -> io::Result<TokenIdentity> {
    let user_buf = token_information(token, TokenUser)?;
    // SAFETY: GetTokenInformation(TokenUser) fills a TOKEN_USER.
    let user = unsafe { OwnedSid::from_psid((*(user_buf.as_ptr() as *const TOKEN_USER)).User.Sid)? };

    let groups_buf = token_information(token, TokenGroups)?;
    let admins = OwnedSid::well_known(WinBuiltinAdministratorsSid)?;
    let mut logon = None;
    let mut admins_present = false;
    // SAFETY: GetTokenInformation(TokenGroups) fills a TOKEN_GROUPS with GroupCount entries.
    unsafe {
        let groups = groups_buf.as_ptr() as *const TOKEN_GROUPS;
        let count = (*groups).GroupCount as usize;
        let entries = std::slice::from_raw_parts((*groups).Groups.as_ptr(), count);
        for g in entries {
            if g.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID && logon.is_none() {
                logon = OwnedSid::from_psid(g.Sid).ok();
            }
            if admins.equals_raw(g.Sid) {
                admins_present = true;
            }
        }
    }
    Ok(TokenIdentity { user, logon, admins_present })
}

/// An ACL with one access-allowed ACE (no inheritance flags) per entry.
pub(crate) fn build_acl(entries: &[(&OwnedSid, u32)]) -> io::Result<Vec<u32>> {
    let ace_base = std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>();
    let size = std::mem::size_of::<ACL>() + entries.iter().map(|(sid, _)| ace_base + sid.len() as usize).sum::<usize>();
    let size = size.div_ceil(4) * 4;
    let mut buf = vec![0u32; size / 4];
    let acl = buf.as_mut_ptr() as *mut ACL;
    // SAFETY: `buf` holds `size` bytes, enough for the header and every ACE.
    unsafe {
        if InitializeAcl(acl, size as u32, ACL_REVISION) == 0 {
            return Err(io::Error::last_os_error());
        }
        for (sid, mask) in entries {
            if AddAccessAllowedAce(acl, ACL_REVISION, *mask, sid.as_psid()) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(buf)
}

/// Creates a primary token derived from the current process token that:
///
/// * has every privilege except `SeChangeNotifyPrivilege` removed (`DISABLE_MAX_PRIVILEGE`);
/// * has `BUILTIN\Administrators` turned into a deny-only SID (relevant when elevated);
/// * is **write-restricted**: write access additionally requires one of the restricting SIDs —
///   `write_sid` (granted on writable roots), the logon SID (window station / desktop / session
///   named objects), `Everyone` and `RESTRICTED` (needed for `NUL`, console and pipe devices
///   whose default DACLs grant those) — to be granted by the object's DACL. Reads are checked
///   against the normal user SIDs, so the child can read whatever the user can read;
/// * has a default DACL that also grants `write_sid`, so objects the sandboxed process creates
///   without an explicit DACL (named pipes, events, semaphores, its own process/threads) stay
///   writable to it and its children;
/// * runs at **Low integrity**. Write-restricted tokens do not subject `DELETE` to the restricting
///   SIDs, so without this the child could delete (though not modify) any file the user owns.
///   Mandatory integrity "no write up" covers `DELETE`, `WRITE_DAC` and `WRITE_OWNER` too, so the
///   writable roots additionally carry an inheritable Low mandatory label (see `acl`).
pub(crate) fn create_write_restricted_token(write_sid: &OwnedSid) -> io::Result<OwnedHandle> {
    let base = open_process_token()?;
    let base_raw = base.as_raw_handle() as HANDLE;
    let id = identity(base_raw)?;

    let everyone = OwnedSid::well_known(WinWorldSid)?;
    let restricted = OwnedSid::well_known(WinRestrictedCodeSid)?;
    let system = OwnedSid::well_known(WinLocalSystemSid)?;
    let admins = OwnedSid::well_known(WinBuiltinAdministratorsSid)?;

    let mut restricting: Vec<&OwnedSid> = vec![write_sid, &everyone, &restricted];
    if let Some(logon) = &id.logon {
        restricting.push(logon);
    }
    let restricting: Vec<SID_AND_ATTRIBUTES> =
        restricting.iter().map(|s| SID_AND_ATTRIBUTES { Sid: s.as_psid(), Attributes: 0 }).collect();
    let disable: Vec<SID_AND_ATTRIBUTES> =
        if id.admins_present { vec![SID_AND_ATTRIBUTES { Sid: admins.as_psid(), Attributes: 0 }] } else { Vec::new() };

    let mut new_token: HANDLE = null_mut();
    // SAFETY: all SID pointers outlive the call; `new_token` receives a handle we own.
    let ok = unsafe {
        CreateRestrictedToken(
            base_raw,
            DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
            disable.len() as u32,
            if disable.is_empty() { null() } else { disable.as_ptr() },
            0,
            null(),
            restricting.len() as u32,
            restricting.as_ptr(),
            &mut new_token,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: freshly created handle.
    let token = unsafe { OwnedHandle::from_raw_handle(new_token as _) };

    let mut dacl_entries: Vec<(&OwnedSid, u32)> =
        vec![(&id.user, GENERIC_ALL), (&system, GENERIC_ALL), (write_sid, GENERIC_ALL)];
    if let Some(logon) = &id.logon {
        dacl_entries.push((logon, GENERIC_ALL));
    }
    let acl = build_acl(&dacl_entries)?;
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: acl.as_ptr() as *mut ACL };
    // SAFETY: `info` points at a valid ACL that outlives the call.
    let ok = unsafe {
        SetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenDefaultDacl,
            (&info as *const TOKEN_DEFAULT_DACL).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let low = OwnedSid::well_known(WinLowLabelSid)?;
    let label =
        TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: low.as_psid(), Attributes: SE_GROUP_INTEGRITY } };
    // SAFETY: `label` references a live SID for the duration of the call.
    let ok = unsafe {
        SetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenIntegrityLevel,
            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
            std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + low.len(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(token)
}
