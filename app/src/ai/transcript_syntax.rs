//! Lightweight, language-agnostic syntax highlighting for the read-only
//! external-session transcript preview.
//!
//! This is deliberately heuristic. A real highlighter (see `crates/syntax_tree`,
//! which drives the editor) needs a buffer, a tree-sitter parser, and a model
//! context — far too much machinery for a static preview pane. Instead we color
//! the handful of token classes that read clearly at a glance — comments,
//! strings, numbers, and a curated set of cross-language keywords — which is
//! enough to make a fenced code block look like code rather than prose.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::OnceLock;

/// A coarse token class. Each maps to one highlight color at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxToken {
    Keyword,
    StringLiteral,
    Comment,
    Number,
}

/// One piece of a transcript message body: free-form prose or a fenced code
/// block (the text between a pair of triple-backtick fences).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptSegment {
    Prose(String),
    Code {
        /// The info string after the opening fence (e.g. `rust`), if any.
        language: Option<String>,
        code: String,
    },
}

/// Splits a message body into prose and fenced-code segments.
///
/// Fences are recognized as lines whose first non-whitespace content is exactly
/// ` ``` ` (optionally followed by an info string). An unterminated fence runs to
/// the end of the text, matching how chat UIs render a still-streaming block.
pub fn split_segments(text: &str) -> Vec<TranscriptSegment> {
    let mut segments = Vec::new();
    let mut prose = String::new();
    let mut code: Option<(Option<String>, String)> = None;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            match code.take() {
                // Closing fence: emit the accumulated code block.
                Some((language, body)) => {
                    segments.push(TranscriptSegment::Code {
                        language,
                        code: body,
                    });
                }
                // Opening fence: flush any pending prose and start a block.
                None => {
                    if !prose.is_empty() {
                        segments.push(TranscriptSegment::Prose(std::mem::take(&mut prose)));
                    }
                    let language = rest.trim().trim_end_matches('\n').trim();
                    let language = (!language.is_empty()).then(|| language.to_string());
                    code = Some((language, String::new()));
                }
            }
        } else if let Some((_, body)) = code.as_mut() {
            body.push_str(line);
        } else {
            prose.push_str(line);
        }
    }

    // Trailing content: an unterminated block, else remaining prose.
    if let Some((language, code)) = code {
        segments.push(TranscriptSegment::Code { language, code });
    } else if !prose.is_empty() {
        segments.push(TranscriptSegment::Prose(prose));
    }

    segments
}

/// Cross-language keyword set. Intentionally broad rather than per-language —
/// the preview can't know the language reliably and a few extra matches read
/// fine. Identifiers are matched whole, so `iffy` is never colored as `if`.
fn keywords() -> &'static HashSet<&'static str> {
    static KEYWORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    KEYWORDS.get_or_init(|| {
        [
            // Rust
            "fn",
            "let",
            "mut",
            "const",
            "static",
            "pub",
            "use",
            "mod",
            "struct",
            "enum",
            "impl",
            "trait",
            "dyn",
            "where",
            "as",
            "ref",
            "move",
            "unsafe",
            "async",
            "await",
            "match",
            "self",
            "Self",
            "super",
            "crate",
            "type",
            // Control flow shared across many languages
            "if",
            "else",
            "elif",
            "for",
            "while",
            "loop",
            "do",
            "switch",
            "case",
            "default",
            "break",
            "continue",
            "return",
            "yield",
            "throw",
            "try",
            "catch",
            "except",
            "finally",
            "with",
            "raise",
            "in",
            "is",
            "and",
            "or",
            "not",
            // JS / TS
            "function",
            "var",
            "class",
            "extends",
            "implements",
            "interface",
            "new",
            "this",
            "typeof",
            "instanceof",
            "void",
            "delete",
            "export",
            "import",
            "from",
            "of",
            // Python
            "def",
            "lambda",
            "pass",
            "global",
            "nonlocal",
            "del",
            // C-family / Java visibility & types
            "public",
            "private",
            "protected",
            "final",
            "abstract",
            "package",
            "namespace",
            "int",
            "float",
            "double",
            "bool",
            "boolean",
            "char",
            "string",
            "long",
            "short",
            // Literals
            "true",
            "false",
            "null",
            "nil",
            "None",
            "True",
            "False",
            "undefined",
        ]
        .into_iter()
        .collect()
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Produces non-overlapping highlight spans (as char-index ranges) for a code
/// snippet. Ranges are returned in source order so they can feed straight into
/// `Text::with_highlights`.
pub fn highlight_spans(code: &str) -> Vec<(Range<usize>, SyntaxToken)> {
    let chars: Vec<char> = code.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        // Line comments: `//`, `#`. (`#` covers shell/python; harmless elsewhere.)
        if (c == '/' && next == Some('/')) || c == '#' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            spans.push((start..i, SyntaxToken::Comment));
            continue;
        }

        // Block comments: `/* ... */`, terminated by `*/` or end of input.
        if c == '/' && next == Some('*') {
            let start = i;
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            spans.push((start..i, SyntaxToken::Comment));
            continue;
        }

        // String / char literals: `"`, `'`, or backtick. Quotes close on a
        // matching unescaped quote; `"`/`'` also close at end of line so an
        // unbalanced quote can't swallow the rest of the block.
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                if chars[i] == '\n' && quote != '`' {
                    break;
                }
                i += 1;
            }
            spans.push((start..i, SyntaxToken::StringLiteral));
            continue;
        }

        // Numbers: a digit run not glued to the tail of an identifier (so the
        // `2` in `utf8` isn't colored). Consumes hex/decimal/underscore digits.
        if c.is_ascii_digit() && (i == 0 || !is_ident_char(chars[i - 1])) {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            spans.push((start..i, SyntaxToken::Number));
            continue;
        }

        // Identifiers: collect the whole word, then color it only if it's a
        // known keyword. Whole-word matching avoids false hits inside names.
        if is_ident_char(c) && !c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if keywords().contains(word.as_str()) {
                spans.push((start..i, SyntaxToken::Keyword));
            }
            continue;
        }

        i += 1;
    }

    spans
}

