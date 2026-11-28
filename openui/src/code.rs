//! Colours for the code an artifact shows: a small tokenizer that tells comments, strings,
//! numbers, keywords, types and calls apart in the languages a repository is likely to
//! hold. It reads no grammar, so it's cheap enough to run on every redraw while a document
//! streams in and small enough for the viewer's module; what it can't tell stays plain.
//! The page gives each kind the colour the chat's code blocks use for it.

/// What a run of code is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Plain,
    Comment,
    Str,
    Num,
    Keyword,
    /// `true`, `None`, `MAX_INLINE`, `$HOME`.
    Const,
    Type,
    /// A name that's called.
    Func,
    /// `#[derive(..)]`, `@decorator`, a key in JSON, YAML or TOML, an HTML attribute.
    Attr,
    /// An HTML or XML tag's name.
    Tag,
    /// A line a diff adds, removes, or heads a hunk with.
    Add,
    Del,
    Meta,
}

impl Kind {
    /// The class the page colours it by (`t-<class>`, as tamale's code blocks have them);
    /// empty for plain text.
    pub fn class(self) -> &'static str {
        match self {
            Kind::Plain => "",
            Kind::Comment => "c",
            Kind::Str => "s",
            Kind::Num => "n",
            Kind::Keyword => "kw",
            Kind::Const => "k",
            Kind::Type => "ty",
            Kind::Func => "fn",
            Kind::Attr => "attr",
            Kind::Tag => "tag",
            Kind::Add => "add",
            Kind::Del => "del",
            Kind::Meta => "hd",
        }
    }
}

/// A run of one kind.
pub type Run = (Kind, String);

#[derive(Clone, Copy, PartialEq)]
enum Family {
    /// Words, strings, numbers and comments: most languages.
    Code,
    /// Tags and attributes.
    Markup,
    /// `key: value` or `key = value` lines, and `[sections]`.
    Keyed,
    Json,
    Css,
    Diff,
    Plain,
}

struct Lang {
    family: Family,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// The characters that quote a string.
    quotes: &'static [char],
    /// Whether `"""` and `'''` quote one that runs over lines.
    triple: bool,
    keywords: &'static [&'static str],
    consts: &'static [&'static str],
    types: &'static [&'static str],
    /// SQL: `select` is `SELECT`.
    any_case: bool,
    /// `'a` is a lifetime unless it closes as a character.
    lifetimes: bool,
    /// `#[..]` is an attribute (Rust) or `#include` a directive (C).
    hash_attr: bool,
    /// `@name` is a decorator or an annotation.
    at_attr: bool,
    /// `$NAME` is a variable.
    dollar: bool,
}

const BASE: Lang = Lang {
    family: Family::Code,
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[],
    consts: &["true", "false", "null"],
    types: &[],
    any_case: false,
    lifetimes: false,
    hash_attr: false,
    at_attr: false,
    dollar: false,
};

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "static", "struct", "super", "trait", "type", "unsafe", "use",
        "where", "while",
    ],
    consts: &["true", "false", "self", "Self", "None", "Some", "Ok", "Err"],
    types: &[
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64", "bool", "char", "str",
    ],
    quotes: &['"'],
    lifetimes: true,
    hash_attr: true,
    ..BASE
};

const JS: Lang = Lang {
    keywords: &[
        "abstract",
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
        "instanceof",
        "interface",
        "keyof",
        "let",
        "namespace",
        "new",
        "of",
        "override",
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
        "with",
        "yield",
    ],
    consts: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    types: &[
        "string", "number", "boolean", "any", "unknown", "never", "object", "symbol", "bigint",
    ],
    quotes: &['"', '\'', '`'],
    at_attr: true,
    ..BASE
};

const PYTHON: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    triple: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
        "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    consts: &["True", "False", "None", "self", "cls"],
    types: &[
        "int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "object",
    ],
    at_attr: true,
    ..BASE
};

