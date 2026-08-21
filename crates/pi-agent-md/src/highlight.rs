//! Paint-only syntax highlighting for fenced code blocks.
//!
//! A single-pass lexer over one line at a time, with a small carry for
//! constructs that span lines (block comments, multi-line strings). It emits
//! only non-plain spans; gaps are the default foreground.
//!
//! The results become `TextRun` *colors* on the same monospace font, so a line
//! measures identically whether or not it has been highlighted — "highlight is
//! pure paint". That is what lets the renderer skip or defer highlighting on a
//! streaming block without ever reflowing it, and it is why a lexer beats a
//! grammar here: streamed code is syntactically incomplete for most of its
//! life, and a lexer degrades into "the tail is plain" instead of re-anchoring
//! an error-recovering parse and flickering colors across the whole block.
//!
//! This module is pure and synchronous; callers cache per line.

use std::ops::Range;

/// Paint class for a token. `Plain` is implicit — unmatched spans keep the
/// block's foreground color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenClass {
    Keyword,
    /// Language-level constant: `true`, `nil`, `None`, numbers' cousins.
    Literal,
    String,
    Comment,
    Number,
    /// A type-shaped identifier (leading uppercase) or a declared type name.
    Type,
    /// An identifier immediately followed by `(`.
    Function,
    /// `@decorator`, `#[attribute]`, preprocessor lines, `$variable`.
    Meta,
    /// Diff insertions and deletions.
    Added,
    Removed,
}

/// One highlighted span, as byte offsets **within its line**.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub range: Range<usize>,
    pub class: TokenClass,
}

/// Lexer state carried across a line boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Carry {
    #[default]
    None,
    /// Inside a block comment.
    BlockComment,
    /// Inside a multi-line string; the payload indexes the language's strings.
    String(u8),
}

/// Languages with a dedicated spec. Anything else renders unhighlighted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Lang {
    Rust,
    /// JavaScript, TypeScript, JSX/TSX — one spec, superset keywords.
    Script,
    Python,
    Go,
    C,
    Java,
    Ruby,
    Swift,
    Json,
    Yaml,
    Toml,
    Shell,
    Css,
    Html,
    Sql,
    Diff,
}

/// Resolve a fenced-code info string to a language spec.
pub fn lang_for_tag(tag: &str) -> Option<Lang> {
    let tag = tag.trim().to_ascii_lowercase();
    Some(match tag.as_str() {
        "rust" | "rs" => Lang::Rust,
        "js" | "jsx" | "mjs" | "cjs" | "javascript" | "node" | "ts" | "tsx" | "mts" | "cts"
        | "typescript" => Lang::Script,
        "py" | "python" | "python3" => Lang::Python,
        "go" | "golang" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "c++" | "cxx" | "hpp" | "objc" | "m" => Lang::C,
        "java" | "kt" | "kotlin" | "scala" | "cs" | "csharp" | "c#" => Lang::Java,
        "rb" | "ruby" | "gemfile" | "rake" => Lang::Ruby,
        "swift" => Lang::Swift,
        "json" | "jsonc" | "json5" => Lang::Json,
        "yaml" | "yml" => Lang::Yaml,
        "toml" | "ini" | "cfg" => Lang::Toml,
        "sh" | "bash" | "zsh" | "shell" | "shellscript" | "console" | "fish" | "dockerfile"
        | "docker" | "makefile" | "make" => Lang::Shell,
        "css" | "scss" | "sass" | "less" => Lang::Css,
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" => Lang::Html,
        "sql" | "postgres" | "postgresql" | "mysql" | "sqlite" => Lang::Sql,
        "diff" | "patch" => Lang::Diff,
        _ => return None,
    })
}

// ── Language specs ─────────────────────────────────────────────────────────

struct StringSpec {
    delimiter: &'static str,
    multiline: bool,
    escapes: bool,
}

