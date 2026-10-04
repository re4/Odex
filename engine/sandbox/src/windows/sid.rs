//! Owned SID buffers.

use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    AllocateAndInitializeSid, CopySid, CreateWellKnownSid, EqualSid, FreeSid, GetLengthSid, IsValidSid, PSID,
    SECURITY_MAX_SID_SIZE, SID_IDENTIFIER_AUTHORITY, WELL_KNOWN_SID_TYPE,
};

/// A SID copied into a 4-byte aligned buffer we own.
pub(crate) struct OwnedSid {
    buf: Box<[u32]>,
}

impl OwnedSid {
    /// Copies the SID at `psid`.
    ///
    /// # Safety
    /// `psid` must be null or point to a readable SID.
    pub(crate) unsafe fn from_psid(psid: PSID) -> io::Result<OwnedSid> {
        if psid.is_null() || IsValidSid(psid) == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid SID"));
        }
        let len = GetLengthSid(psid);
        let mut buf = vec![0u32; (len as usize).div_ceil(4)].into_boxed_slice();
        if CopySid(len, buf.as_mut_ptr().cast(), psid) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedSid { buf })
    }

    pub(crate) fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<OwnedSid> {
        let mut buf = vec![0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)].into_boxed_slice();
        let mut size = SECURITY_MAX_SID_SIZE;
        // SAFETY: the buffer holds SECURITY_MAX_SID_SIZE bytes.
        let ok = unsafe { CreateWellKnownSid(kind, null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedSid { buf })
    }

    /// Builds `S-1-<authority>-<subs...>` (1..=8 sub-authorities).
    pub(crate) fn from_parts(authority: [u8; 6], subs: &[u32]) -> io::Result<OwnedSid> {
        if subs.is_empty() || subs.len() > 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a SID needs 1..=8 sub-authorities"));
        }
        let mut s = [0u32; 8];
        s[..subs.len()].copy_from_slice(subs);
        let auth = SID_IDENTIFIER_AUTHORITY { Value: authority };
        let mut psid: PSID = null_mut();
        // SAFETY: valid authority; the allocated SID is copied and then released with FreeSid.
        unsafe {
            if AllocateAndInitializeSid(
                &auth,
                subs.len() as u8,
                s[0],
                s[1],
                s[2],
                s[3],
                s[4],
                s[5],
                s[6],
                s[7],
                &mut psid,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let owned = OwnedSid::from_psid(psid);
            FreeSid(psid);
            owned
        }
    }

    /// Pointer usable with Win32 APIs that only read the SID.
    pub(crate) fn as_psid(&self) -> PSID {
        self.buf.as_ptr() as PSID
    }

    pub(crate) fn len(&self) -> u32 {
        // SAFETY: the buffer holds a valid SID.
        unsafe { GetLengthSid(self.as_psid()) }
    }

    /// Compares with a raw SID.
    ///
    /// # Safety
    /// `other` must point to a valid SID.
    pub(crate) unsafe fn equals_raw(&self, other: PSID) -> bool {
        EqualSid(self.as_psid(), other) != 0
    }

    /// `S-1-...` string form.
    pub(crate) fn to_sddl(&self) -> String {
        let mut out: *mut u16 = null_mut();
        // SAFETY: valid SID; the returned string is freed with LocalFree.
        unsafe {
            if ConvertSidToStringSidW(self.as_psid(), &mut out) == 0 || out.is_null() {
                return String::from("<sid>");
            }
            let mut len = 0usize;
            while *out.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
            LocalFree(out as HLOCAL);
            s
        }
    }
}

impl Clone for OwnedSid {
    fn clone(&self) -> Self {
        OwnedSid { buf: self.buf.clone() }
    }
}

impl std::fmt::Debug for OwnedSid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_sddl())
    }
}