const GO: Lang = Lang {
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
    consts: &["true", "false", "nil", "iota"],
    types: &[
        "string", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32",
        "uint64", "float32", "float64", "bool", "byte", "rune", "error", "any",
    ],
    quotes: &['"', '\'', '`'],
    ..BASE
};

const C: Lang = Lang {
    keywords: &[
        "auto",
        "break",
        "case",
        "catch",
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
        "noexcept",
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
    consts: &["true", "false", "NULL", "nullptr"],
    types: &[
        "int", "char", "float", "double", "void", "long", "short", "unsigned", "signed", "bool",
        "size_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t", "int16_t", "int32_t",
        "int64_t",
    ],
    hash_attr: true,
    ..BASE
};

/// Java, Kotlin, Swift, C#, Scala, Dart, PHP, Zig: near enough to share their words.
const CURLY: Lang = Lang {
    keywords: &[
        "abstract",
        "actor",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "defer",
        "deinit",
        "do",
        "else",
        "enum",
        "extends",
        "extension",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "fun",
        "func",
        "function",
        "get",
        "guard",
        "if",
        "implements",
        "import",
        "in",
        "init",
        "inout",
        "instanceof",
        "interface",
        "internal",
        "is",
        "lazy",
        "let",
        "mutating",
        "namespace",
        "new",
        "object",
        "open",
        "operator",
        "override",
        "package",
        "private",
        "protected",
        "protocol",
        "pub",
        "public",
        "readonly",
        "return",
        "sealed",
        "set",
        "static",
        "struct",
        "super",
        "switch",
        "this",
        "throw",
        "throws",
        "try",
        "typealias",
        "use",
        "using",
        "val",
        "var",
        "virtual",
        "void",
        "weak",
        "when",
        "where",
        "while",
        "yield",
    ],
    consts: &["true", "false", "null", "nil", "self"],
    types: &[
        "int", "long", "float", "double", "boolean", "bool", "string", "char", "byte", "short",
    ],
    at_attr: true,
    dollar: true,
    ..BASE
};

const RUBY: Lang = Lang {
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
        "defined",
        "defmodule",
        "defp",
        "do",
        "else",
        "elsif",
        "end",
        "ensure",
        "fn",
        "for",
        "if",
        "in",
        "module",
        "next",
        "not",
        "or",
        "redo",
        "require",
        "rescue",
        "retry",
        "return",
        "super",
        "then",
        "undef",
        "unless",
        "until",
        "use",
        "when",
        "while",
        "yield",
    ],
    consts: &["true", "false", "nil", "self"],
    ..BASE
};

const SHELL: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
        "in", "function", "select", "return", "exit", "export", "local", "readonly", "declare",
        "set", "unset", "source", "alias",
    ],
    consts: &[],
    dollar: true,
    ..BASE
};

const SQL: Lang = Lang {
    line_comments: &["--"],
    quotes: &['\''],
    keywords: &[
        "select",
        "from",
        "where",
        "and",
        "or",
        "not",
        "insert",
        "into",
        "values",
        "update",
        "set",
        "delete",
        "create",
        "table",
        "alter",
        "drop",
        "index",
        "view",
        "join",
        "left",
        "right",
        "inner",
        "outer",
        "full",
        "on",
        "as",
        "group",
        "by",
        "order",
        "having",
        "limit",
        "offset",
        "distinct",
        "union",
        "all",
        "case",
        "when",
        "then",
        "else",
        "end",
        "is",
        "in",
        "like",
        "between",
        "exists",
        "primary",
        "key",
        "foreign",
        "references",
        "default",
        "constraint",
        "begin",
        "commit",
        "rollback",
        "with",
        "returning",
        "asc",
        "desc",
        "unique",
        "if",
    ],
    consts: &["null", "true", "false"],
    types: &[
        "int",
        "integer",
        "text",
        "varchar",
        "boolean",
        "timestamp",
        "date",
        "numeric",
        "serial",
        "bigint",
        "real",
        "blob",
    ],
    any_case: true,
    ..BASE
};

const LUA: Lang = Lang {
    line_comments: &["--"],
    block_comment: Some(("--[[", "]]")),
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    consts: &["true", "false", "nil"],
    ..BASE
};