const SINGLE: StringSpec = StringSpec {
    delimiter: "'",
    multiline: false,
    escapes: true,
};
const DOUBLE: StringSpec = StringSpec {
    delimiter: "\"",
    multiline: false,
    escapes: true,
};
const BACKTICK: StringSpec = StringSpec {
    delimiter: "`",
    multiline: true,
    escapes: true,
};
const TRIPLE_DOUBLE: StringSpec = StringSpec {
    delimiter: "\"\"\"",
    multiline: true,
    escapes: true,
};
const TRIPLE_SINGLE: StringSpec = StringSpec {
    delimiter: "'''",
    multiline: true,
    escapes: true,
};

struct LangSpec {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Ordered: longer delimiters must precede their prefixes.
    strings: &'static [StringSpec],
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    /// Lowercase primitive type names, which `capitalized_types` cannot catch.
    types: &'static [&'static str],
    /// Characters that may appear inside an identifier beyond alphanumerics.
    extra_identifier: &'static [char],
    /// `@name` / `$name` sigils that mark a whole token as meta.
    meta_sigils: &'static [char],
    /// Highlight `Capitalized` identifiers as types.
    capitalized_types: bool,
    /// Highlight `name(` as a function.
    call_functions: bool,
}

const DEFAULT_SPEC: LangSpec = LangSpec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &[DOUBLE, SINGLE],
    keywords: &[],
    literals: &["true", "false", "null"],
    types: &[],
    extra_identifier: &['_'],
    meta_sigils: &[],
    capitalized_types: true,
    call_functions: true,
};

