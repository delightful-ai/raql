use crate::diag::{Diagnostic, DiagnosticLabel};
use crate::source::{RaqlFileId, SrcSpan};
use camino::Utf8PathBuf;
use smol_str::SmolStr;
use text_size::{TextRange, TextSize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Ident(SmolStr),
    Int(i64),
    Str(SmolStr),
    Dot,
    Comma,
    Colon,
    ColonColon,
    ColonEq,
    Semicolon,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
    NotEq,
    Plus,
    Minus,
    Star,
    Slash,
    Question,
    RuleArrow,
    Eof,
}

impl TokenKind {
    #[must_use]
    pub(crate) fn ident_name(&self) -> Option<&str> {
        match self {
            Self::Ident(name) => Some(name.as_str()),
            _ => None,
        }
    }
}

pub(crate) fn lex(
    file: RaqlFileId,
    source: &str,
    include_stack: &[Utf8PathBuf],
) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        file,
        source,
        offset: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
        include_stack: include_stack.to_vec(),
    };
    lexer.lex_all();
    (lexer.tokens, lexer.diagnostics)
}

struct Lexer<'a> {
    file: RaqlFileId,
    source: &'a str,
    offset: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
    include_stack: Vec<Utf8PathBuf>,
}

impl<'a> Lexer<'a> {
    fn lex_all(&mut self) {
        while self.offset < self.source.len() {
            self.skip_whitespace_and_comments();
            if self.offset >= self.source.len() {
                break;
            }

            if self.try_punctuation() {
                continue;
            }

            let Some(ch) = self.peek_char() else {
                break;
            };

            if is_ident_start(ch) {
                self.lex_ident();
                continue;
            }
            if ch.is_ascii_digit() {
                self.lex_int();
                continue;
            }
            if ch == '"' {
                self.lex_string();
                continue;
            }

            let start = self.offset;
            self.bump_char();
            let span = SrcSpan::new(self.file, start as u32, self.offset as u32);
            self.diagnostics.push(
                Diagnostic::lex(
                    self.unexpected_character_message(ch),
                    Some(DiagnosticLabel::new(span, "invalid token")),
                )
                .with_include_stack(self.include_stack.clone()),
            );
        }

        let eof = self.source.len() as u32;
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            range: TextRange::new(TextSize::from(eof), TextSize::from(eof)),
        });
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            let Some(ch) = self.peek_char() else {
                return;
            };
            if ch.is_whitespace() {
                self.bump_char();
                continue;
            }

            if self.starts_with("%") {
                self.skip_line_comment();
                continue;
            }

            if self.starts_with("//") {
                self.skip_line_comment();
                continue;
            }

            if self.starts_with("/*") {
                self.skip_block_comment();
                continue;
            }

            return;
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.peek_char() {
            self.bump_char();
            if ch == '\n' {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) {
        let start = self.offset;
        self.offset += 2;
        let mut depth = 1usize;

        while self.offset < self.source.len() {
            if self.starts_with("/*") {
                depth += 1;
                self.offset += 2;
                continue;
            }
            if self.starts_with("*/") {
                depth -= 1;
                self.offset += 2;
                if depth == 0 {
                    return;
                }
                continue;
            }
            self.bump_char();
        }

        let span = SrcSpan::new(self.file, start as u32, self.source.len() as u32);
        self.diagnostics.push(
            Diagnostic::lex(
                "unterminated block comment; add a closing `*/`",
                Some(DiagnosticLabel::new(span, "comment starts here")),
            )
            .with_include_stack(self.include_stack.clone()),
        );
    }

    fn try_punctuation(&mut self) -> bool {
        let start = self.offset;

        let Some((len, kind)) = self.match_punctuation() else {
            return false;
        };
        self.offset += len;
        self.tokens.push(Token {
            kind,
            range: TextRange::new(
                TextSize::from(start as u32),
                TextSize::from(self.offset as u32),
            ),
        });
        true
    }

    fn match_punctuation(&self) -> Option<(usize, TokenKind)> {
        const TWO_CHAR: [(&str, TokenKind); 6] = [
            (":-", TokenKind::RuleArrow),
            (":=", TokenKind::ColonEq),
            ("::", TokenKind::ColonColon),
            ("<=", TokenKind::LtEq),
            (">=", TokenKind::GtEq),
            ("!=", TokenKind::NotEq),
        ];

        if self.starts_with("//") {
            return None;
        }
        for (text, kind) in TWO_CHAR {
            if self.starts_with(text) {
                return Some((text.len(), kind));
            }
        }

        let ch = self.peek_char()?;
        let kind = match ch {
            '.' => TokenKind::Dot,
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            '<' => TokenKind::Lt,
            '>' => TokenKind::Gt,
            '=' => TokenKind::Eq,
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '?' => TokenKind::Question,
            _ => return None,
        };
        Some((ch.len_utf8(), kind))
    }

    fn lex_ident(&mut self) {
        let start = self.offset;
        self.bump_char();
        while let Some(ch) = self.peek_char() {
            if is_ident_continue(ch) {
                self.bump_char();
            } else {
                break;
            }
        }

        let text = &self.source[start..self.offset];
        self.tokens.push(Token {
            kind: TokenKind::Ident(SmolStr::new(text)),
            range: TextRange::new(
                TextSize::from(start as u32),
                TextSize::from(self.offset as u32),
            ),
        });
    }

    fn lex_int(&mut self) {
        let start = self.offset;
        self.bump_char();
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                self.bump_char();
            } else {
                break;
            }
        }
        let text = &self.source[start..self.offset];
        match text.parse::<i64>() {
            Ok(value) => self.tokens.push(Token {
                kind: TokenKind::Int(value),
                range: TextRange::new(
                    TextSize::from(start as u32),
                    TextSize::from(self.offset as u32),
                ),
            }),
            Err(_) => {
                let span = SrcSpan::new(self.file, start as u32, self.offset as u32);
                self.diagnostics.push(
                    Diagnostic::lex(
                        format!("integer literal `{text}` is out of range for i64"),
                        Some(DiagnosticLabel::new(span, "invalid integer literal")),
                    )
                    .with_include_stack(self.include_stack.clone()),
                );
            }
        }
    }

    fn lex_string(&mut self) {
        let start = self.offset;
        self.bump_char();
        let mut out = String::new();
        let mut terminated = false;

        while let Some(ch) = self.peek_char() {
            self.bump_char();
            if ch == '"' {
                terminated = true;
                break;
            }

            if ch == '\\' {
                let escape_start = self.offset - 1;
                let Some(next) = self.peek_char() else {
                    let span = SrcSpan::new(self.file, start as u32, self.offset as u32);
                    self.diagnostics.push(
                        Diagnostic::lex(
                            Self::unterminated_string_message(),
                            Some(DiagnosticLabel::new(span, "string starts here")),
                        )
                        .with_include_stack(self.include_stack.clone()),
                    );
                    return;
                };
                self.bump_char();
                match next {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    _ => {
                        let span = SrcSpan::new(self.file, escape_start as u32, self.offset as u32);
                        self.diagnostics.push(
                            Diagnostic::lex(
                                format!(
                                    "unsupported escape sequence `\\{next}`; valid escapes are `\\\\`, `\\\"`, `\\n`, `\\t`, and `\\r`"
                                ),
                                Some(DiagnosticLabel::new(span, "invalid escape")),
                            )
                            .with_include_stack(self.include_stack.clone()),
                        );
                    }
                }
                continue;
            }

            if ch == '\n' {
                let span = SrcSpan::new(self.file, start as u32, self.offset as u32);
                self.diagnostics.push(
                    Diagnostic::lex(
                        Self::unterminated_string_message(),
                        Some(DiagnosticLabel::new(span, "newline in string literal")),
                    )
                    .with_include_stack(self.include_stack.clone()),
                );
                return;
            }

            out.push(ch);
        }

        if !terminated {
            let span = SrcSpan::new(self.file, start as u32, self.offset as u32);
            self.diagnostics.push(
                Diagnostic::lex(
                    Self::unterminated_string_message(),
                    Some(DiagnosticLabel::new(span, "string starts here")),
                )
                .with_include_stack(self.include_stack.clone()),
            );
            return;
        }

        self.tokens.push(Token {
            kind: TokenKind::Str(SmolStr::new(out.as_str())),
            range: TextRange::new(
                TextSize::from(start as u32),
                TextSize::from(self.offset as u32),
            ),
        });
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn bump_char(&mut self) {
        if let Some(ch) = self.peek_char() {
            self.offset += ch.len_utf8();
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.source[self.offset..].starts_with(text)
    }

    fn unexpected_character_message(&self, ch: char) -> String {
        if ch == '\'' {
            return "unexpected character `'`; RAQL strings must use double quotes, not single quotes (example: replace `'value'` with `\"value\"`)"
                .to_owned();
        }
        format!(
            "unexpected character `{ch}`; accepted tokens begin with letters/`_`, digits, `\"` for strings, `%`/`//`/`/*` for comments, or punctuation like `.`, `,`, `(`, `)`, `[`, `]`, `{{`, `}}`, `:`, `:-`, `=`, `!=`, `<`, `>`, `+`, `-`, `*`, `/`"
        )
    }

    const fn unterminated_string_message() -> &'static str {
        "unterminated string literal; add a closing `\"` (escape embedded quotes as `\\\"`)"
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}