const KEYED: Lang = Lang {
    family: Family::Keyed,
    line_comments: &["#"],
    block_comment: None,
    consts: &["true", "false", "null", "yes", "no", "on", "off"],
    ..BASE
};

const fn family(family: Family) -> Lang {
    Lang { family, ..BASE }
}

/// The language a fence or a file's name asks for.
fn lang(name: &str) -> Lang {
    let name = name
        .split([',', ' ', '{'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match name.as_str() {
        "rust" | "rs" => RUST,
        "js" | "jsx" | "mjs" | "cjs" | "javascript" | "ts" | "tsx" | "typescript" => JS,
        "py" | "python" | "pyi" => PYTHON,
        "go" | "golang" => GO,
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "c++" | "objc" | "m" | "mm" => C,
        "java" | "kt" | "kts" | "kotlin" | "swift" | "cs" | "csharp" | "c#" | "scala" | "dart"
        | "php" | "zig" | "groovy" | "gradle" => CURLY,
        "rb" | "ruby" | "ex" | "exs" | "elixir" | "cr" => RUBY,
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" | "dockerfile" | "makefile"
        | "make" | "just" | "nix" => SHELL,
        "sql" | "sqlite" | "psql" | "mysql" => SQL,
        "lua" => LUA,
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" | "properties" | "lock" => KEYED,
        "json" | "jsonc" | "json5" | "jsonl" => family(Family::Json),
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" | "plist" | "xhtml" => {
            family(Family::Markup)
        }
        "css" | "scss" | "less" | "sass" => family(Family::Css),
        "diff" | "patch" => family(Family::Diff),
        _ => family(Family::Plain),
    }
}

/// The language a file is in, as [`highlight`] takes it: its extension, or its name when
/// that says more (`Dockerfile`, `Makefile`).
pub fn language_of(file: &str) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    // `src/web.rs:120` names a line too.
    let name = name.split([':', '#']).next().unwrap_or(name);
    match name.to_ascii_lowercase().as_str() {
        "dockerfile" | "makefile" | "justfile" => name.to_ascii_lowercase(),
        lower => lower.rsplit_once('.').map_or("", |(_, e)| e).to_string(),
    }
}

/// `code` in `language` (a fence's name or a file's extension), as its lines' runs. A
/// language this doesn't know comes back plain.
pub fn highlight(language: &str, code: &str) -> Vec<Vec<Run>> {
    let lang = lang(language);
    let chars: Vec<char> = code.chars().collect();
    let runs = match lang.family {
        Family::Plain => vec![(Kind::Plain, code.to_string())],
        Family::Diff => diff(code),
        Family::Markup => markup(&chars),
        Family::Code | Family::Keyed | Family::Json | Family::Css => words(&chars, &lang),
    };
    lines(runs)
}

/// One line of code, on its own: a diff's row, whose neighbours may be missing.
pub fn highlight_line(language: &str, line: &str) -> Vec<Run> {
    highlight(language, line)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Runs, which may hold newlines, as lines of runs.
fn lines(runs: Vec<Run>) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = vec![Vec::new()];
    for (kind, text) in runs {
        for (i, piece) in text.split('\n').enumerate() {
            if i > 0 {
                out.push(Vec::new());
            }
            let piece = piece.strip_suffix('\r').unwrap_or(piece);
            if piece.is_empty() {
                continue;
            }
            let line = out.last_mut().unwrap();
            match line.last_mut() {
                Some((last, run)) if *last == kind => run.push_str(piece),
                _ => line.push((kind, piece.to_string())),
            }
        }
    }
    // The newline a block ends with isn't a line of its own.
    if out.len() > 1 && out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

fn diff(code: &str) -> Vec<Run> {
    let mut out = Vec::new();
    for (i, line) in code.split('\n').enumerate() {
        if i > 0 {
            out.push((Kind::Plain, "\n".to_string()));
        }
        let kind = if line.starts_with("@@") || line.starts_with("diff ") {
            Kind::Meta
        } else if line.starts_with("+++") || line.starts_with("---") {
            Kind::Comment
        } else if line.starts_with('+') {
            Kind::Add
        } else if line.starts_with('-') {
            Kind::Del
        } else {
            Kind::Plain
        };
        out.push((kind, line.to_string()));
    }
    out
}

fn starts(chars: &[char], at: usize, what: &str) -> bool {
    let mut i = at;
    for c in what.chars() {
        if chars.get(i) != Some(&c) {
            return false;
        }
        i += 1;
    }
    true
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The end of the string that opens at `at` with `quote`: past its closing quote, or at
/// the end of the line it's left open on (a backtick's runs on).
fn string_end(chars: &[char], at: usize, quote: char) -> usize {
    let mut i = at + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            c if c == quote => return i + 1,
            '\n' if quote != '`' => return i,
            _ => {}
        }
        i += 1;
    }
    chars.len().min(i)
}

