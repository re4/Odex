//! Output capture helpers: head+tail capped buffers, decoding and the denial heuristic.

use std::collections::VecDeque;

/// Keeps the first half and the last half of a byte stream once it exceeds `cap`.
pub(crate) struct HeadTail {
    head_cap: usize,
    tail_cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl HeadTail {
    pub(crate) fn new(cap: usize) -> Self {
        let cap = cap.max(2);
        let head_cap = cap / 2;
        HeadTail { head_cap, tail_cap: cap - head_cap, head: Vec::new(), tail: VecDeque::new(), total: 0 }
    }

    pub(crate) fn push(&mut self, mut data: &[u8]) {
        self.total += data.len();
        if self.head.len() < self.head_cap {
            let n = (self.head_cap - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..n]);
            data = &data[n..];
        }
        if data.is_empty() {
            return;
        }
        if data.len() >= self.tail_cap {
            self.tail.clear();
            self.tail.extend(&data[data.len() - self.tail_cap..]);
            return;
        }
        self.tail.extend(data);
        if self.tail.len() > self.tail_cap {
            let excess = self.tail.len() - self.tail_cap;
            self.tail.drain(..excess);
        }
    }

    pub(crate) fn truncated(&self) -> bool {
        self.total > self.head.len() + self.tail.len()
    }

    /// Decoded text (with a truncation marker when needed) and whether it was truncated.
    pub(crate) fn render(&self) -> (String, bool) {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if !self.truncated() {
            let mut all = self.head.clone();
            all.extend_from_slice(&tail);
            return (decode(&all), false);
        }
        let omitted = self.total - self.head.len() - tail.len();
        // The tail may start in the middle of a UTF-8 sequence; skip dangling continuation bytes.
        let skip = tail.iter().take(3).take_while(|b| (**b & 0xC0) == 0x80).count();
        let mut text = decode(&self.head);
        text.push_str(&format!("\n[... {omitted} bytes truncated ...]\n"));
        text.push_str(&decode(&tail[skip..]));
        (text, true)
    }
}

/// UTF-8 first; if the bytes are not UTF-8 (beyond an incomplete trailing sequence), fall back to
/// the console's OEM code page on Windows, else lossy UTF-8.
pub(crate) fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => {
            #[cfg(windows)]
            if let Some(s) = crate::windows::decode_oem(bytes) {
                return s;
            }
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

const DENIAL_MARKERS: &[&str] = &[
    "access is denied",
    "access denied",
    "permission denied",
    "read-only file system",
    "eacces",
    "eperm",
    "unauthorizedaccessexception",
    "operation not permitted",
    "access to the path",
    "is denied",
    "sandbox-exec: ",
    "deny file-write",
];

/// Heuristic for "the sandbox blocked this": non-zero exit plus access-denied style output.
pub(crate) fn looks_sandbox_denied(exit_code: Option<i32>, output: &str) -> bool {
    if !matches!(exit_code, Some(c) if c != 0) {
        return false;
    }
    let lower = output.to_lowercase();
    DENIAL_MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_keeps_everything_under_cap() {
        let mut b = HeadTail::new(10);
        b.push(b"hello");
        b.push(b"world");
        assert_eq!(b.render(), ("helloworld".to_string(), false));
    }

    #[test]
    fn head_tail_truncates_with_marker() {
        let mut b = HeadTail::new(10);
        for _ in 0..10 {
            b.push(b"0123456789");
        }
        let (s, t) = b.render();
        assert!(t);
        assert!(s.starts_with("01234"), "{s}");
        assert!(s.ends_with("56789"), "{s}");
        assert!(s.contains("90 bytes truncated"), "{s}");
    }

    #[test]
    fn head_tail_large_single_push() {
        let mut b = HeadTail::new(4);
        b.push(b"abcdefghij");
        let (s, t) = b.render();
        assert!(t);
        assert!(s.starts_with("ab") && s.ends_with("ij"), "{s}");
    }

    #[test]
    fn decode_utf8_and_partial() {
        assert_eq!(decode("héllo".as_bytes()), "héllo");
        let bytes = "é".as_bytes();
        assert_eq!(decode(&bytes[..1]), "\u{FFFD}");
    }

    #[test]
    fn denial_heuristic() {
        assert!(looks_sandbox_denied(Some(1), "Access is denied."));
        assert!(looks_sandbox_denied(Some(1), "bash: x: Permission denied"));
        assert!(looks_sandbox_denied(Some(1), "Access to the path 'C:\\x' is denied."));
        assert!(!looks_sandbox_denied(Some(0), "Access is denied."));
        assert!(!looks_sandbox_denied(None, "Access is denied."));
        assert!(!looks_sandbox_denied(Some(2), "file not found"));
    }
}