fn spec(lang: Lang) -> LangSpec {
    match lang {
        Lang::Rust => LangSpec {
            keywords: &[
                "as",
                "async",
                "await",
                "break",
                "const",
                "continue",
                "crate",
                "dyn",
                "else",
                "enum",
                "extern",
                "fn",
                "for",
                "if",
                "impl",
                "in",
                "let",
                "loop",
                "match",
                "mod",
                "move",
                "mut",
                "pub",
                "ref",
                "return",
                "self",
                "Self",
                "static",
                "struct",
                "super",
                "trait",
                "type",
                "unsafe",
                "use",
                "where",
                "while",
                "yield",
                "macro_rules",
            ],
            literals: &["true", "false", "None", "Some", "Ok", "Err"],
            types: &[
                "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str",
                "u8", "u16", "u32", "u64", "u128", "usize",
            ],
            meta_sigils: &['\''],
            ..DEFAULT_SPEC
        },
        Lang::Script => LangSpec {
            strings: &[DOUBLE, SINGLE, BACKTICK],
            keywords: &[
                "as",
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "declare",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "finally",
                "for",
                "from",
                "function",
                "get",
                "if",
                "implements",
                "import",
                "in",
                "infer",
                "instanceof",
                "interface",
                "keyof",
                "let",
                "namespace",
                "new",
                "of",
                "private",
                "protected",
                "public",
                "readonly",
                "return",
                "satisfies",
                "set",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "try",
                "type",
                "typeof",
                "var",
                "void",
                "while",
                "yield",
            ],
            literals: &["true", "false", "null", "undefined", "NaN", "Infinity"],
            extra_identifier: &['_', '$'],
            ..DEFAULT_SPEC
        },
        Lang::Python => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            strings: &[TRIPLE_DOUBLE, TRIPLE_SINGLE, DOUBLE, SINGLE],
            keywords: &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise",
                "return", "try", "while", "with", "yield",
            ],
            literals: &["True", "False", "None", "self", "cls"],
            meta_sigils: &['@'],
            ..DEFAULT_SPEC
        },
        Lang::Go => LangSpec {
            strings: &[DOUBLE, BACKTICK, SINGLE],
            keywords: &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "type",
                "var",
            ],
            literals: &[
                "true", "false", "nil", "iota", "bool", "byte", "error", "float32", "float64",
                "int", "int8", "int16", "int32", "int64", "rune", "string", "uint", "uint8",
                "uint16", "uint32", "uint64",
            ],
            ..DEFAULT_SPEC
        },
        Lang::C => LangSpec {
            keywords: &[
                "auto",
                "break",
                "case",
                "class",
                "const",
                "constexpr",
                "continue",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "explicit",
                "extern",
                "for",
                "friend",
                "goto",
                "if",
                "inline",
                "namespace",
                "new",
                "operator",
                "override",
                "private",
                "protected",
                "public",
                "register",
                "return",
                "sizeof",
                "static",
                "struct",
                "switch",
                "template",
                "this",
                "throw",
                "try",
                "typedef",
                "typename",
                "union",
                "using",
                "virtual",
                "volatile",
                "while",
            ],
            literals: &[
                "true", "false", "NULL", "nullptr", "bool", "char", "double", "float", "int",
                "long", "short", "signed", "unsigned", "void", "size_t", "uint8_t", "uint32_t",
                "uint64_t",
            ],
            meta_sigils: &['#'],
            ..DEFAULT_SPEC
        },
        Lang::Java => LangSpec {
            keywords: &[
                "abstract",
                "as",
                "assert",
                "break",
                "case",
                "catch",
                "class",
                "companion",
                "const",
                "continue",
                "data",
                "def",
                "default",
                "do",
                "else",
                "enum",
                "extends",
                "final",
                "finally",
                "for",
                "fun",
                "if",
                "implements",
                "import",
                "instanceof",
                "interface",
                "internal",
                "is",
                "lateinit",
                "native",
                "new",
                "object",
                "open",
                "operator",
                "override",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "sealed",
                "static",
                "super",
                "suspend",
                "switch",
                "synchronized",
                "this",
                "throw",
                "throws",
                "trait",
                "transient",
                "try",
                "val",
                "var",
                "volatile",
                "when",
                "while",
                "yield",
            ],
            literals: &[
                "true", "false", "null", "boolean", "byte", "char", "double", "float", "int",
                "long", "short", "void", "String", "Unit",
            ],
            meta_sigils: &['@'],
            ..DEFAULT_SPEC
        },
        Lang::Ruby => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            keywords: &[
                "alias",
                "and",
                "begin",
                "break",
                "case",
                "class",
                "def",
                "defined?",
                "do",
                "else",
                "elsif",
                "end",
                "ensure",
                "for",
                "if",
                "in",
                "module",
                "next",
                "not",
                "or",
                "redo",
                "rescue",
                "retry",
                "return",
                "then",
                "undef",
                "unless",
                "until",
                "when",
                "while",
                "yield",
                "require",
                "require_relative",
                "attr_accessor",
                "attr_reader",
                "attr_writer",
            ],
            literals: &["true", "false", "nil", "self", "__FILE__", "__dir__"],
            extra_identifier: &['_', '?', '!'],
            meta_sigils: &['@', '$', ':'],
            ..DEFAULT_SPEC
        },
        Lang::Swift => LangSpec {
            strings: &[TRIPLE_DOUBLE, DOUBLE],
            keywords: &[
                "actor",
                "as",
                "associatedtype",
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "continue",
                "default",
                "defer",
                "deinit",
                "do",
                "else",
                "enum",
                "extension",
                "fallthrough",
                "fileprivate",
                "final",
                "for",
                "func",
                "guard",
                "if",
                "import",
                "in",
                "indirect",
                "init",
                "inout",
                "internal",
                "is",
                "lazy",
                "let",
                "mutating",
                "nonisolated",
                "open",
                "operator",
                "private",
                "protocol",
                "public",
                "repeat",
                "rethrows",
                "return",
                "self",
                "static",
                "struct",
                "subscript",
                "super",
                "switch",
                "throw",
                "throws",
                "try",
                "typealias",
                "var",
                "where",
                "while",
            ],
            literals: &[
                "true", "false", "nil", "Any", "Bool", "Double", "Int", "String", "Void", "some",
            ],
            meta_sigils: &['@', '#'],
            ..DEFAULT_SPEC
        },
        Lang::Json => LangSpec {
            line_comments: &["//"],
            strings: &[DOUBLE],
            keywords: &[],
            literals: &["true", "false", "null"],
            capitalized_types: false,
            call_functions: false,
            ..DEFAULT_SPEC
        },
        Lang::Yaml => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            strings: &[DOUBLE, SINGLE],
            keywords: &[],
            literals: &["true", "false", "null", "yes", "no", "on", "off", "~"],
            capitalized_types: false,
            call_functions: false,
            ..DEFAULT_SPEC
        },
        Lang::Toml => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            strings: &[TRIPLE_DOUBLE, DOUBLE, SINGLE],
            keywords: &[],
            literals: &["true", "false"],
            capitalized_types: false,
            call_functions: false,
            ..DEFAULT_SPEC
        },
        Lang::Shell => LangSpec {
            line_comments: &["#"],
            block_comment: None,
            strings: &[DOUBLE, SINGLE],
            keywords: &[
                "case",
                "do",
                "done",
                "elif",
                "else",
                "esac",
                "export",
                "fi",
                "for",
                "function",
                "if",
                "in",
                "local",
                "return",
                "select",
                "then",
                "until",
                "while",
                "ARG",
                "COPY",
                "CMD",
                "ENV",
                "EXPOSE",
                "FROM",
                "RUN",
                "WORKDIR",
                "ENTRYPOINT",
            ],
            literals: &["true", "false"],
            extra_identifier: &['_', '-'],
            meta_sigils: &['$'],
            capitalized_types: false,
            ..DEFAULT_SPEC
        },
        Lang::Css => LangSpec {
            line_comments: &[],
            strings: &[DOUBLE, SINGLE],
            keywords: &[
                "important",
                "media",
                "keyframes",
                "import",
                "supports",
                "font-face",
                "from",
                "to",
                "and",
                "not",
                "only",
            ],
            literals: &["auto", "none", "inherit", "initial", "unset", "transparent"],
            extra_identifier: &['_', '-'],
            meta_sigils: &['@', '$', '#', '.', '&'],
            capitalized_types: false,
            ..DEFAULT_SPEC
        },
        Lang::Html => LangSpec {
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            strings: &[DOUBLE, SINGLE],
            keywords: &[],
            literals: &[],
            extra_identifier: &['_', '-', ':'],
            capitalized_types: false,
            call_functions: false,
            ..DEFAULT_SPEC
        },
        Lang::Sql => LangSpec {
            line_comments: &["--", "#"],
            strings: &[SINGLE, DOUBLE],
            keywords: &[
                "add",
                "all",
                "alter",
                "and",
                "as",
                "asc",
                "begin",
                "between",
                "by",
                "case",
                "cast",
                "column",
                "commit",
                "constraint",
                "create",
                "cross",
                "default",
                "delete",
                "desc",
                "distinct",
                "drop",
                "else",
                "end",
                "exists",
                "foreign",
                "from",
                "full",
                "group",
                "having",
                "if",
                "in",
                "index",
                "inner",
                "insert",
                "into",
                "is",
                "join",
                "key",
                "left",
                "like",
                "limit",
                "not",
                "offset",
                "on",
                "or",
                "order",
                "outer",
                "primary",
                "references",
                "returning",
                "right",
                "rollback",
                "select",
                "set",
                "table",
                "then",
                "transaction",
                "union",
                "unique",
                "update",
                "values",
                "view",
                "when",
                "where",
                "with",
            ],
            literals: &["null", "true", "false"],
            capitalized_types: false,
            ..DEFAULT_SPEC
        },
        Lang::Diff => LangSpec {
            line_comments: &[],
            block_comment: None,
            strings: &[],
            keywords: &[],
            literals: &[],
            capitalized_types: false,
            call_functions: false,
            ..DEFAULT_SPEC
        },
    }
}