/// Most languages: comments, strings, numbers, and words told apart by the language's
/// lists and by what follows them.
fn words(chars: &[char], lang: &Lang) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut push = |kind: Kind, text: &[char]| {
        if text.is_empty() {
            return;
        }
        let text: String = text.iter().collect();
        match out.last_mut() {
            Some((last, run)) if *last == kind => run.push_str(&text),
            _ => out.push((kind, text)),
        }
    };
    let keyed = lang.family == Family::Keyed;
    let json = lang.family == Family::Json;
    let css = lang.family == Family::Css;
    // How deep in `{}` a stylesheet is: a name before `:` is a property only inside one.
    let mut depth = 0usize;
    // Whether the last word was one that names what follows it (`fn`, `def`).
    let mut defining = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let line_start = chars[..i]
            .iter()
            .rev()
            .take_while(|c| **c != '\n')
            .all(|c| c.is_whitespace() || (keyed && *c == '-'));
        // Comments.
        if let Some((open, close)) = lang.block_comment
            && starts(chars, i, open)
        {
            let mut j = i + open.chars().count();
            while j < chars.len() && !starts(chars, j, close) {
                j += 1;
            }
            let end = (j + close.chars().count()).min(chars.len());
            push(Kind::Comment, &chars[i..end]);
            i = end;
            continue;
        }
        let comment = lang.line_comments.iter().any(|m| starts(chars, i, m))
            // In a shell or a config file `#` only starts a comment after a space.
            && (c != '#' || prev.is_none_or(char::is_whitespace))
            && !css;
        if comment {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |n| i + n);
            push(Kind::Comment, &chars[i..end]);
            i = end;
            continue;
        }
        // Attributes and directives.
        if lang.hash_attr && c == '#' {
            let end = if starts(chars, i, "#[") || starts(chars, i, "#![") {
                let mut open = 0i32;
                let mut j = i;
                loop {
                    match chars.get(j) {
                        Some('[') => open += 1,
                        Some(']') => {
                            open -= 1;
                            if open == 0 {
                                break j + 1;
                            }
                        }
                        Some('\n') | None => break j,
                        _ => {}
                    }
                    j += 1;
                }
            } else if line_start {
                // `#include`, `#define`: the directive's word.
                let mut j = i + 1;
                while j < chars.len() && word_char(chars[j]) {
                    j += 1;
                }
                j
            } else {
                i + 1
            };
            push(Kind::Attr, &chars[i..end]);
            i = end;
            continue;
        }
        if lang.at_attr && c == '@' && chars.get(i + 1).is_some_and(|n| n.is_alphabetic()) {
            let mut j = i + 1;
            while j < chars.len() && (word_char(chars[j]) || chars[j] == '.') {
                j += 1;
            }
            push(Kind::Attr, &chars[i..j]);
            i = j;
            continue;
        }
        if css && c == '@' {
            let mut j = i + 1;
            while j < chars.len() && (word_char(chars[j]) || chars[j] == '-') {
                j += 1;
            }
            push(Kind::Keyword, &chars[i..j]);
            i = j;
            continue;
        }
        if lang.dollar && c == '$' {
            let mut j = i + 1;
            if chars.get(j) == Some(&'{') {
                while j < chars.len() && chars[j] != '}' && chars[j] != '\n' {
                    j += 1;
                }
                j = (j + 1).min(chars.len());
            } else {
                while j < chars.len() && word_char(chars[j]) {
                    j += 1;
                }
            }
            if j > i + 1 {
                push(Kind::Const, &chars[i..j]);
                i = j;
                continue;
            }
        }
        // A `[section]` of a config file.
        if keyed && c == '[' && line_start {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |n| i + n);
            let text: String = chars[i..end].iter().collect();
            if text.trim_end().ends_with(']') && !text.contains(',') {
                push(Kind::Type, &chars[i..end]);
                i = end;
                continue;
            }
        }
        // Strings.
        if lang.triple && (starts(chars, i, "\"\"\"") || starts(chars, i, "'''")) {
            let quote: String = chars[i..i + 3].iter().collect();
            let mut j = i + 3;
            while j < chars.len() && !starts(chars, j, &quote) {
                j += 1;
            }
            let end = (j + 3).min(chars.len());
            push(Kind::Str, &chars[i..end]);
            i = end;
            continue;
        }
        if lang.lifetimes && c == '\'' {
            // 'x' and '\n' are characters; 'a and 'static are lifetimes.
            let closes = match chars.get(i + 1) {
                Some('\\') => chars[i + 2..]
                    .iter()
                    .take(8)
                    .position(|c| *c == '\'')
                    .map(|n| i + 2 + n + 1),
                Some(_) if chars.get(i + 2) == Some(&'\'') => Some(i + 3),
                _ => None,
            };
            match closes {
                Some(end) => {
                    push(Kind::Str, &chars[i..end]);
                    i = end;
                }
                None => {
                    let mut j = i + 1;
                    while j < chars.len() && word_char(chars[j]) {
                        j += 1;
                    }
                    push(Kind::Attr, &chars[i..j]);
                    i = j;
                }
            }
            continue;
        }
        if lang.quotes.contains(&c) {
            let end = string_end(chars, i, c);
            // A key, in JSON: the string a colon follows.
            let key = json
                && chars[end..]
                    .iter()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| *c == ':');
            push(if key { Kind::Attr } else { Kind::Str }, &chars[i..end]);
            i = end;
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() && !prev.is_some_and(word_char) {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric()
                    || chars[j] == '_'
                    || (chars[j] == '.' && chars.get(j + 1).is_some_and(char::is_ascii_digit))
                    || (css && chars[j] == '%'))
            {
                j += 1;
            }
            push(Kind::Num, &chars[i..j]);
            i = j;
            continue;
        }
        // Words.
        if c.is_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < chars.len() && (word_char(chars[j]) || ((css || keyed) && chars[j] == '-')) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let known = |list: &[&str]| {
                if lang.any_case {
                    list.contains(&word.to_ascii_lowercase().as_str())
                } else {
                    list.contains(&word.as_str())
                }
            };
            let after = chars[j..].iter().find(|c| **c != ' ').copied();
            let kind = if keyed {
                // A key, where a line starts; a word that means a value; else text.
                let is_key = line_start
                    && (after == Some(':') || after == Some('='))
                    && !starts(chars, j, "://");
                if is_key {
                    Kind::Attr
                } else if known(lang.consts) {
                    Kind::Const
                } else {
                    Kind::Plain
                }
            } else if json {
                if known(lang.consts) {
                    Kind::Const
                } else {
                    Kind::Plain
                }
            } else if css {
                if depth > 0 && after == Some(':') {
                    Kind::Attr
                } else {
                    Kind::Plain
                }
            } else if prev == Some('.') {
                // A field or a method: `self.type` isn't a keyword because `type` is one.
                if chars.get(j) == Some(&'(') {
                    Kind::Func
                } else {
                    Kind::Plain
                }
            } else if std::mem::take(&mut defining) {
                // The name a `fn` or a `def` gives.
                Kind::Func
            } else if known(lang.keywords) {
                defining = matches!(word.as_str(), "fn" | "def" | "func" | "function" | "fun");
                Kind::Keyword
            } else if known(lang.consts) {
                Kind::Const
            } else if known(lang.types) {
                Kind::Type
            } else if after == Some('(') && chars.get(j) == Some(&'(') {
                Kind::Func
            } else if lang.lifetimes && starts(chars, j, "!") && !starts(chars, j, "!=") {
                // A macro: `format!(..)`.
                j += 1;
                Kind::Func
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                let shouted = word.len() > 1
                    && word
                        .chars()
                        .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
                if shouted { Kind::Const } else { Kind::Type }
            } else {
                Kind::Plain
            };
            push(kind, &chars[i..j]);
            i = j;
            continue;
        }
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        push(Kind::Plain, &chars[i..i + 1]);
        i += 1;
    }
    out
}

