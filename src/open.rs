//! Carrying out what the OPEN plugin (`lvim-fuzzy-plugin-open`) decided. The plugin chooses which program opens a file
//! from its configuration and answers with an `open` instruction; a tool (lvc, lfz, the Neovim picker through
//! `lvim-plugin host`) reads it with [`parse`] and does it: start its editor, or [`spawn_detached`] the
//! command. Nothing here decides anything.
//!
//! The answer (a JSON array of instructions, the first `open` or `pass` counts):
//! `{"op":"open","action":"editor"}` · `{"op":"open","action":"exec","cmd":"imv '/p/a.png'"}` ·
//! `{"op":"open","missing":["`imv` is not installed (…)"]}` · `{"op":"pass"}`.

use serde_json::Value;

/// What to do with a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// In the tool's own editor (lvc and lfz start it, the Neovim picker opens a buffer).
    Editor,
    /// A shell command with the file filled in (already quoted); run detached.
    Exec(String),
    /// The program that would open it is not installed: one line each. Open nothing, say so.
    Missing(Vec<String>),
    /// No plugin has an answer.
    Pass,
}

/// Read an open plugin's answer.
pub fn parse(answer: &str) -> Result<Open, String> {
    let v: Value = serde_json::from_str(answer).map_err(|e| format!("bad JSON: {e}"))?;
    let list = v.as_array().ok_or("the answer must be an array of instructions")?;
    for ins in list {
        match ins.get("op").and_then(Value::as_str) {
            Some("pass") => return Ok(Open::Pass),
            Some("open") => {
                let missing: Vec<String> =
                    ins.get("missing").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                if !missing.is_empty() {
                    return Ok(Open::Missing(missing));
                }
                return match ins.get("action").and_then(Value::as_str) {
                    Some("editor") => Ok(Open::Editor),
                    Some("exec") => match ins.get("cmd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                        Some(c) => Ok(Open::Exec(c.to_string())),
                        None => Err("an `exec` open has no `cmd`".into()),
                    },
                    other => Err(format!("unknown open action: {}", other.unwrap_or("(none)"))),
                };
            }
            _ => {}
        }
    }
    Err("the answer has no `open` (or `pass`) instruction".into())
}

/// Run a shell command detached from the terminal and from this program (a GUI viewer): no stdio, its own session.
pub fn spawn_detached(line: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut c = Command::new("sh");
    c.arg("-c").arg(line).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_answer() {
        assert_eq!(parse(r#"[{"op":"open","action":"editor"}]"#).unwrap(), Open::Editor);
        assert_eq!(parse(r#"[{"op":"open","action":"exec","cmd":"imv 'a'"}]"#).unwrap(), Open::Exec("imv 'a'".into()));
        assert_eq!(parse(r#"[{"op":"open","missing":["imv is not installed"]}]"#).unwrap(), Open::Missing(vec!["imv is not installed".into()]));
        assert_eq!(parse(r#"[{"op":"pass"}]"#).unwrap(), Open::Pass);
        // a message before it is skipped
        assert_eq!(parse(r#"[{"op":"message","text":"x"},{"op":"open","action":"editor"}]"#).unwrap(), Open::Editor);
    }

    #[test]
    fn mistakes_are_said() {
        assert!(parse("[]").is_err());
        assert!(parse("{}").is_err());
        assert!(parse("nope").is_err());
        assert!(parse(r#"[{"op":"open","action":"exec"}]"#).is_err());
        assert!(parse(r#"[{"op":"open","action":"fly"}]"#).is_err());
    }
}
