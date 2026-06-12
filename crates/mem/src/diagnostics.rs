//! Deterministic, per-file syntax/structure checks — no LLM.
//!
//! Dispatched on file extension and intentionally conservative: every check
//! here must be near-zero false-positive, because its findings drive a cleanup
//! model that will *rewrite* the file. A wrong finding makes a good file worse.
//!
//! - JSON: validated exactly via `serde_json` (config files like `package.json`).
//! - Braced sources (js/ts/tsx/jsx/css/rs/go/c/java…): a string- and
//!   comment-aware balance scan that flags only the truncation signature —
//!   unclosed brackets or an unterminated string/comment at end of file. This
//!   is the most common cut-off-output failure, caught regardless of language.
//! - `.js`/`.mjs`/`.cjs`: TypeScript-only syntax leaking into plain JavaScript.
//!
//! Unknown extensions yield no findings. There is no per-language *parser* here
//! (that is the deferred build gate); these are structural smoke checks.

/// Syntax findings for one file. Empty when nothing is wrong (or the extension
/// is unrecognised).
pub(crate) fn file_diagnostics(path: &str, content: &str) -> Vec<String> {
    let ext = extension(path);
    let mut findings = Vec::new();
    match ext {
        "json" => json_diagnostics(content, &mut findings),
        "js" | "mjs" | "cjs" => {
            balance_diagnostics(content, &mut findings);
            typescript_only_syntax(content, &mut findings);
        }
        "ts" | "tsx" | "jsx" | "css" | "scss" | "rs" | "go" | "c" | "h" | "cpp" | "java" => {
            balance_diagnostics(content, &mut findings);
        }
        _ => {}
    }
    findings
}

fn extension(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((_, ext)) if !ext.contains('/') => ext,
        _ => "",
    }
}

fn json_diagnostics(content: &str, findings: &mut Vec<String>) {
    if content.trim().is_empty() {
        return; // emptiness is the auditor's job, not a JSON-syntax finding
    }
    if let Err(err) = serde_json::from_str::<serde_json::Value>(content) {
        findings.push(format!("invalid JSON: {err}"));
    }
}

fn typescript_only_syntax(content: &str, findings: &mut Vec<String>) {
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("interface ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("type ") && trimmed.contains('=')
        {
            findings.push(
                "JavaScript file contains TypeScript-only syntax (interface/enum/type)".to_string(),
            );
            return;
        }
    }
}

/// State of the balance scanner as it walks one file.
#[derive(PartialEq)]
enum Mode {
    Normal,
    LineComment,
    BlockComment,
    Single,
    Double,
    Template,
}

/// Walk the source tracking bracket depth, skipping the contents of strings and
/// comments, and flag only the truncation signature: an unclosed bracket or an
/// unterminated string/comment at end of file. Template-literal `${…}` is walked
/// as code so its braces still balance.
fn balance_diagnostics(content: &str, findings: &mut Vec<String>) {
    if content.trim().is_empty() {
        return;
    }
    let mut mode = Mode::Normal;
    // Bracket stack; for template `${` we remember to pop back into Template.
    let mut brackets: Vec<char> = Vec::new();
    let mut template_depth: Vec<usize> = Vec::new(); // bracket-len at each `${`
    let mut prev = '\0';
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        match mode {
            Mode::LineComment => {
                if c == '\n' {
                    mode = Mode::Normal;
                }
            }
            Mode::BlockComment => {
                if prev == '*' && c == '/' {
                    mode = Mode::Normal;
                    prev = '\0';
                    continue;
                }
            }
            Mode::Single => {
                if c == '\\' {
                    chars.next(); // skip escaped char
                } else if c == '\'' {
                    mode = Mode::Normal;
                }
            }
            Mode::Double => {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    mode = Mode::Normal;
                }
            }
            Mode::Template => {
                if c == '\\' {
                    chars.next();
                } else if c == '`' {
                    mode = Mode::Normal;
                } else if c == '$' && chars.peek() == Some(&'{') {
                    chars.next(); // consume '{'
                    template_depth.push(brackets.len());
                    brackets.push('{');
                    mode = Mode::Normal;
                }
            }
            Mode::Normal => match c {
                '/' if chars.peek() == Some(&'/') => mode = Mode::LineComment,
                '/' if chars.peek() == Some(&'*') => mode = Mode::BlockComment,
                '\'' => mode = Mode::Single,
                '"' => mode = Mode::Double,
                '`' => mode = Mode::Template,
                '(' | '[' | '{' => brackets.push(c),
                ')' | ']' | '}' => {
                    let opener = matching_opener(c);
                    if brackets.last() == Some(&opener) {
                        brackets.pop();
                        // Closing the `{` that opened a template substitution?
                        if c == '}' && template_depth.last() == Some(&brackets.len()) {
                            template_depth.pop();
                            mode = Mode::Template;
                        }
                    }
                    // A stray closer (more closers than openers) is left
                    // unreported: it is not the truncation signature and is
                    // easy to trip on regex/operators. We only trust "unclosed".
                }
                _ => {}
            },
        }
        prev = c;
    }

    if !brackets.is_empty() {
        findings.push(format!(
            "looks truncated: {} unclosed bracket(s) at end of file",
            brackets.len()
        ));
    } else if mode == Mode::Single || mode == Mode::Double || mode == Mode::Template {
        findings.push("looks truncated: unterminated string at end of file".to_string());
    } else if mode == Mode::BlockComment {
        findings.push("looks truncated: unterminated block comment at end of file".to_string());
    }
}

fn matching_opener(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => '\0',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_validates_exactly() {
        assert!(file_diagnostics("package.json", "{\"a\": 1}").is_empty());
        let bad = file_diagnostics("package.json", "{\"a\": 1,}");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("invalid JSON"), "got: {bad:?}");
    }

    #[test]
    fn balanced_typescript_is_clean() {
        let src = "export function f(a: number): number {\n  return (a + [1,2][0]);\n}\n";
        assert!(file_diagnostics("src/f.ts", src).is_empty());
    }

    #[test]
    fn truncated_file_is_flagged() {
        let src = "export function f() {\n  if (x) {\n    return 1;\n"; // cut off
        let f = file_diagnostics("src/f.ts", src);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("truncated"), "got: {f:?}");
    }

    #[test]
    fn template_literals_and_strings_do_not_false_positive() {
        let src =
            "const s = `a ${b ? '{' : ']'} c`;\nconst t = \"a ) b\";\n// trailing } in comment\n";
        assert!(
            file_diagnostics("src/s.ts", src).is_empty(),
            "got: {:?}",
            file_diagnostics("src/s.ts", src)
        );
    }

    #[test]
    fn unterminated_string_is_flagged() {
        let f = file_diagnostics("src/s.ts", "const s = \"oops\n");
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("unterminated string"), "got: {f:?}");
    }

    #[test]
    fn typescript_syntax_in_js_is_flagged() {
        let f = file_diagnostics("src/x.js", "interface Foo { a: number }\n");
        assert!(
            f.iter().any(|m| m.contains("TypeScript-only")),
            "got: {f:?}"
        );
    }

    #[test]
    fn unknown_extension_yields_nothing() {
        assert!(file_diagnostics("notes.md", "# hi { unbalanced").is_empty());
        assert!(file_diagnostics("script.py", "def f(:\n").is_empty());
    }
}
