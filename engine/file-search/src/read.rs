//! Paged file reads with encoding and binary detection.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::Error;

/// How many leading bytes are sniffed for BOMs and NUL bytes.
const SNIFF_LEN: usize = 8 * 1024;

/// One page of a text file, formatted like `cat -n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePage {
    /// Lines prefixed with right-aligned line numbers and a tab, joined with `\n`.
    pub text: String,
    /// First line in `text` (1-based).
    pub start_line: usize,
    /// Last line in `text` (inclusive; `start_line - 1` when the page is empty).
    pub end_line: usize,
    pub total_lines: usize,
    /// Whether lines exist after `end_line`.
    pub truncated: bool,
    pub is_binary: bool,
    /// `utf-8`, `utf-8-bom`, `utf-16le`, `utf-16be`, `windows-1252` (fallback for invalid UTF-8)
    /// or `binary`.
    pub encoding: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

struct Pager {
    start: usize,
    end: usize,
    max_line_chars: usize,
    total: usize,
    out: Vec<String>,
    lossy: bool,
}

impl Pager {
    fn wants(&self, line_no: usize) -> bool {
        line_no >= self.start && line_no <= self.end
    }

    fn push(&mut self, line: &str) {
        self.total += 1;
        let n = self.total;
        if !self.wants(n) {
            return;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let shown = truncate_line(line, self.max_line_chars);
        self.out.push(format!("{n:>6}\t{shown}"));
    }

    fn push_bytes(&mut self, raw: &[u8]) {
        if !self.wants(self.total + 1) {
            self.total += 1;
            return;
        }
        match std::str::from_utf8(raw) {
            Ok(s) => self.push(s),
            Err(_) => {
                self.lossy = true;
                let (s, _, _) = encoding_rs::WINDOWS_1252.decode(raw);
                self.push(&s);
            }
        }
    }
}

fn truncate_line(line: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    if max_chars == 0 {
        return line.into();
    }
    match line.char_indices().nth(max_chars) {
        None => line.into(),
        Some((cut, _)) => {
            let omitted = line[cut..].chars().count();
            format!("{}… [{omitted} chars]", &line[..cut]).into()
        }
    }
}

fn read_up_to(file: &mut File, buf: &mut Vec<u8>, n: usize) -> std::io::Result<()> {
    buf.clear();
    file.by_ref().take(n as u64).read_to_end(buf)?;
    Ok(())
}

/// Read lines `offset..offset+limit` (1-based; `limit` 0 = to the end) of a text file.
///
/// Detects UTF-8/UTF-16 BOMs, falls back to Windows-1252 for invalid UTF-8, strips `\r` from
/// CRLF line endings, and truncates lines longer than `max_line_chars` (0 = no limit) with
/// `… [N chars]` (N = omitted chars). Files with NUL bytes in the first 8 KiB (and no UTF-16 BOM)
/// are reported as binary with a short placeholder instead of content.
pub fn read_file_paged(path: &Path, offset: usize, limit: usize, max_line_chars: usize) -> Result<FilePage, Error> {
    let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() {
        return Err(Error::InvalidArgument(format!("is a directory: {}", path.display())));
    }
    let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    read_up_to(&mut file, &mut head, SNIFF_LEN).map_err(|e| Error::io(path, e))?;

    let (encoding, bom_len) = if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (Encoding::Utf8Bom, 3)
    } else if head.starts_with(&[0xFF, 0xFE]) {
        (Encoding::Utf16Le, 2)
    } else if head.starts_with(&[0xFE, 0xFF]) {
        (Encoding::Utf16Be, 2)
    } else if head.contains(&0) {
        return Ok(FilePage {
            text: format!("[binary file: {} bytes, not shown]", meta.len()),
            start_line: 0,
            end_line: 0,
            total_lines: 0,
            truncated: false,
            is_binary: true,
            encoding: "binary".into(),
        });
    } else {
        (Encoding::Utf8, 0)
    };

    let start = offset.max(1);
    let end = if limit == 0 { usize::MAX } else { start.saturating_add(limit - 1) };
    let mut pager = Pager { start, end, max_line_chars, total: 0, out: Vec::new(), lossy: false };

