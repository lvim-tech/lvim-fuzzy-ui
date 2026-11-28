//! Small helpers shared by the programs.

/// Make `path` (and its parents) a directory of ours that nobody else can enter: created `0700`, and when it was there already it must be a
/// real directory (not a link) owned by this user — a socket or a temporary file in a directory somebody else made (or can write to) is
/// theirs for the asking. Else an error saying so.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let m = std::fs::symlink_metadata(path)?;
    // SAFETY: getuid has no preconditions
    let me = unsafe { libc::getuid() };
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(std::io::Error::other(format!("{} is not a directory of its own", path.display())));
    }
    if m.uid() != me {
        return Err(std::io::Error::other(format!("{} belongs to someone else", path.display())));
    }
    // ours: closed to the others if it was not (a directory made by an older version, or with a looser umask)
    if m.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The directory of this process's temporary files: `$XDG_RUNTIME_DIR/lvim-fuzzy/tmp-<pid>` (local, private to the user), else
/// `<tmp>/lvim-fuzzy-<uid>/tmp-<pid>`; made `0700` at the first use, with the leftovers of processes that are gone cleaned out. Every
/// temporary file of the tools lives here, so nothing predictable is left in the shared `/tmp`; [`cleanup_scratch`] removes it at the end.
pub fn scratch() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let mut base = crate::pool::dir();
        // a base that is not ours (made by someone else, a link) is not used: a directory with a name nobody can guess is made instead
        if private_dir(&base).is_err() {
            let unique = format!("lvim-fuzzy-{}-{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
            base = std::env::temp_dir().join(unique);
            let _ = private_dir(&base);
        }
        // the directories of processes that no longer run (a killed finder cannot clean up after itself)
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(pid) = name.strip_prefix("tmp-").and_then(|p| p.parse::<i32>().ok())
                    // SAFETY: signal 0 only asks whether the process exists
                    && unsafe { libc::kill(pid, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let d = base.join(format!("tmp-{}", std::process::id()));
        let _ = private_dir(&d);
        d
    })
    .clone()
}

/// Remove [`scratch`] (the end of the program; a directory never made is nothing to remove).
pub fn cleanup_scratch() {
    // (where `scratch` made it: its path is the one it handed out)
    let d = scratch();
    let _ = std::fs::remove_dir_all(&d);
}

/// Write `data` to a NEW file `path` (private: `0600`, never through a link an enemy left at that name).
pub fn write_new(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)
}

/// Split a string into shell words (single/double quotes and backslashes).
pub fn shell_words(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err("unterminated single quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => cur.push(x),
                            Some('\n') => {}
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => return Err("unterminated double quote".into()),
                        },
                        Some(x) => cur.push(x),
                        None => return Err("unterminated double quote".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match it.next() {
                    Some('\n') => {}
                    Some(x) => cur.push(x),
                    None => cur.push('\\'),
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

/// Standard base64 (with padding): the kitty graphics protocol, OSC 52.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod private_dir_tests {
    use super::private_dir;
    use std::os::unix::fs::PermissionsExt;

    /// A directory is made `0700`; one that was there with looser rights is closed; a link or a file in its place is refused.
    #[test]
    fn a_private_dir_is_ours_and_closed() {
        let base = std::env::temp_dir().join(format!("lvim-priv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let d = base.join("a/b");
        private_dir(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        // looser rights: closed
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        // a link is no directory of ours
        let link = base.join("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(private_dir(&link).is_err());
        // a file
        let f = base.join("file");
        std::fs::write(&f, b"x").unwrap();
        assert!(private_dir(&f).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
