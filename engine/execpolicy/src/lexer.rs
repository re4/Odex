//! Tokenising shell command lines (POSIX shells, PowerShell, cmd) and
//! splitting them into simple commands.
//!
//! The lexer is intentionally conservative: anything whose effect cannot be
//! determined statically (command substitution, subshells, process
//! substitution, PowerShell sub-expressions, ...) makes the strict parse fail
//! with [`Complex`]. A *lenient* mode treats those constructs as command
//! separators instead, which is good enough for spotting dangerous commands
//! buried inside complex scripts.

use crate::ShellKind;

/// Maximum nesting of PowerShell script blocks we analyse.
const MAX_BLOCK_DEPTH: u8 = 4;

/// Operator that joins two simple commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    And,
    Or,
    Seq,
    Pipe,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirKind {
    Out,
    Append,
    In,
    /// `2>&1`, `>&-`: duplicates a descriptor, no file involved.
    Dup,
    HereDoc,
    HereString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    pub fd: Option<String>,
    pub kind: RedirKind,
    pub target: String,
}

impl Redirect {
    /// Output redirection into a real file (not a null device / descriptor).
    pub(crate) fn writes_file(&self) -> bool {
        matches!(self.kind, RedirKind::Out | RedirKind::Append) && !is_null_device(&self.target)
    }
}

pub(crate) fn is_null_device(target: &str) -> bool {
    matches!(
        target.to_ascii_lowercase().as_str(),
        "/dev/null" | "$null" | "nul" | "nul:" | "\\\\.\\nul" | "/dev/stdout" | "/dev/stderr"
    )
}

/// One simple command: words, redirections and how it was joined to the
/// previous command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SimpleCommand {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
    pub op_before: Option<Op>,
    /// PowerShell: a pipeline element that is an expression (variable,
    /// literal) rather than a command invocation.
    pub expression: bool,
    /// Commands inside PowerShell script blocks (`{ ... }`) of this command.
    pub nested: Vec<SimpleCommand>,
}

/// Marker error: the command uses constructs we do not analyse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Complex;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word {
        text: String,
        quoted: bool,
    },
    Op(Op),
    Redirect(Redirect),
    Block(String),
    /// PowerShell `&` call operator.
    CallOp,
}

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Lexer {
    chars: Vec<char>,
    i: usize,
    lenient: bool,
    tokens: Vec<Token>,
    word: String,
    in_word: bool,
    word_quoted: bool,
    pending_redirect: Option<(Option<String>, RedirKind)>,
    heredocs: Vec<Heredoc>,
}

fn is_posix_operator_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

