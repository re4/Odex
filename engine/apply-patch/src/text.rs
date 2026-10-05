//! A text file split into lines while remembering its BOM, per-line endings
//! and whether it ends with a newline, so edits round-trip faithfully.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
        }
    }
}

/// One edit against the original line array: replace `old_len` lines at
/// `start` with `new_lines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replacement {
    pub start: usize,
    pub old_len: usize,
    pub new_lines: Vec<String>,
    /// 1-based chunk index (for error messages).
    pub chunk: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextFile {
    pub bom: bool,
    pub lines: Vec<String>,
    /// Line ending of each line. The last line's entry is only emitted when
    /// `final_newline` is set.
    pub eols: Vec<Eol>,
    pub final_newline: bool,
    /// Ending used for inserted lines: the file's dominant style.
    pub default_eol: Eol,
}

impl TextFile {
    pub(crate) fn parse(text: &str) -> Self {
        let (bom, body) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let crlf = body.matches("\r\n").count();
        let lf = body.matches('\n').count() - crlf;
        let default_eol = if crlf > lf { Eol::CrLf } else { Eol::Lf };

        let mut lines = Vec::new();
        let mut eols = Vec::new();
        let mut final_newline = true;
        for piece in body.split_inclusive('\n') {
            if let Some(line) = piece.strip_suffix("\r\n") {
                lines.push(line.to_string());
                eols.push(Eol::CrLf);
            } else if let Some(line) = piece.strip_suffix('\n') {
                lines.push(line.to_string());
                eols.push(Eol::Lf);
            } else {
                lines.push(piece.to_string());
                eols.push(default_eol);
                final_newline = false;
            }
        }
        TextFile { bom, lines, eols, final_newline, default_eol }
    }

    /// Build a file from new contents, adopting the BOM and line-ending
    /// style of `like` (used when an Add overwrites an existing file).
    pub(crate) fn with_style_of(contents: &str, like: &TextFile) -> Self {
        let mut file = TextFile::parse(contents);
        file.bom = like.bom;
        file.default_eol = like.default_eol;
        file.eols.iter_mut().for_each(|e| *e = like.default_eol);
        file
    }

    /// Exact on-disk rendering.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        self.render_body(&mut out, false);
        out
    }

    /// LF-only rendering without BOM, for diffs.
    pub(crate) fn render_normalized(&self) -> String {
        let mut out = String::new();
        self.render_body(&mut out, true);
        out
    }

    fn render_body(&self, out: &mut String, normalized: bool) {
        let n = self.lines.len();
        for (i, line) in self.lines.iter().enumerate() {
            out.push_str(line);
            if i + 1 < n || self.final_newline {
                out.push_str(if normalized { "\n" } else { self.eols[i].as_str() });
            }
        }
    }

    /// Apply non-overlapping replacements sorted by `start`.
    pub(crate) fn apply(&self, replacements: &[Replacement]) -> TextFile {
        let mut lines = Vec::with_capacity(self.lines.len());
        let mut eols = Vec::with_capacity(self.lines.len());
        let mut pos = 0;
        for rep in replacements {
            lines.extend_from_slice(&self.lines[pos..rep.start]);
            eols.extend_from_slice(&self.eols[pos..rep.start]);
            lines.extend(rep.new_lines.iter().cloned());
            eols.resize(eols.len() + rep.new_lines.len(), self.default_eol);
            pos = rep.start + rep.old_len;
        }
        lines.extend_from_slice(&self.lines[pos..]);
        eols.extend_from_slice(&self.eols[pos..]);
        TextFile { bom: self.bom, lines, eols, final_newline: self.final_newline, default_eol: self.default_eol }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for text in ["", "a", "a\n", "a\nb", "a\r\nb\r\n", "\u{feff}x\r\ny", "mixed\r\nlf\nend\r\n", "\n\n"] {
            assert_eq!(TextFile::parse(text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn detects_style() {
        let f = TextFile::parse("\u{feff}a\r\nb\r\nc");
        assert!(f.bom);
        assert_eq!(f.default_eol, Eol::CrLf);
        assert!(!f.final_newline);
        assert_eq!(f.lines, vec!["a", "b", "c"]);
        assert_eq!(f.render_normalized(), "a\nb\nc");
        let empty = TextFile::parse("");
        assert!(empty.lines.is_empty());
        assert!(empty.final_newline);
    }

    #[test]
    fn apply_uses_default_eol_for_new_lines_and_keeps_others() {
        let f = TextFile::parse("a\r\nb\nc\r\n");
        let out = f.apply(&[Replacement { start: 1, old_len: 1, new_lines: vec!["B".into(), "B2".into()], chunk: 1 }]);
        assert_eq!(out.render(), "a\r\nB\r\nB2\r\nc\r\n");
    }

    #[test]
    fn apply_preserves_missing_final_newline() {
        let f = TextFile::parse("a\nb");
        let out = f.apply(&[Replacement { start: 2, old_len: 0, new_lines: vec!["c".into()], chunk: 1 }]);
        assert_eq!(out.render(), "a\nb\nc");
        let out = f.apply(&[Replacement { start: 0, old_len: 2, new_lines: vec![], chunk: 1 }]);
        assert_eq!(out.render(), "");
    }

    #[test]
    fn with_style_of_adopts_bom_and_crlf() {
        let like = TextFile::parse("\u{feff}old\r\n");
        assert_eq!(TextFile::with_style_of("x\ny\n", &like).render(), "\u{feff}x\r\ny\r\n");
    }
}