// ── Lexer ──────────────────────────────────────────────────────────────────

/// Tokenize one line. Returns its spans plus the carry for the next line.
pub fn tokenize_line(lang: Lang, line: &str, carry: Carry) -> (Vec<Token>, Carry) {
    let spec = spec(lang);
    if lang == Lang::Diff {
        return (diff_line(line), Carry::None);
    }

    let bytes = line.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut index = 0usize;
    let mut carry = carry;

    // Resume a construct that opened on an earlier line.
    match carry {
        Carry::BlockComment => {
            let Some((_, close)) = spec.block_comment else {
                return (tokens, Carry::None);
            };
            match line.find(close) {
                Some(end) => {
                    let end = end + close.len();
                    push(&mut tokens, 0..end, TokenClass::Comment);
                    index = end;
                    carry = Carry::None;
                }
                None => {
                    push(&mut tokens, 0..line.len(), TokenClass::Comment);
                    return (tokens, Carry::BlockComment);
                }
            }
        }
        Carry::String(slot) => {
            let Some(string) = spec.strings.get(slot as usize) else {
                carry = Carry::None;
                return (tokens, carry);
            };
            match find_close(line, 0, string) {
                Some(end) => {
                    push(&mut tokens, 0..end, TokenClass::String);
                    index = end;
                    carry = Carry::None;
                }
                None => {
                    push(&mut tokens, 0..line.len(), TokenClass::String);
                    return (tokens, Carry::String(slot));
                }
            }
        }
        Carry::None => {}
    }

    // A `#`-prefixed line in C-family languages is a preprocessor directive,
    // and in shells the same character opens a comment — the spec decides.
    if index == 0
        && spec.meta_sigils.contains(&'#')
        && line.trim_start().starts_with('#')
        && !spec.line_comments.contains(&"#")
    {
        push(&mut tokens, 0..line.len(), TokenClass::Meta);
        return (tokens, Carry::None);
    }

    while index < bytes.len() {
        let rest = &line[index..];

        if spec
            .line_comments
            .iter()
            .any(|marker| rest.starts_with(*marker))
        {
            push(&mut tokens, index..line.len(), TokenClass::Comment);
            break;
        }

        if let Some((open, close)) = spec.block_comment
            && rest.starts_with(open)
        {
            match line[index + open.len()..].find(close) {
                Some(offset) => {
                    let end = index + open.len() + offset + close.len();
                    push(&mut tokens, index..end, TokenClass::Comment);
                    index = end;
                }
                None => {
                    push(&mut tokens, index..line.len(), TokenClass::Comment);
                    return (tokens, Carry::BlockComment);
                }
            }
            continue;
        }

        if let Some((slot, string)) = spec
            .strings
            .iter()
            .enumerate()
            .find(|(_, string)| rest.starts_with(string.delimiter))
        {
            let content_start = index + string.delimiter.len();
            match find_close(line, content_start, string) {
                Some(end) => {
                    push(&mut tokens, index..end, TokenClass::String);
                    index = end;
                }
                None if string.multiline => {
                    push(&mut tokens, index..line.len(), TokenClass::String);
                    return (tokens, Carry::String(slot as u8));
                }
                None => {
                    // An unterminated single-line string still colors to the
                    // end of the line — the common case while streaming.
                    push(&mut tokens, index..line.len(), TokenClass::String);
                    break;
                }
            }
            continue;
        }

        let ch = rest.chars().next().expect("rest is non-empty");

        if ch.is_ascii_digit() {
            let end = index + number_length(rest);
            push(&mut tokens, index..end, TokenClass::Number);
            index = end;
            continue;
        }

        if spec.meta_sigils.contains(&ch) {
            let length = ch.len_utf8() + identifier_length(&rest[ch.len_utf8()..], &spec);
            if length > ch.len_utf8() {
                push(&mut tokens, index..index + length, TokenClass::Meta);
                index += length;
                continue;
            }
        }

        if is_identifier_start(ch, &spec) {
            let length = identifier_length(rest, &spec);
            let word = &rest[..length];
            let end = index + length;
            let class = if spec.keywords.contains(&word) {
                Some(TokenClass::Keyword)
            } else if spec.literals.contains(&word) {
                Some(TokenClass::Literal)
            } else if spec.types.contains(&word) {
                Some(TokenClass::Type)
            } else if spec.call_functions && next_non_space(&rest[length..]) == Some('(') {
                Some(TokenClass::Function)
            } else if spec.capitalized_types && word.starts_with(char::is_uppercase) {
                Some(TokenClass::Type)
            } else {
                None
            };
            if let Some(class) = class {
                push(&mut tokens, index..end, class);
            }
            index = end;
            continue;
        }

        index += ch.len_utf8();
    }

    (tokens, carry)
}