impl Lexer {
    fn new(src: &str, lenient: bool) -> Self {
        Lexer {
            chars: src.chars().collect(),
            i: 0,
            lenient,
            tokens: Vec::new(),
            word: String::new(),
            in_word: false,
            word_quoted: false,
            pending_redirect: None,
            heredocs: Vec::new(),
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    fn push_char(&mut self, c: char) {
        if !self.in_word {
            self.word_quoted = false;
        }
        self.in_word = true;
        self.word.push(c);
    }

    fn start_quote(&mut self) {
        if !self.in_word {
            self.word_quoted = true;
        }
        self.in_word = true;
    }

    fn flush(&mut self) {
        if !self.in_word {
            return;
        }
        let text = std::mem::take(&mut self.word);
        let quoted = self.word_quoted;
        self.in_word = false;
        self.word_quoted = false;
        match self.pending_redirect.take() {
            Some((fd, kind)) => self.tokens.push(Token::Redirect(Redirect { fd, kind, target: text })),
            None => self.tokens.push(Token::Word { text, quoted }),
        }
    }

    fn push_op(&mut self, op: Op) -> Result<(), Complex> {
        self.flush();
        if self.pending_redirect.take().is_some() && !self.lenient {
            return Err(Complex);
        }
        self.tokens.push(Token::Op(op));
        Ok(())
    }

    /// A construct we cannot analyse: fail (strict) or split there (lenient).
    fn complex(&mut self) -> Result<(), Complex> {
        if self.lenient {
            self.push_op(Op::Seq)
        } else {
            Err(Complex)
        }
    }

    fn finish(mut self) -> Result<Vec<Token>, Complex> {
        self.flush();
        if self.pending_redirect.is_some() && !self.lenient {
            return Err(Complex);
        }
        Ok(self.tokens)
    }

    fn at_command_start(&self) -> bool {
        !self.in_word && matches!(self.tokens.last(), None | Some(Token::Op(_)))
    }

    /// Read characters up to whitespace / an operator character.
    fn read_bare_word(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek(0) {
            if is_posix_operator_char(c) {
                break;
            }
            out.push(c);
            self.i += 1;
        }
        out
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek(0), Some(' ') | Some('\t')) {
            self.i += 1;
        }
    }

    /// Take the fd prefix (`2>`) from the current word, or flush it.
    fn take_fd_prefix(&mut self, allow_star: bool) -> Option<String> {
        let is_fd = self.in_word
            && !self.word_quoted
            && !self.word.is_empty()
            && (self.word.chars().all(|c| c.is_ascii_digit()) || (allow_star && self.word == "*"));
        if is_fd {
            self.in_word = false;
            Some(std::mem::take(&mut self.word))
        } else {
            self.flush();
            None
        }
    }

    // ----- POSIX shells -------------------------------------------------

    fn lex_posix(mut self) -> Result<Vec<Token>, Complex> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.flush();
                    self.i += 1;
                }
                '\n' => {
                    self.push_op(Op::Seq)?;
                    self.i += 1;
                    self.read_heredoc_bodies()?;
                }
                '#' if !self.in_word => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\\' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(next) => {
                        self.push_char(next);
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                '\'' => {
                    self.start_quote();
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None if self.lenient => break,
                            None => return Err(Complex),
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(ch) => {
                                self.word.push(ch);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    self.start_quote();
                    self.i += 1;
                    self.lex_posix_double_quoted()?;
                }
                '`' => {
                    self.complex()?;
                    self.i += 1;
                }
                '$' => match self.peek(1) {
                    Some('(') => {
                        self.complex()?;
                        self.i += 2;
                    }
                    Some('{') => {
                        self.push_char('$');
                        self.i += 1;
                        while let Some(ch) = self.peek(0) {
                            self.word.push(ch);
                            self.i += 1;
                            if ch == '}' {
                                break;
                            }
                        }
                    }
                    _ => {
                        self.push_char('$');
                        self.i += 1;
                    }
                },
                '(' | ')' => {
                    self.complex()?;
                    self.i += 1;
                }
                '{' | '}' if !self.in_word && standalone_brace(self.peek(1)) => {
                    self.complex()?;
                    self.i += 1;
                }
                '&' => match self.peek(1) {
                    Some('&') => {
                        self.push_op(Op::And)?;
                        self.i += 2;
                    }
                    Some('>') => {
                        self.flush();
                        self.i += 2;
                        let kind = if self.peek(0) == Some('>') {
                            self.i += 1;
                            RedirKind::Append
                        } else {
                            RedirKind::Out
                        };
                        self.pending_redirect = Some((Some("&".to_string()), kind));
                    }
                    _ => {
                        self.push_op(Op::Background)?;
                        self.i += 1;
                    }
                },
                '|' => match self.peek(1) {
                    Some('|') => {
                        self.push_op(Op::Or)?;
                        self.i += 2;
                    }
                    Some('&') => {
                        self.push_op(Op::Pipe)?;
                        self.i += 2;
                    }
                    _ => {
                        self.push_op(Op::Pipe)?;
                        self.i += 1;
                    }
                },
                ';' => {
                    self.push_op(Op::Seq)?;
                    self.i += 1;
                    if self.peek(0) == Some(';') {
                        self.i += 1;
                    }
                }
                '>' | '<' => self.lex_posix_redirect()?,
                _ => {
                    self.push_char(c);
                    self.i += 1;
                }
            }
        }
        self.finish()
    }

    fn lex_posix_double_quoted(&mut self) -> Result<(), Complex> {
        loop {
            match self.peek(0) {
                None if self.lenient => return Ok(()),
                None => return Err(Complex),
                Some('"') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => match self.peek(1) {
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        self.word.push(next);
                        self.i += 2;
                    }
                    Some('\n') => self.i += 2,
                    Some(next) => {
                        self.word.push('\\');
                        self.word.push(next);
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                Some('$') if self.peek(1) == Some('(') => {
                    if !self.lenient {
                        return Err(Complex);
                    }
                    self.word.push('$');
                    self.i += 1;
                }
                Some('`') => {
                    if !self.lenient {
                        return Err(Complex);
                    }
                    self.word.push('`');
                    self.i += 1;
                }
                Some(ch) => {
                    self.word.push(ch);
                    self.i += 1;
                }
            }
        }
    }

    fn lex_posix_redirect(&mut self) -> Result<(), Complex> {
        let c = self.peek(0).unwrap_or('>');
        let fd = self.take_fd_prefix(false);
        if self.pending_redirect.is_some() && !self.lenient {
            return Err(Complex);
        }
        if c == '>' {
            match self.peek(1) {
                Some('>') => {
                    self.i += 2;
                    self.pending_redirect = Some((fd, RedirKind::Append));
                }
                Some('&') => {
                    self.i += 2;
                    self.skip_blanks();
                    let target = self.read_bare_word();
                    let redirect =
                        if target == "-" || (!target.is_empty() && target.chars().all(|c| c.is_ascii_digit())) {
                            Redirect { fd, kind: RedirKind::Dup, target: format!("&{target}") }
                        } else {
                            Redirect { fd, kind: RedirKind::Out, target }
                        };
                    self.tokens.push(Token::Redirect(redirect));
                }
                Some('|') => {
                    self.i += 2;
                    self.pending_redirect = Some((fd, RedirKind::Out));
                }
                Some('(') => {
                    self.i += 2;
                    return self.complex();
                }
                _ => {
                    self.i += 1;
                    self.pending_redirect = Some((fd, RedirKind::Out));
                }
            }
            return Ok(());
        }
        match (self.peek(1), self.peek(2)) {
            (Some('<'), Some('<')) => {
                self.i += 3;
                self.pending_redirect = Some((fd, RedirKind::HereString));
            }
            (Some('<'), _) => {
                self.i += 2;
                let strip_tabs = self.peek(0) == Some('-');
                if strip_tabs {
                    self.i += 1;
                }
                self.skip_blanks();
                let (delimiter, quoted) = self.read_heredoc_delimiter();
                if delimiter.is_empty() {
                    return if self.lenient { Ok(()) } else { Err(Complex) };
                }
                self.tokens.push(Token::Redirect(Redirect { fd, kind: RedirKind::HereDoc, target: delimiter.clone() }));
                self.heredocs.push(Heredoc { delimiter, strip_tabs, quoted });
            }
            (Some('&'), _) => {
                self.i += 2;
                self.skip_blanks();
                let target = self.read_bare_word();
                self.tokens.push(Token::Redirect(Redirect { fd, kind: RedirKind::Dup, target: format!("&{target}") }));
            }
            (Some('('), _) => {
                self.i += 2;
                return self.complex();
            }
            (Some('>'), _) => {
                self.i += 2;
                self.pending_redirect = Some((fd, RedirKind::Out));
            }
            _ => {
                self.i += 1;
                self.pending_redirect = Some((fd, RedirKind::In));
            }
        }
        Ok(())
    }

    fn read_heredoc_delimiter(&mut self) -> (String, bool) {
        let mut out = String::new();
        let mut quoted = false;
        while let Some(c) = self.peek(0) {
            match c {
                '\'' | '"' => {
                    quoted = true;
                    self.i += 1;
                    while let Some(ch) = self.peek(0) {
                        self.i += 1;
                        if ch == c {
                            break;
                        }
                        out.push(ch);
                    }
                }
                '\\' => {
                    quoted = true;
                    self.i += 1;
                    if let Some(ch) = self.peek(0) {
                        out.push(ch);
                        self.i += 1;
                    }
                }
                c if is_posix_operator_char(c) => break,
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        (out, quoted)
    }

    /// Called right after a newline: consume pending heredoc bodies.
    fn read_heredoc_bodies(&mut self) -> Result<(), Complex> {
        let heredocs = std::mem::take(&mut self.heredocs);
        for heredoc in heredocs {
            while self.i < self.chars.len() {
                let start = self.i;
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line: String = self.chars[start..self.i].iter().collect();
                if self.peek(0) == Some('\n') {
                    self.i += 1;
                }
                let line = line.strip_suffix('\r').unwrap_or(&line);
                let candidate = if heredoc.strip_tabs { line.trim_start_matches('\t') } else { line };
                if candidate == heredoc.delimiter {
                    break;
                }
                if !heredoc.quoted && (line.contains("$(") || line.contains('`')) && !self.lenient {
                    return Err(Complex);
                }
            }
        }
        Ok(())
    }

    // ----- PowerShell ---------------------------------------------------

    fn lex_powershell(mut self, depth: u8) -> Result<Vec<Token>, Complex> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.flush();
                    self.i += 1;
                }
                '\n' => {
                    self.push_op(Op::Seq)?;
                    self.i += 1;
                }
                '#' if !self.in_word => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '<' if self.peek(1) == Some('#') => {
                    self.flush();
                    self.i += 2;
                    while self.i < self.chars.len() && !(self.peek(0) == Some('#') && self.peek(1) == Some('>')) {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(self.chars.len());
                }
                '`' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some('\r') if self.peek(2) == Some('\n') => self.i += 3,
                    Some(next) => {
                        self.push_char(powershell_escape(next));
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                '\'' => {
                    self.start_quote();
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None if self.lenient => break,
                            None => return Err(Complex),
                            Some('\'') if self.peek(1) == Some('\'') => {
                                self.word.push('\'');
                                self.i += 2;
                            }
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(ch) => {
                                self.word.push(ch);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    self.start_quote();
                    self.i += 1;
                    self.lex_powershell_double_quoted()?;
                }
                '@' if !self.in_word
                    && matches!(self.peek(1), Some('\'') | Some('"'))
                    && self.here_string_follows() =>
                {
                    self.lex_here_string()?;
                }
                '@' if !self.in_word && matches!(self.peek(1), Some('(') | Some('{')) => {
                    self.complex()?;
                    self.i += 2;
                }
                '$' => match self.peek(1) {
                    Some('(') => {
                        self.complex()?;
                        self.i += 2;
                    }
                    Some('{') => {
                        self.push_char('$');
                        self.i += 1;
                        while let Some(ch) = self.peek(0) {
                            self.word.push(ch);
                            self.i += 1;
                            if ch == '}' {
                                break;
                            }
                        }
                    }
                    _ => {
                        self.push_char('$');
                        self.i += 1;
                    }
                },
                '(' | ')' | '}' => {
                    self.complex()?;
                    self.i += 1;
                }
                '{' => {
                    self.flush();
                    if depth >= MAX_BLOCK_DEPTH && !self.lenient {
                        return Err(Complex);
                    }
                    let block = self.read_block()?;
                    self.tokens.push(Token::Block(block));
                }
                '&' => {
                    if self.peek(1) == Some('&') {
                        self.push_op(Op::And)?;
                        self.i += 2;
                    } else {
                        self.flush();
                        if self.at_command_start() {
                            self.tokens.push(Token::CallOp);
                        } else {
                            self.push_op(Op::Background)?;
                        }
                        self.i += 1;
                    }
                }
                '|' => {
                    if self.peek(1) == Some('|') {
                        self.push_op(Op::Or)?;
                        self.i += 2;
                    } else {
                        self.push_op(Op::Pipe)?;
                        self.i += 1;
                    }
                }
                ';' => {
                    self.push_op(Op::Seq)?;
                    self.i += 1;
                }
                '>' | '<' => self.lex_powershell_redirect()?,
                _ => {
                    self.push_char(c);
                    self.i += 1;
                }
            }
        }
        self.finish()
    }

    fn lex_powershell_double_quoted(&mut self) -> Result<(), Complex> {
        loop {
            match self.peek(0) {
                None if self.lenient => return Ok(()),
                None => return Err(Complex),
                Some('`') => {
                    if let Some(next) = self.peek(1) {
                        self.word.push(powershell_escape(next));
                    }
                    self.i += 2;
                }
                Some('"') if self.peek(1) == Some('"') => {
                    self.word.push('"');
                    self.i += 2;
                }
                Some('"') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('$') if self.peek(1) == Some('(') => {
                    if !self.lenient {
                        return Err(Complex);
                    }
                    self.word.push('$');
                    self.i += 1;
                }
                Some(ch) => {
                    self.word.push(ch);
                    self.i += 1;
                }
            }
        }
    }

    fn here_string_follows(&self) -> bool {
        let mut j = self.i + 2;
        while let Some(&c) = self.chars.get(j) {
            match c {
                ' ' | '\t' | '\r' => j += 1,
                '\n' => return true,
                _ => return false,
            }
        }
        false
    }

    fn lex_here_string(&mut self) -> Result<(), Complex> {
        let quote = self.peek(1).unwrap_or('\'');
        while self.peek(0).is_some_and(|c| c != '\n') {
            self.i += 1;
        }
        self.i += 1;
        let mut lines: Vec<String> = Vec::new();
        let mut terminated = false;
        while self.i < self.chars.len() {
            let start = self.i;
            while self.peek(0).is_some_and(|c| c != '\n') {
                self.i += 1;
            }
            let line: String = self.chars[start..self.i].iter().collect();
            let trimmed = line.trim_start();
            if trimmed.starts_with(quote) && trimmed[quote.len_utf8()..].starts_with('@') {
                // Continue lexing right after the terminator (`self.i` counts chars).
                let lead_chars = line.chars().count() - trimmed.chars().count();
                self.i = start + lead_chars + 2;
                terminated = true;
                break;
            }
            if self.peek(0) == Some('\n') {
                self.i += 1;
            }
            if quote == '"' && line.contains("$(") && !self.lenient {
                return Err(Complex);
            }
            lines.push(line.strip_suffix('\r').unwrap_or(&line).to_string());
        }
        if !terminated && !self.lenient {
            return Err(Complex);
        }
        self.start_quote();
        self.word.push_str(&lines.join("\n"));
        Ok(())
    }

    /// Read a balanced `{ ... }` block starting at `{`; returns its contents.
    fn read_block(&mut self) -> Result<String, Complex> {
        self.i += 1;
        let start = self.i;
        let mut depth = 1usize;
        while let Some(c) = self.peek(0) {
            match c {
                '\'' => {
                    self.i += 1;
                    while let Some(ch) = self.peek(0) {
                        self.i += 1;
                        if ch == '\'' {
                            break;
                        }
                    }
                    continue;
                }
                '"' => {
                    self.i += 1;
                    while let Some(ch) = self.peek(0) {
                        self.i += 1;
                        if ch == '`' {
                            self.i += 1;
                        } else if ch == '"' {
                            break;
                        }
                    }
                    continue;
                }
                '`' => self.i += 1,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let body: String = self.chars[start..self.i].iter().collect();
                        self.i += 1;
                        return Ok(body);
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        if self.lenient {
            let end = self.chars.len().min(self.i);
            Ok(self.chars[start.min(end)..end].iter().collect())
        } else {
            Err(Complex)
        }
    }

    fn lex_powershell_redirect(&mut self) -> Result<(), Complex> {
        let c = self.peek(0).unwrap_or('>');
        let fd = self.take_fd_prefix(true);
        if c == '<' {
            self.i += 1;
            self.pending_redirect = Some((fd, RedirKind::In));
            return Ok(());
        }
        match self.peek(1) {
            Some('>') => {
                self.i += 2;
                self.pending_redirect = Some((fd, RedirKind::Append));
            }
            Some('&') => {
                self.i += 2;
                let target = self.read_bare_word();
                self.tokens.push(Token::Redirect(Redirect { fd, kind: RedirKind::Dup, target: format!("&{target}") }));
            }
            _ => {
                self.i += 1;
                self.pending_redirect = Some((fd, RedirKind::Out));
            }
        }
        Ok(())
    }

    // ----- cmd.exe ------------------------------------------------------

    fn lex_cmd(mut self) -> Result<Vec<Token>, Complex> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => {
                    self.flush();
                    self.i += 1;
                }
                '\n' => {
                    self.push_op(Op::Seq)?;
                    self.i += 1;
                }
                '^' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(next) => {
                        self.push_char(next);
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                '"' => {
                    self.start_quote();
                    self.i += 1;
                    while let Some(ch) = self.peek(0) {
                        self.i += 1;
                        if ch == '"' {
                            break;
                        }
                        self.word.push(ch);
                    }
                }
                '&' => {
                    if self.peek(1) == Some('&') {
                        self.push_op(Op::And)?;
                        self.i += 2;
                    } else {
                        self.push_op(Op::Seq)?;
                        self.i += 1;
                    }
                }
                '|' => {
                    if self.peek(1) == Some('|') {
                        self.push_op(Op::Or)?;
                        self.i += 2;
                    } else {
                        self.push_op(Op::Pipe)?;
                        self.i += 1;
                    }
                }
                '(' | ')' => {
                    self.complex()?;
                    self.i += 1;
                }
                '>' | '<' => {
                    let fd = self.take_fd_prefix(false);
                    if c == '<' {
                        self.i += 1;
                        self.pending_redirect = Some((fd, RedirKind::In));
                    } else if self.peek(1) == Some('>') {
                        self.i += 2;
                        self.pending_redirect = Some((fd, RedirKind::Append));
                    } else if self.peek(1) == Some('&') {
                        self.i += 2;
                        let target = self.read_bare_word();
                        self.tokens.push(Token::Redirect(Redirect {
                            fd,
                            kind: RedirKind::Dup,
                            target: format!("&{target}"),
                        }));
                    } else {
                        self.i += 1;
                        self.pending_redirect = Some((fd, RedirKind::Out));
                    }
                }
                _ => {
                    self.push_char(c);
                    self.i += 1;
                }
            }
        }
        self.finish()
    }
}

/// `{` / `}` standing alone as a word (group command), not part of a word.
fn standalone_brace(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(c) => c.is_whitespace() || c == ';',
    }
}

