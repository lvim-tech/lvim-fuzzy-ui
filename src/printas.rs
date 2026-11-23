//! The folder of a choice (Shift-Enter of lfz and lvc, for `cd "$(lfz)"`): a folder is itself, a file is the folder it is in.

use std::path::{Component, Path, PathBuf};

/// The folder of `line`: `line` made absolute against `base` (no symlinks resolved, `.` and `..` folded); of a file, the folder it is in.
pub fn folder(line: &str, base: &Path) -> String {
    let p = Path::new(line.strip_prefix("./").unwrap_or(line));
    let abs = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    if out.is_dir() { out } else { out.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")) }.to_string_lossy().into_owned()
}