    match encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let mut rest = Vec::new();
            file.read_to_end(&mut rest).map_err(|e| Error::io(path, e))?;
            head.extend_from_slice(&rest);
            let codec = if encoding == Encoding::Utf16Le { encoding_rs::UTF_16LE } else { encoding_rs::UTF_16BE };
            let (text, _) = codec.decode_without_bom_handling(&head[bom_len..]);
            let body = text.strip_suffix('\n').unwrap_or(&text);
            if !text.is_empty() {
                for line in body.split('\n') {
                    pager.push(line);
                }
            }
        }
        Encoding::Utf8 | Encoding::Utf8Bom => {
            let mut head = std::io::Cursor::new(head);
            head.set_position(bom_len as u64);
            let mut reader = BufReader::new(head.chain(file));
            let mut buf = Vec::new();
            loop {
                buf.clear();
                let n = reader.read_until(b'\n', &mut buf).map_err(|e| Error::io(path, e))?;
                if n == 0 {
                    break;
                }
                let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
                pager.push_bytes(line);
            }
        }
    }

    let total_lines = pager.total;
    let shown = pager.out.len();
    let end_line = if shown == 0 { start - 1 } else { start + shown - 1 };
    let encoding = match encoding {
        Encoding::Utf8 if pager.lossy => "windows-1252",
        Encoding::Utf8 => "utf-8",
        Encoding::Utf8Bom => "utf-8-bom",
        Encoding::Utf16Le => "utf-16le",
        Encoding::Utf16Be => "utf-16be",
    };
    Ok(FilePage {
        text: pager.out.join("\n"),
        start_line: start,
        end_line,
        total_lines,
        truncated: end_line < total_lines,
        is_binary: false,
        encoding: encoding.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn offsets_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=10).map(|i| format!("line {i}\r\n")).collect();
        let p = write(&dir, "a.txt", body.as_bytes());

        let page = read_file_paged(&p, 1, 3, 0).unwrap();
        assert_eq!(page.text, "     1\tline 1\n     2\tline 2\n     3\tline 3");
        assert_eq!((page.start_line, page.end_line, page.total_lines, page.truncated), (1, 3, 10, true));
        assert_eq!(page.encoding, "utf-8");

        let page = read_file_paged(&p, 9, 5, 0).unwrap();
        assert_eq!(page.text, "     9\tline 9\n    10\tline 10");
        assert_eq!((page.start_line, page.end_line, page.truncated), (9, 10, false));

        let page = read_file_paged(&p, 0, 0, 0).unwrap();
        assert_eq!(page.end_line, 10);
        assert!(!page.text.contains('\r'));

        let page = read_file_paged(&p, 50, 10, 0).unwrap();
        assert_eq!((page.text.as_str(), page.start_line, page.end_line, page.total_lines), ("", 50, 49, 10));

        let p = write(&dir, "noeol.txt", b"a\nb");
        assert_eq!(read_file_paged(&p, 1, 0, 0).unwrap().total_lines, 2);
        let p = write(&dir, "empty.txt", b"");
        let page = read_file_paged(&p, 1, 10, 0).unwrap();
        assert_eq!((page.total_lines, page.end_line, page.text.as_str()), (0, 0, ""));
    }

    #[test]
    fn utf16_and_bom_detection() {
        let dir = tempfile::tempdir().unwrap();
        let text = "héllo\r\nwörld\r\n";
        let mut le = vec![0xFF, 0xFE];
        le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let page = read_file_paged(&write(&dir, "le.txt", &le), 1, 0, 0).unwrap();
        assert_eq!(page.encoding, "utf-16le");
        assert_eq!(page.text, "     1\théllo\n     2\twörld");
        assert!(!page.is_binary);

        let mut be = vec![0xFE, 0xFF];
        be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        let page = read_file_paged(&write(&dir, "be.txt", &be), 2, 1, 0).unwrap();
        assert_eq!(page.encoding, "utf-16be");
        assert_eq!(page.text, "     2\twörld");

        let page = read_file_paged(&write(&dir, "bom.txt", b"\xEF\xBB\xBFfirst\nsecond\n"), 1, 1, 0).unwrap();
        assert_eq!(page.encoding, "utf-8-bom");
        assert_eq!(page.text, "     1\tfirst");

        let page = read_file_paged(&write(&dir, "latin.txt", b"caf\xE9\n"), 1, 0, 0).unwrap();
        assert_eq!(page.encoding, "windows-1252");
        assert_eq!(page.text, "     1\tcafé");
    }

    #[test]
    fn binary_and_long_lines() {
        let dir = tempfile::tempdir().unwrap();
        let page = read_file_paged(&write(&dir, "x.bin", b"PK\x03\x04\x00\x00data"), 1, 10, 0).unwrap();
        assert!(page.is_binary);
        assert_eq!(page.encoding, "binary");
        assert!(page.text.contains("binary file"));

        let long = format!("{}\nshort\n", "é".repeat(30));
        let page = read_file_paged(&write(&dir, "long.txt", long.as_bytes()), 1, 0, 10).unwrap();
        assert_eq!(page.text, format!("     1\t{}… [20 chars]\n     2\tshort", "é".repeat(10)));

        assert!(matches!(read_file_paged(dir.path(), 1, 1, 0), Err(Error::InvalidArgument(_))));
        assert!(matches!(read_file_paged(&dir.path().join("nope"), 1, 1, 0), Err(Error::NotFound(_))));
    }
}