fn powershell_escape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        other => other,
    }
}

fn is_env_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Parse a command line into simple commands.
pub(crate) fn parse_commands(src: &str, shell: ShellKind, lenient: bool) -> Result<Vec<SimpleCommand>, Complex> {
    parse_at_depth(src, shell, lenient, 0)
}

fn parse_at_depth(src: &str, shell: ShellKind, lenient: bool, depth: u8) -> Result<Vec<SimpleCommand>, Complex> {
    let src = src.replace("\r\n", "\n");
    let lexer = Lexer::new(&src, lenient);
    let tokens = match shell {
        ShellKind::PowerShell => lexer.lex_powershell(depth)?,
        ShellKind::Cmd => lexer.lex_cmd()?,
        ShellKind::Bash | ShellKind::Sh | ShellKind::Zsh => lexer.lex_posix()?,
    };
    build_commands(tokens, shell, lenient, depth)
}

#[derive(Default)]
struct Builder {
    words: Vec<(String, bool)>,
    redirects: Vec<Redirect>,
    nested: Vec<SimpleCommand>,
    op_before: Option<Op>,
    call_op: bool,
}

fn build_commands(
    tokens: Vec<Token>,
    shell: ShellKind,
    lenient: bool,
    depth: u8,
) -> Result<Vec<SimpleCommand>, Complex> {
    let mut out = Vec::new();
    let mut builder = Builder::default();
    for token in tokens {
        match token {
            Token::Word { text, quoted } => builder.words.push((text, quoted)),
            Token::CallOp => builder.call_op = true,
            Token::Redirect(redirect) => builder.redirects.push(redirect),
            Token::Block(body) => {
                builder.words.push((format!("{{{body}}}"), false));
                if depth >= MAX_BLOCK_DEPTH {
                    if !lenient {
                        return Err(Complex);
                    }
                } else {
                    builder.nested.extend(parse_at_depth(&body, shell, lenient, depth + 1)?);
                }
            }
            Token::Op(op) => {
                finish_command(&mut out, std::mem::take(&mut builder), shell, lenient)?;
                builder.op_before = Some(op);
            }
        }
    }
    finish_command(&mut out, builder, shell, lenient)?;
    Ok(out)
}