/// Tokenize a whole block, one entry per line, threading the carry.
pub fn tokenize(lang: Lang, code: &str) -> Vec<Vec<Token>> {
    let mut carry = Carry::None;
    code.split('\n')
        .map(|line| {
            let (tokens, next) = tokenize_line(lang, line, carry);
            carry = next;
            tokens
        })
        .collect()
}

fn push(tokens: &mut Vec<Token>, range: Range<usize>, class: TokenClass) {
    if range.start >= range.end {
        return;
    }
    // Merge with the previous span when it is adjacent and identical, so paint
    // sees the fewest possible runs.
    match tokens.last_mut() {
        Some(last) if last.class == class && last.range.end == range.start => {
            last.range.end = range.end;
        }
        _ => tokens.push(Token { range, class }),
    }
}

/// Byte offset just past a string's closing delimiter, searching from
/// `content_start`. `None` when the line ends first.
fn find_close(line: &str, content_start: usize, string: &StringSpec) -> Option<usize> {
    let bytes = line.as_bytes();
    let delimiter = string.delimiter.as_bytes();
    let mut index = content_start;
    while index < bytes.len() {
        if string.escapes && bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index..].starts_with(delimiter) {
            return Some(index + delimiter.len());
        }
        index += 1;
    }
    None
}

