//! Syntax colouring for the built-in file preview, with syntect's grammars
//! (the ~150 languages Sublime Text / bat know). syntect only says what each
//! piece of text IS (a comment, a string, a function name, ...); the colours
//! come from the theme's palette, so the preview wears the lvim theme.
//!
//! Files are parsed off the UI thread (with the file read), once per file.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Comment,
    String,
    Escape,
    Number,
    Constant,
    Keyword,
    Func,
    Type,
    Tag,
    Attribute,
    Heading,
    Emphasis,
}

/// A run of chars [start, end) in a line.
pub type Span = (u32, u32, Tok);

fn set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The grammar for `path` (by extension / file name, then by its first line).
fn syntax_for(path: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = set();
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

/// What a scope stack means for the colour (innermost scope wins).
fn classify(stack: &ScopeStack) -> Option<Tok> {
    for scope in stack.as_slice().iter().rev() {
        let s = scope.build_string();
        let t = if s.starts_with("comment") {
            Tok::Comment
        } else if s.starts_with("constant.character.escape") {
            Tok::Escape
        } else if s.starts_with("string") {
            Tok::String
        } else if s.starts_with("constant.numeric") {
            Tok::Number
        } else if s.starts_with("constant") || s.starts_with("support.constant") || s.starts_with("variable.language") {
            Tok::Constant
        } else if s.starts_with("entity.name.function")
            || s.starts_with("support.function")
            || s.starts_with("variable.function")
            || s.starts_with("meta.function-call")
        {
            Tok::Func
        } else if s.starts_with("entity.name.type")
            || s.starts_with("entity.name.class")
            || s.starts_with("support.type")
            || s.starts_with("support.class")
            || s.starts_with("storage.type")
            || s.starts_with("entity.other.inherited-class")
        {
            Tok::Type
        } else if s.starts_with("keyword") || s.starts_with("storage") {
            Tok::Keyword
        } else if s.starts_with("entity.name.tag") {
            Tok::Tag
        } else if s.starts_with("entity.other.attribute-name") || s.starts_with("meta.attribute") {
            Tok::Attribute
        } else if s.starts_with("markup.heading") || s.starts_with("entity.name.section") {
            Tok::Heading
        } else if s.starts_with("markup.bold") || s.starts_with("markup.italic") || s.starts_with("markup.raw") {
            Tok::Emphasis
        } else {
            continue;
        };
        return Some(t);
    }
    None
}

/// Token spans for every line of a file (empty when no grammar knows it).
/// Long files are coloured up to `max_lines`.
#[cfg(test)]
pub fn highlight_file(path: &str, lines: &[String], max_lines: usize) -> Vec<Vec<Span>> {
    highlight_file_until(path, lines, max_lines, &|| false).unwrap_or_default()
}

/// `highlight_file`, given up (None) as soon as `stop` says so; it is asked
/// every 32 lines.
pub fn highlight_file_until(path: &str, lines: &[String], max_lines: usize, stop: &dyn Fn() -> bool) -> Option<Vec<Vec<Span>>> {
    let Some(syntax) = syntax_for(path, lines.first().map(String::as_str).unwrap_or("")) else {
        return Some(Vec::new());
    };
    let ss = set();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::with_capacity(lines.len().min(max_lines));
    for (n, line) in lines.iter().take(max_lines).enumerate() {
        if n % 32 == 0 && stop() {
            return None;
        }
        let text = format!("{line}\n");
        let Ok(ops) = state.parse_line(&text, ss) else {
            break;
        };
        // byte offsets → char offsets
        let mut spans: Vec<Span> = Vec::new();
        let mut last_byte = 0usize;
        let push = |from: usize, to: usize, stack: &ScopeStack, spans: &mut Vec<Span>| {
            if to <= from || from >= line.len() {
                return;
            }
            let to = to.min(line.len());
            if let Some(t) = classify(stack) {
                // byte offsets from the parser: only on character boundaries
                // (a slice inside a multi-byte character would panic)
                let (Some(head), Some(mid)) = (line.get(..from), line.get(from..to)) else {
                    return;
                };
                let c0 = head.chars().count() as u32;
                let c1 = c0 + mid.chars().count() as u32;
                match spans.last_mut() {
                    Some(l) if l.1 == c0 && l.2 == t => l.1 = c1,
                    _ => spans.push((c0, c1, t)),
                }
            }
        };
        for (at, op) in ops {
            push(last_byte, at, &stack, &mut spans);
            let _ = stack.apply(&op);
            last_byte = at;
        }
        push(last_byte, line.len(), &stack, &mut spans);
        out.push(spans);
    }
    Some(out)
}

/// The palette accent a token wears (and whether it is italic / bold).
pub fn accent(t: Tok) -> (&'static str, bool, bool) {
    match t {
        Tok::Comment => ("comment", true, false),
        Tok::String => ("green", false, false),
        Tok::Escape => ("cyan", false, false),
        Tok::Number => ("orange", false, false),
        Tok::Constant => ("orange", false, false),
        Tok::Keyword => ("red", false, false),
        Tok::Func => ("blue", false, false),
        Tok::Type => ("yellow", false, false),
        Tok::Tag => ("red", false, false),
        Tok::Attribute => ("orange", false, false),
        Tok::Heading => ("blue", false, true),
        Tok::Emphasis => ("yellow", false, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_a_lua_file() {
        let lines: Vec<String> = ["local x = foo(\"s\") -- note", "return x"].iter().map(|s| s.to_string()).collect();
        let spans = highlight_file("a.lua", &lines, 100);
        assert_eq!(spans.len(), 2);
        let at = |line: usize, c: u32| spans[line].iter().find(|s| s.0 <= c && c < s.1).map(|s| s.2);
        assert_eq!(at(0, 0), Some(Tok::Keyword)); // local
        assert_eq!(at(0, 15), Some(Tok::String)); // "s"
        assert_eq!(at(0, 22), Some(Tok::Comment));
        assert!(highlight_file("x.nosuchext", &lines, 100).is_empty());
    }

    #[test]
    fn knows_many_languages() {
        for (f, src) in [
            ("a.rs", "fn main() {}"),
            ("a.py", "def f(): pass"),
            ("a.go", "func main() {}"),
            ("a.json", "{\"a\": 1}"),
            ("a.html", "<div class=\"x\"></div>"),
            ("a.sh", "echo \"hi\""),
            ("a.md", "# Title"),
            ("Makefile", "all:\n\techo x"),
        ] {
            let l = vec![src.to_string()];
            assert!(!highlight_file(f, &l, 10)[0].is_empty(), "{f}");
        }
    }
}