/// POSIX reserved words that may prefix a simple command (`then rm x`).
const POSIX_PREFIX_KEYWORDS: [&str; 8] = ["!", "if", "then", "else", "elif", "do", "while", "until"];
/// Reserved words closing a compound command; they run nothing themselves.
const POSIX_CLOSING_KEYWORDS: [&str; 3] = ["done", "fi", "esac"];
/// Compound commands whose words are not a command invocation.
const POSIX_COMPOUND_KEYWORDS: [&str; 6] = ["for", "case", "select", "function", "coproc", "[["];

fn finish_command(
    out: &mut Vec<SimpleCommand>,
    builder: Builder,
    shell: ShellKind,
    lenient: bool,
) -> Result<(), Complex> {
    let Builder { mut words, redirects, nested, op_before, call_op } = builder;
    let mut expression = false;
    match shell {
        ShellKind::Bash | ShellKind::Sh | ShellKind::Zsh => loop {
            let assignments = words.iter().take_while(|(w, quoted)| !*quoted && is_env_assignment(w)).count();
            words.drain(..assignments);
            let Some((first, quoted)) = words.first() else { break };
            if *quoted {
                break;
            }
            if POSIX_PREFIX_KEYWORDS.contains(&first.as_str()) {
                words.remove(0);
            } else if POSIX_CLOSING_KEYWORDS.contains(&first.as_str()) && words.len() == 1 {
                words.clear();
            } else if POSIX_COMPOUND_KEYWORDS.contains(&first.as_str()) {
                // Loops / case statements: analysed only leniently.
                if !lenient {
                    return Err(Complex);
                }
                words.clear();
            } else {
                break;
            }
        },
        ShellKind::PowerShell => {
            if words.len() >= 2
                && words[0].0.starts_with('$')
                && matches!(words[1].0.as_str(), "=" | "+=" | "-=" | "*=" | "/=" | "??=")
            {
                words.drain(..2);
            }
            if let Some((first, quoted)) = words.first() {
                expression = !call_op && (first.starts_with('$') || *quoted || first.parse::<f64>().is_ok());
            }
        }
        ShellKind::Cmd => {
            if let Some((first, _)) = words.first_mut() {
                if let Some(stripped) = first.strip_prefix('@') {
                    *first = stripped.to_string();
                }
            }
            if words.first().is_some_and(|(w, _)| w.eq_ignore_ascii_case("rem") || w.starts_with("::")) {
                return Ok(());
            }
            if words.first().is_some_and(|(w, _)| w.is_empty()) {
                words.remove(0);
            }
        }
    }
    if words.is_empty() && redirects.is_empty() && nested.is_empty() {
        return Ok(());
    }
    out.push(SimpleCommand {
        argv: words.into_iter().map(|(w, _)| w).collect(),
        redirects,
        op_before,
        expression,
        nested,
    });
    Ok(())
}