/// Length of a numeric literal at the start of `rest`, covering hex/binary/
/// octal prefixes, digit separators, exponents, and type suffixes.
fn number_length(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 1;
    if bytes.len() > 1
        && bytes[0] == b'0'
        && matches!(bytes[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    {
        index = 2;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        return index;
    }
    let mut seen_dot = false;
    while index < bytes.len() {
        match bytes[index] {
            b'0'..=b'9' | b'_' => index += 1,
            b'.' if !seen_dot && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) => {
                seen_dot = true;
                index += 1;
            }
            b'e' | b'E'
                if bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit() || matches!(next, b'+' | b'-')) =>
            {
                index += 2;
            }
            // Type suffixes: `1u32`, `2.0f64`, `3L`, `4n`.
            b'a'..=b'z' | b'A'..=b'Z' => index += 1,
            _ => break,
        }
    }
    index
}

fn is_identifier_start(ch: char, spec: &LangSpec) -> bool {
    ch.is_alphabetic() || spec.extra_identifier.contains(&ch)
}

fn identifier_length(rest: &str, spec: &LangSpec) -> usize {
    let mut length = 0;
    for (offset, ch) in rest.char_indices() {
        let continues = if offset == 0 {
            is_identifier_start(ch, spec)
        } else {
            ch.is_alphanumeric() || spec.extra_identifier.contains(&ch)
        };
        if !continues {
            break;
        }
        length = offset + ch.len_utf8();
    }
    length
}

fn next_non_space(rest: &str) -> Option<char> {
    rest.chars().find(|ch| !ch.is_whitespace())
}

