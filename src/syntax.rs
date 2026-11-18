//! The syntaxes code is highlighted with, in the terminal (`app::markdown`) and in the
//! web UI (`web::render`): one set for both, and one way to find a code block's.

use std::sync::OnceLock;

use syntect::parsing::{SyntaxReference, SyntaxSet};

/// syntect's default syntaxes, loaded on first use.
pub fn set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The syntax for a fenced code block, from the language its fence names: `rust`, or the
/// first word of `rust,ignore`. `None` for plain text and for languages the set lacks.
pub fn for_language(lang: &str) -> Option<&'static SyntaxReference> {
    let lang = lang
        .split([',', ' ', '{'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    // Names the set doesn't know, by the syntax that reads them best.
    let token = match lang.as_str() {
        "" => return None,
        "ts" | "tsx" | "typescript" | "jsx" | "mjs" | "cjs" => "js",
        "shell" | "zsh" | "console" => "bash",
        "jsonc" | "json5" => "json",
        "golang" => "go",
        other => other,
    };
    set()
        .find_syntax_by_token(token)
        .filter(|syntax| syntax.name != "Plain Text")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(lang: &str) -> Option<&'static str> {
        for_language(lang).map(|syntax| syntax.name.as_str())
    }

    #[test]
    fn languages_are_found_by_name_extension_or_alias() {
        for (lang, syntax) in [
            ("rust", "Rust"),
            ("Rust,ignore", "Rust"),
            ("py", "Python"),
            ("html artifact", "HTML"),
            ("tsx", "JavaScript"),
            ("mjs", "JavaScript"),
            ("console", "Bourne Again Shell (bash)"),
            ("sh", "Bourne Again Shell (bash)"),
            ("json5", "JSON"),
            ("yml", "YAML"),
            ("golang", "Go"),
            ("c++", "C++"),
            ("patch", "Diff"),
        ] {
            assert_eq!(name(lang), Some(syntax), "{lang}");
        }
    }

    #[test]
    fn plain_text_and_unknown_languages_have_no_syntax() {
        for lang in ["", "txt", "text", "plaintext", "no-such-language"] {
            assert_eq!(name(lang), None, "{lang}");
        }
    }
}