/// Flatten commands (including script-block contents) into argv lists,
/// skipping PowerShell expression segments.
pub(crate) fn flatten_argvs(commands: &[SimpleCommand], out: &mut Vec<Vec<String>>) {
    for command in commands {
        if !command.expression && !command.argv.is_empty() {
            out.push(command.argv.clone());
        }
        flatten_argvs(&command.nested, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn split(src: &str, shell: ShellKind) -> Option<Vec<Vec<String>>> {
        let commands = parse_commands(src, shell, false).ok()?;
        let mut out = Vec::new();
        flatten_argvs(&commands, &mut out);
        Some(out)
    }

    fn v(groups: &[&[&str]]) -> Option<Vec<Vec<String>>> {
        Some(groups.iter().map(|g| g.iter().map(|s| s.to_string()).collect()).collect())
    }

    #[test]
    fn posix_operators_and_quotes() {
        assert_eq!(
            split("git status && echo 'a b' || ls -la; pwd | wc -l & true", ShellKind::Bash),
            v(&[&["git", "status"], &["echo", "a b"], &["ls", "-la"], &["pwd"], &["wc", "-l"], &["true"]])
        );
        assert_eq!(split("echo \"x \\\"y\\\" $HOME\" a\\ b", ShellKind::Sh), v(&[&["echo", "x \"y\" $HOME", "a b"]]));
        assert_eq!(
            split("ls\nfoo   bar\n\n# comment\nbaz # trailing", ShellKind::Bash),
            v(&[&["ls"], &["foo", "bar"], &["baz"]])
        );
        assert_eq!(split("echo ${HOME}/x ''", ShellKind::Zsh), v(&[&["echo", "${HOME}/x", ""]]));
    }

    #[test]
    fn posix_env_assignments_are_stripped() {
        assert_eq!(split("FOO=1 BAR='x y' cargo test", ShellKind::Bash), v(&[&["cargo", "test"]]));
        assert_eq!(split("FOO=1", ShellKind::Bash), v(&[]));
        assert_eq!(split("echo FOO=1", ShellKind::Bash), v(&[&["echo", "FOO=1"]]));
    }

    #[test]
    fn posix_complex_constructs() {
        for src in [
            "echo $(whoami)",
            "echo `whoami`",
            "(cd x && ls)",
            "{ ls; }",
            "diff <(ls a) <(ls b)",
            "echo \"$(rm -rf /)\"",
            "f() { ls; }",
            "echo 'unterminated",
            "echo >",
        ] {
            assert_eq!(split(src, ShellKind::Bash), None, "{src}");
        }
        // Lenient mode splits at those constructs instead.
        let lenient = parse_commands("echo $(rm -rf /)", ShellKind::Bash, true).unwrap();
        assert!(lenient.iter().any(|c| c.argv == ["rm", "-rf", "/"]));
    }

    #[test]
    fn posix_reserved_words() {
        assert_eq!(
            split("if [ -f x ]; then cat x; else echo no; fi", ShellKind::Bash),
            v(&[&["[", "-f", "x", "]"], &["cat", "x"], &["echo", "no"]])
        );
        assert_eq!(split("while true; do ls; done", ShellKind::Bash), v(&[&["true"], &["ls"]]));
        assert_eq!(split("for f in *; do rm $f; done", ShellKind::Bash), None);
        let lenient = parse_commands("for f in *; do rm -rf ~; done", ShellKind::Bash, true).unwrap();
        assert_eq!(lenient.iter().map(|c| c.argv.clone()).collect::<Vec<_>>(), vec![vec!["rm", "-rf", "~"]]);
        // Quoted keywords are ordinary words.
        assert_eq!(split("echo 'if' && 'do' x", ShellKind::Bash), v(&[&["echo", "if"], &["do", "x"]]));
    }

    #[test]
    fn posix_redirections() {
        let cmds = parse_commands(
            "echo hi > out.txt 2>&1; cat < in.txt >> log 2>/dev/null; ls &> all",
            ShellKind::Bash,
            false,
        )
        .unwrap();
        assert_eq!(cmds[0].argv, ["echo", "hi"]);
        assert_eq!(cmds[0].redirects.len(), 2);
        assert!(cmds[0].redirects[0].writes_file());
        assert_eq!(cmds[0].redirects[1].kind, RedirKind::Dup);
        assert!(!cmds[0].redirects[1].writes_file());
        assert_eq!(cmds[1].argv, ["cat"]);
        assert_eq!(cmds[1].redirects.iter().filter(|r| r.writes_file()).count(), 1);
        assert_eq!(cmds[2].redirects[0].target, "all");
        assert!(cmds[2].redirects[0].writes_file());
    }

    #[test]
    fn posix_heredocs() {
        let src = "cat > notes.md <<'EOF'\nrm -rf / is bad\n$(not run)\nEOF\necho done";
        assert_eq!(split(src, ShellKind::Bash), v(&[&["cat"], &["echo", "done"]]));
        let src = "cat <<-EOF\n\tplain $HOME\n\tEOF\nls";
        assert_eq!(split(src, ShellKind::Bash), v(&[&["cat"], &["ls"]]));
        // Unquoted heredoc with command substitution is complex.
        assert_eq!(split("cat <<EOF\n$(whoami)\nEOF", ShellKind::Bash), None);
    }

    #[test]
    fn powershell_splitting() {
        assert_eq!(
            split(
                "Get-ChildItem -Recurse | Select-String 'foo bar'; git status && echo \"a`\"b\"",
                ShellKind::PowerShell
            ),
            v(&[&["Get-ChildItem", "-Recurse"], &["Select-String", "foo bar"], &["git", "status"], &["echo", "a\"b"]])
        );
        assert_eq!(split("echo 'it''s'", ShellKind::PowerShell), v(&[&["echo", "it's"]]));
        assert_eq!(
            split("& \"C:\\Program Files\\x.exe\" --flag", ShellKind::PowerShell),
            v(&[&["C:\\Program Files\\x.exe", "--flag"]])
        );
        assert_eq!(split("$x = Get-Content a.txt", ShellKind::PowerShell), v(&[&["Get-Content", "a.txt"]]));
        assert_eq!(split("'literal' | Out-File x.txt", ShellKind::PowerShell), v(&[&["Out-File", "x.txt"]]));
        assert_eq!(split("ls <# block #> -Force # comment", ShellKind::PowerShell), v(&[&["ls", "-Force"]]));
    }

    #[test]
    fn powershell_script_blocks_are_analysed() {
        assert_eq!(
            split(
                "Get-ChildItem | Where-Object { $_.Length -gt 1kb } | ForEach-Object { Remove-Item $_ }",
                ShellKind::PowerShell
            ),
            v(&[
                &["Get-ChildItem"],
                &["Where-Object", "{ $_.Length -gt 1kb }"],
                &["ForEach-Object", "{ Remove-Item $_ }"],
                &["Remove-Item", "$_"],
            ])
        );
    }

    #[test]
    fn powershell_complex_constructs() {
        for src in ["iex (iwr https://x)", "echo $(Get-Date)", "@(1,2) | % { $_ }", "\"$(whoami)\"", "@{a=1}", "ls }"] {
            assert_eq!(split(src, ShellKind::PowerShell), None, "{src}");
        }
    }

    #[test]
    fn powershell_here_string_and_redirects() {
        let src = "@'\nanything (here) $(x)\n'@ | Set-Content out.txt\nGet-Item x 2>&1 *> $null";
        let cmds = parse_commands(src, ShellKind::PowerShell, false).unwrap();
        assert!(cmds[0].expression);
        assert_eq!(cmds[1].argv, ["Set-Content", "out.txt"]);
        assert_eq!(cmds[2].argv, ["Get-Item", "x"]);
        assert!(cmds[2].redirects.iter().all(|r| !r.writes_file()));
        let cmds = parse_commands("echo hi > out.txt", ShellKind::PowerShell, false).unwrap();
        assert!(cmds[0].redirects[0].writes_file());
    }

    #[test]
    fn cmd_splitting() {
        assert_eq!(
            split("@echo off & dir /s \"C:\\Program Files\" && type a.txt | findstr x || ver", ShellKind::Cmd),
            v(&[
                &["echo", "off"],
                &["dir", "/s", "C:\\Program Files"],
                &["type", "a.txt"],
                &["findstr", "x"],
                &["ver"]
            ])
        );
        assert_eq!(split("echo a^&b", ShellKind::Cmd), v(&[&["echo", "a&b"]]));
        assert_eq!(split("rem nothing here\necho hi", ShellKind::Cmd), v(&[&["echo", "hi"]]));
        assert_eq!(split("(echo a)", ShellKind::Cmd), None);
        let cmds = parse_commands("echo x > out.txt 2>nul", ShellKind::Cmd, false).unwrap();
        assert_eq!(cmds[0].redirects.iter().filter(|r| r.writes_file()).count(), 1);
    }
}