/// Diff hunks are line-classified, not lexed.
fn diff_line(line: &str) -> Vec<Token> {
    let class = match line.as_bytes().first() {
        Some(b'+') if !line.starts_with("+++") => TokenClass::Added,
        Some(b'-') if !line.starts_with("---") => TokenClass::Removed,
        Some(b'@') => TokenClass::Meta,
        Some(b'+') | Some(b'-') | Some(b'd') | Some(b'i') => TokenClass::Comment,
        _ => return Vec::new(),
    };
    vec![Token {
        range: 0..line.len(),
        class,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(lang: Lang, line: &str) -> Vec<(&str, TokenClass)> {
        tokenize_line(lang, line, Carry::None)
            .0
            .into_iter()
            .map(|token| (&line[token.range], token.class))
            .collect()
    }

    #[test]
    fn tags_resolve_to_specs() {
        assert_eq!(lang_for_tag("rs"), Some(Lang::Rust));
        assert_eq!(lang_for_tag("TypeScript"), Some(Lang::Script));
        assert_eq!(lang_for_tag(" tsx "), Some(Lang::Script));
        assert_eq!(lang_for_tag("brainfuck"), None);
    }

    #[test]
    fn rust_line_classifies_keywords_types_calls_and_numbers() {
        assert_eq!(
            spans(Lang::Rust, "let mut count = compute(42) as u32; // note"),
            vec![
                ("let", TokenClass::Keyword),
                ("mut", TokenClass::Keyword),
                ("compute", TokenClass::Function),
                ("42", TokenClass::Number),
                ("as", TokenClass::Keyword),
                ("u32", TokenClass::Type),
                ("// note", TokenClass::Comment),
            ]
        );
    }

    #[test]
    fn strings_consume_escapes_and_stop_at_the_closer() {
        assert_eq!(
            spans(Lang::Rust, r#"let s = "a \" b"; let t = 1;"#),
            vec![
                ("let", TokenClass::Keyword),
                (r#""a \" b""#, TokenClass::String),
                ("let", TokenClass::Keyword),
                ("1", TokenClass::Number),
            ]
        );
    }

    #[test]
    fn block_comments_carry_across_lines() {
        let (tokens, carry) = tokenize_line(Lang::Rust, "code /* open", Carry::None);
        assert_eq!(carry, Carry::BlockComment);
        assert_eq!(tokens.last().unwrap().class, TokenClass::Comment);

        let (tokens, carry) = tokenize_line(Lang::Rust, "still comment", carry);
        assert_eq!(carry, Carry::BlockComment);
        assert_eq!(tokens.len(), 1);

        let (tokens, carry) = tokenize_line(Lang::Rust, "done */ let x = 1;", carry);
        assert_eq!(carry, Carry::None);
        assert_eq!(tokens[0].class, TokenClass::Comment);
        assert_eq!(&"done */ let x = 1;"[tokens[0].range.clone()], "done */");
        assert!(
            tokens
                .iter()
                .any(|token| token.class == TokenClass::Keyword)
        );
    }

    #[test]
    fn python_triple_quoted_strings_carry_across_lines() {
        let lines = tokenize(Lang::Python, "x = \"\"\"one\ntwo\"\"\"\ny = 2");
        assert_eq!(lines[0].last().unwrap().class, TokenClass::String);
        assert_eq!(lines[1][0].class, TokenClass::String);
        assert!(lines[2].iter().any(|t| t.class == TokenClass::Number));
    }

    #[test]
    fn python_decorators_and_literals() {
        assert_eq!(
            spans(Lang::Python, "@cache"),
            vec![("@cache", TokenClass::Meta)]
        );
        assert_eq!(
            spans(Lang::Python, "def f(self): return None  # ok"),
            vec![
                ("def", TokenClass::Keyword),
                ("f", TokenClass::Function),
                ("self", TokenClass::Literal),
                ("return", TokenClass::Keyword),
                ("None", TokenClass::Literal),
                ("# ok", TokenClass::Comment),
            ]
        );
    }

    #[test]
    fn shell_treats_hash_as_a_comment_and_dollar_as_meta() {
        assert_eq!(
            spans(Lang::Shell, "echo $HOME # trailing"),
            vec![
                ("$HOME", TokenClass::Meta),
                ("# trailing", TokenClass::Comment),
            ]
        );
    }

    #[test]
    fn c_preprocessor_lines_are_meta_not_comments() {
        assert_eq!(
            spans(Lang::C, "#include <stdio.h>"),
            vec![("#include <stdio.h>", TokenClass::Meta)]
        );
    }

    #[test]
    fn json_highlights_keys_as_strings_and_literals() {
        assert_eq!(
            spans(Lang::Json, r#"{"on": true, "n": 1.5e3}"#),
            vec![
                (r#""on""#, TokenClass::String),
                ("true", TokenClass::Literal),
                (r#""n""#, TokenClass::String),
                ("1.5e3", TokenClass::Number),
            ]
        );
    }

    #[test]
    fn diff_lines_are_classified_whole() {
        assert_eq!(
            spans(Lang::Diff, "+added line"),
            vec![("+added line", TokenClass::Added)]
        );
        assert_eq!(
            spans(Lang::Diff, "-removed"),
            vec![("-removed", TokenClass::Removed)]
        );
        assert_eq!(
            spans(Lang::Diff, "@@ -1,2 +1,3 @@"),
            vec![("@@ -1,2 +1,3 @@", TokenClass::Meta)]
        );
        assert_eq!(spans(Lang::Diff, " context"), vec![]);
    }

    #[test]
    fn spans_are_ordered_disjoint_and_within_the_line() {
        let sources = [
            (Lang::Rust, "pub fn main() { let x: Vec<u8> = vec![1, 2]; }"),
            (Lang::Script, "const f = async (a) => `t ${a}` // x"),
            (Lang::Python, "class A:\n    '''doc'''\n    x = 0b1010"),
            (Lang::Go, "func main() { s := `raw` }"),
            (Lang::Sql, "SELECT * FROM t WHERE a = 'b' -- note"),
            (Lang::Css, "@media (min-width: 10px) { .a { color: red } }"),
            (Lang::Html, "<a href=\"x\">t</a><!-- c -->"),
            (Lang::Yaml, "key: value # c"),
            (Lang::Toml, "[table]\nkey = \"v\""),
            (Lang::Ruby, "def x?(a) @b = :sym end"),
            (Lang::Swift, "let x: Int = 1 // c"),
            (Lang::Java, "@Override public void f() {}"),
            (Lang::C, "int main(void) { return 0; }"),
        ];
        for (lang, source) in sources {
            for line in source.split('\n') {
                let (tokens, _) = tokenize_line(lang, line, Carry::None);
                let mut previous_end = 0;
                for token in &tokens {
                    assert!(
                        token.range.start >= previous_end,
                        "{lang:?} produced overlapping spans on {line:?}: {tokens:?}"
                    );
                    assert!(
                        token.range.end <= line.len(),
                        "{lang:?} ran past the line on {line:?}: {tokens:?}"
                    );
                    assert!(
                        line.is_char_boundary(token.range.start)
                            && line.is_char_boundary(token.range.end),
                        "{lang:?} split a character on {line:?}: {tokens:?}"
                    );
                    previous_end = token.range.end;
                }
            }
        }
    }

    /// Streaming feeds every prefix of a code block through the lexer, so no
    /// prefix may panic or produce an out-of-range span.
    #[test]
    fn every_prefix_of_a_code_block_tokenizes() {
        let code =
            "fn main() {\n    let s = \"héllo 🎉\";\n    /* note */\n    println!(\"{s}\");\n}";
        for end in 0..=code.len() {
            if !code.is_char_boundary(end) {
                continue;
            }
            let prefix = &code[..end];
            let lines = tokenize(Lang::Rust, prefix);
            for (line, tokens) in prefix.split('\n').zip(&lines) {
                for token in tokens {
                    assert!(token.range.end <= line.len());
                    assert!(line.is_char_boundary(token.range.start));
                    assert!(line.is_char_boundary(token.range.end));
                }
            }
        }
    }
}