/// HTML and XML: comments, tags, their attributes and the strings they're given.
fn markup(chars: &[char]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut push = |kind: Kind, text: &[char]| {
        if text.is_empty() {
            return;
        }
        let text: String = text.iter().collect();
        match out.last_mut() {
            Some((last, run)) if *last == kind => run.push_str(&text),
            _ => out.push((kind, text)),
        }
    };
    let mut i = 0;
    let mut in_tag = false;
    while i < chars.len() {
        let c = chars[i];
        if starts(chars, i, "<!--") {
            let mut j = i + 4;
            while j < chars.len() && !starts(chars, j, "-->") {
                j += 1;
            }
            let end = (j + 3).min(chars.len());
            push(Kind::Comment, &chars[i..end]);
            i = end;
        } else if !in_tag && c == '<' && chars.get(i + 1).is_some_and(|n| !n.is_whitespace()) {
            let mut j = i + 1;
            if matches!(chars.get(j), Some('/' | '!' | '?')) {
                j += 1;
            }
            push(Kind::Plain, &chars[i..j]);
            let start = j;
            while j < chars.len() && (word_char(chars[j]) || matches!(chars[j], '-' | ':' | '.')) {
                j += 1;
            }
            push(Kind::Tag, &chars[start..j]);
            in_tag = true;
            i = j;
        } else if in_tag && c == '>' {
            push(Kind::Plain, &chars[i..i + 1]);
            in_tag = false;
            i += 1;
        } else if in_tag && (c == '"' || c == '\'') {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            push(Kind::Str, &chars[i..end]);
            i = end;
        } else if in_tag && (c.is_alphabetic() || matches!(c, '_' | '@' | ':')) {
            let mut j = i + 1;
            while j < chars.len() && (word_char(chars[j]) || matches!(chars[j], '-' | ':' | '.')) {
                j += 1;
            }
            push(Kind::Attr, &chars[i..j]);
            i = j;
        } else {
            push(Kind::Plain, &chars[i..i + 1]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, code: &str) -> Vec<(Kind, String)> {
        highlight(lang, code)
            .into_iter()
            .flatten()
            .filter(|(k, _)| *k != Kind::Plain)
            .collect()
    }

    fn has(runs: &[(Kind, String)], kind: Kind, text: &str) -> bool {
        runs.iter().any(|(k, t)| *k == kind && t == text)
    }

    #[test]
    fn rust() {
        let r = kinds(
            "rust",
            "/// Doc\n#[derive(Debug)]\npub fn read<'a>(p: &'a Path) -> io::Result<u64> {\n    let n = MAX_INLINE * 2; // cap\n    format!(\"{}\\n\", 'x')\n}",
        );
        assert!(has(&r, Kind::Comment, "/// Doc"), "{r:?}");
        assert!(has(&r, Kind::Attr, "#[derive(Debug)]"), "{r:?}");
        assert!(has(&r, Kind::Keyword, "pub"), "{r:?}");
        assert!(has(&r, Kind::Keyword, "fn"), "{r:?}");
        assert!(has(&r, Kind::Func, "read"), "{r:?}");
        assert!(has(&r, Kind::Attr, "'a"), "{r:?}");
        assert!(has(&r, Kind::Type, "Path"), "{r:?}");
        assert!(has(&r, Kind::Type, "u64"), "{r:?}");
        assert!(has(&r, Kind::Const, "MAX_INLINE"), "{r:?}");
        assert!(has(&r, Kind::Num, "2"), "{r:?}");
        assert!(has(&r, Kind::Comment, "// cap"), "{r:?}");
        assert!(has(&r, Kind::Func, "format!"), "{r:?}");
        assert!(has(&r, Kind::Str, "\"{}\\n\""), "{r:?}");
        assert!(has(&r, Kind::Str, "'x'"), "{r:?}");
    }

    #[test]
    fn a_field_named_like_a_keyword_stays_plain() {
        let r = kinds("rust", "self.type = x.match(y)");
        assert!(!has(&r, Kind::Keyword, "type"), "{r:?}");
        assert!(has(&r, Kind::Func, "match"), "{r:?}");
    }

    #[test]
    fn lines_split_what_runs_over_them() {
        let l = highlight("python", "x = \"\"\"a\nb\"\"\"\n# done\n");
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(l[1], vec![(Kind::Str, "b\"\"\"".to_string())]);
        assert_eq!(l[2], vec![(Kind::Comment, "# done".to_string())]);
    }

    #[test]
    fn data_and_markup() {
        let j = kinds("json", "{\"t\": \"send\", \"n\": 3, \"ok\": true}");
        assert!(has(&j, Kind::Attr, "\"t\""), "{j:?}");
        assert!(has(&j, Kind::Str, "\"send\""), "{j:?}");
        assert!(has(&j, Kind::Const, "true"), "{j:?}");
        let t = kinds(
            "toml",
            "[permissions]\nmode = \"ask\" # default\nallow = [\"bash\"]",
        );
        assert!(has(&t, Kind::Type, "[permissions]"), "{t:?}");
        assert!(has(&t, Kind::Attr, "mode"), "{t:?}");
        assert!(has(&t, Kind::Comment, "# default"), "{t:?}");
        let y = kinds(
            "yaml",
            "- name: build\n  run: cargo test # all\n  url: https://x.y",
        );
        assert!(has(&y, Kind::Attr, "name"), "{y:?}");
        assert!(has(&y, Kind::Attr, "run"), "{y:?}");
        let h = kinds("html", "<!-- x --><a href=\"/y\" class='z'>hi</a>");
        assert!(has(&h, Kind::Comment, "<!-- x -->"), "{h:?}");
        assert!(has(&h, Kind::Tag, "a"), "{h:?}");
        assert!(has(&h, Kind::Attr, "href"), "{h:?}");
        assert!(has(&h, Kind::Str, "\"/y\""), "{h:?}");
        let s = kinds("sh", "export X=1 # set\necho \"$HOME\" ${Y} a#b");
        assert!(has(&s, Kind::Keyword, "export"), "{s:?}");
        assert!(has(&s, Kind::Comment, "# set"), "{s:?}");
        assert!(has(&s, Kind::Const, "${Y}"), "{s:?}");
        assert!(
            !s.iter()
                .any(|(k, t)| *k == Kind::Comment && t.contains('b')),
            "{s:?}"
        );
    }

    #[test]
    fn unknown_languages_and_text_stay_plain() {
        assert_eq!(
            highlight("text", "a \"b\"\nc"),
            vec![
                vec![(Kind::Plain, "a \"b\"".to_string())],
                vec![(Kind::Plain, "c".to_string())]
            ]
        );
        assert_eq!(highlight("rust", ""), vec![Vec::<Run>::new()]);
    }

    #[test]
    fn a_files_language_is_its_extension() {
        assert_eq!(language_of("src/web/files.rs"), "rs");
        assert_eq!(language_of("src/web.rs:120"), "rs");
        assert_eq!(language_of("Dockerfile"), "dockerfile");
        assert_eq!(language_of("README"), "");
    }
}