/// An inline markdown style applied to a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Bold,
    Italic,
    Code,
}

/// The block-level kind of a single prose line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProseLine {
    /// `#`..`######` heading; carries the level (1-6).
    Heading { level: u8, text: String },
    /// `-`/`*`/`+` unordered list item; `text` excludes the marker.
    Bullet { text: String },
    /// Anything else, including ordered-list items (kept verbatim).
    Normal { text: String },
}

impl ProseLine {
    /// The text content with any block marker stripped.
    pub fn text(&self) -> &str {
        match self {
            ProseLine::Heading { text, .. }
            | ProseLine::Bullet { text }
            | ProseLine::Normal { text } => text,
        }
    }
}

/// Classifies one line of prose by its leading block marker (heading or
/// unordered-list bullet), returning the kind with the marker stripped.
pub fn classify_prose_line(line: &str) -> ProseLine {
    let trimmed = line.trim_start();

    // ATX heading: 1-6 `#` followed by a space.
    if let Some(rest) = trimmed.strip_prefix('#') {
        let extra_hashes = rest.chars().take_while(|&c| c == '#').count();
        let level = 1 + extra_hashes;
        let after = &rest[extra_hashes..];
        if level <= 6 && after.starts_with(' ') {
            return ProseLine::Heading {
                level: level as u8,
                text: after.trim_start().to_string(),
            };
        }
    }

    // Unordered list item: `-`, `*`, or `+` then a space.
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            if rest.starts_with(' ') {
                return ProseLine::Bullet {
                    text: rest.trim_start().to_string(),
                };
            }
        }
    }

    ProseLine::Normal {
        text: line.to_string(),
    }
}

fn is_md_punct(c: char) -> bool {
    matches!(c, '*' | '_' | '`' | '\\' | '[' | ']' | '(' | ')' | '#')
}

/// Parses inline markdown emphasis, returning the display text with markers
/// removed and the styled char-index ranges (over the display text).
///
/// Handles code spans (`` `code` ``), bold (`**`/`__`), and italic (`*`/`_`).
/// Emphasis is intentionally non-nested — agent output rarely nests it, and the
/// flat model keeps the highlight ranges simple. Underscore emphasis requires a
/// word boundary before the opener so `snake_case_names` are left alone.
pub fn parse_inline_markdown(text: &str) -> (String, Vec<(Range<usize>, InlineStyle)>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut out_len = 0usize;
    let mut styles = Vec::new();
    let mut i = 0;

    // Finds the next unescaped occurrence of `target` at or after `from`.
    let find = |from: usize, target: char| -> Option<usize> {
        let mut j = from;
        while j < chars.len() {
            if chars[j] == '\\' {
                j += 2;
                continue;
            }
            if chars[j] == target {
                return Some(j);
            }
            j += 1;
        }
        None
    };

    while i < chars.len() {
        let c = chars[i];

        // Escaped punctuation renders literally.
        if c == '\\' {
            if let Some(&escaped) = chars.get(i + 1) {
                if is_md_punct(escaped) {
                    out.push(escaped);
                    out_len += 1;
                    i += 2;
                    continue;
                }
            }
        }

        // Code span: verbatim, no inner parsing.
        if c == '`' {
            if let Some(close) = find(i + 1, '`') {
                if close > i + 1 {
                    let start = out_len;
                    out.extend(&chars[i + 1..close]);
                    out_len += close - (i + 1);
                    styles.push((start..out_len, InlineStyle::Code));
                    i = close + 1;
                    continue;
                }
            }
        }

        // Bold: `**` or `__`, opener must be followed by non-space.
        if (c == '*' || c == '_') && chars.get(i + 1) == Some(&c) {
            let inner_start = i + 2;
            if chars.get(inner_start).is_some_and(|n| !n.is_whitespace()) {
                if let Some(close) = find_run(&chars, inner_start, c) {
                    let start = out_len;
                    out.extend(&chars[inner_start..close]);
                    out_len += close - inner_start;
                    styles.push((start..out_len, InlineStyle::Bold));
                    i = close + 2;
                    continue;
                }
            }
        }

        // Italic: single `*`/`_`. Require a non-space inner and, for `_`, a
        // word boundary before the opener to spare snake_case identifiers.
        if c == '*' || c == '_' {
            let boundary_ok = c == '*' || i == 0 || !is_ident_char(chars[i - 1]);
            if boundary_ok && chars.get(i + 1).is_some_and(|n| !n.is_whitespace()) {
                if let Some(close) = find(i + 1, c) {
                    let closes_word = chars.get(close + 1).is_none_or(|n| !is_ident_char(*n));
                    if close > i + 1 && closes_word {
                        let start = out_len;
                        out.extend(&chars[i + 1..close]);
                        out_len += close - (i + 1);
                        styles.push((start..out_len, InlineStyle::Italic));
                        i = close + 1;
                        continue;
                    }
                }
            }
        }

        out.push(c);
        out_len += 1;
        i += 1;
    }

    (out, styles)
}

/// Finds a `marker marker` run (e.g. `**`) at or after `from`, returning the
/// index of the first marker char. Skips escaped chars.
fn find_run(chars: &[char], from: usize, marker: char) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '\\' {
            j += 2;
            continue;
        }
        if chars[j] == marker && chars.get(j + 1) == Some(&marker) {
            return Some(j);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests;
