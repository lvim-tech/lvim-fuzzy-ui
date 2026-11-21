//! Archives, through the usual programs (tar, unzip / zip, 7z, unrar): what an archive is, how to list,
//! extract and make one. A program that is missing is reported as the job's error.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Tar,
    TarGz,
    TarXz,
    TarBz2,
    TarZst,
    Zip,
    SevenZ,
    Rar,
    /// read through libarchive (`bsdtar`): .deb .rpm .iso .cpio .cab .xar .tar.lz .tar.lzma …
    Bsd,
}

/// Is `prog` on the PATH?
fn have(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

/// The format of an archive by its name.
pub fn format(name: &str) -> Option<Format> {
    let n = name.to_ascii_lowercase();
    let ends = |s: &[&str]| s.iter().any(|e| n.ends_with(e));
    Some(if ends(&[".tar.gz", ".tgz"]) {
        Format::TarGz
    } else if ends(&[".tar.xz", ".txz"]) {
        Format::TarXz
    } else if ends(&[".tar.bz2", ".tbz2", ".tbz"]) {
        Format::TarBz2
    } else if ends(&[".tar.zst", ".tzst"]) {
        Format::TarZst
    } else if ends(&[".tar"]) {
        Format::Tar
    } else if ends(&[".zip", ".jar", ".apk", ".epub"]) {
        Format::Zip
    } else if ends(&[".7z"]) {
        Format::SevenZ
    } else if ends(&[".rar"]) {
        Format::Rar
    } else if ends(&[".deb", ".rpm", ".iso", ".cpio", ".cab", ".xar", ".pkg", ".tar.lz", ".tlz", ".tar.lzma", ".lha", ".lzh"]) {
        Format::Bsd
    } else {
        return None;
    })
}

/// The archive's name without its archive extension (`a.tar.gz` → `a`).
pub fn stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for e in [
        ".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".tgz", ".txz", ".tbz2", ".tbz", ".tzst", ".tar", ".zip",
        ".jar", ".apk", ".epub", ".7z", ".rar", ".deb", ".rpm", ".iso", ".cpio", ".cab", ".xar", ".pkg", ".tar.lz",
        ".tlz", ".tar.lzma", ".lha", ".lzh",
    ] {
        if lower.ends_with(e) {
            return name[..name.len() - e.len()].to_string();
        }
    }
    name.to_string()
}


/// Extract `archive` into `dest` (made first).
pub fn extract_cmd(archive: &Path, dest: &Path) -> Option<Command> {
    let f = format(&archive.file_name()?.to_string_lossy())?;
    let mut c;
    let own = match f {
        Format::Zip => "unzip",
        Format::SevenZ => "7z",
        Format::Rar => "unrar",
        Format::Bsd => "bsdtar",
        _ => "tar",
    };
    if f == Format::Bsd || (!have(own) && have("bsdtar")) {
        c = Command::new("bsdtar");
        c.arg("-xf").arg(archive).arg("-C").arg(dest);
        return Some(c);
    }
    match f {
        Format::Zip => {
            c = Command::new("unzip");
            c.arg("-q").arg("-o").arg(archive).arg("-d").arg(dest);
        }
        Format::SevenZ => {
            c = Command::new("7z");
            c.arg("x").arg("-y").arg(format!("-o{}", dest.display())).arg(archive);
        }
        Format::Rar => {
            c = Command::new("unrar");
            c.arg("x").arg("-o+").arg(archive).arg(format!("{}/", dest.display()));
        }
        Format::TarZst => {
            c = Command::new("tar");
            c.arg("--zstd").arg("-xf").arg(archive).arg("-C").arg(dest);
        }
        _ => {
            c = Command::new("tar");
            c.arg("-xf").arg(archive).arg("-C").arg(dest);
        }
    }
    Some(c)
}

/// Pack `names` (relative to `dir`) into `out`; the format comes from `out`'s extension.
pub fn compress_cmd(dir: &Path, names: &[String], out: &Path) -> Result<Command, String> {
    let fname = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let f = format(&fname).ok_or_else(|| {
        format!("{fname}: not an archive name (.tar.gz .tar.xz .tar.bz2 .tar.zst .tar .zip .7z)")
    })?;
    let mut c;
    match f {
        Format::Zip => {
            c = Command::new("zip");
            c.arg("-qr").arg(out);
        }
        Format::SevenZ => {
            c = Command::new("7z");
            c.arg("a").arg(out);
        }
        Format::Rar => return Err("rar archives cannot be made (unrar only reads them)".into()),
        Format::Bsd => return Err(format!("{fname}: this kind is only read and extracted here")),
        Format::Tar => {
            c = Command::new("tar");
            c.arg("-cf").arg(out);
        }
        Format::TarGz => {
            c = Command::new("tar");
            c.arg("-czf").arg(out);
        }
        Format::TarXz => {
            c = Command::new("tar");
            c.arg("-cJf").arg(out);
        }
        Format::TarBz2 => {
            c = Command::new("tar");
            c.arg("-cjf").arg(out);
        }
        Format::TarZst => {
            c = Command::new("tar");
            c.arg("--zstd").arg("-cf").arg(out);
        }
    }
    // the names after `--` (tar) so one that starts with `-` is never an option
    if matches!(f, Format::Tar | Format::TarGz | Format::TarXz | Format::TarBz2 | Format::TarZst) {
        c.arg("--");
    }
    c.args(names).current_dir(dir);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_stems() {
        assert_eq!(format("a.tar.gz"), Some(Format::TarGz));
        assert_eq!(format("A.ZIP"), Some(Format::Zip));
        assert_eq!(format("a.txt"), None);
        assert_eq!(stem("release-1.0.tar.xz"), "release-1.0");
        assert_eq!(stem("x.7z"), "x");
    }

    #[test]
    fn packs_and_unpacks_with_tar() {
        if Command::new("tar").arg("--version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("lvc-arc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/inner")).unwrap();
        std::fs::write(d.join("src/inner/a.txt"), "a").unwrap();
        let out = d.join("pack.tar.gz");
        let st = compress_cmd(&d, &["src".into()], &out).unwrap().status().unwrap();
        assert!(st.success());
        std::fs::create_dir_all(d.join("x")).unwrap();
        let st = extract_cmd(&out, &d.join("x")).unwrap().status().unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read_to_string(d.join("x/src/inner/a.txt")).unwrap(), "a");
        let rows = tree(&out).unwrap();
        assert!(rows.iter().any(|r| r.name == "a.txt" && r.depth == 2));
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// A line of an archive's tree (the quick view): how deep, the name, a directory or not, the size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// The archive as a tree: directories first, then files, each level by name (natural order). Read with
/// `bsdtar -tv` (libarchive reads every format here); the error text when it cannot be listed.
pub fn tree(archive: &Path) -> Result<Vec<Row>, String> {
    if !have("bsdtar") {
        return Err("bsdtar (libarchive) is needed for the tree".into());
    }
    let o = Command::new("bsdtar").arg("-tvf").arg(archive).output().map_err(|e| e.to_string())?;
    if !o.status.success() && o.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    let mut entries: Vec<(String, bool, u64)> = Vec::new();
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        // perms links owner group size month day time|year name…
        let mut rest = line;
        let mut f = Vec::new();
        for _ in 0..8 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            f.push(&rest[..end]);
            rest = &rest[end..];
        }
        let name = rest.trim_start();
        let name = name.split(" -> ").next().unwrap_or(name).trim_start_matches("./");
        if f.len() < 8 || name.is_empty() || name == "." {
            continue;
        }
        let dir = f[0].starts_with('d') || name.ends_with('/');
        let size = f[4].parse().unwrap_or(0);
        entries.push((name.trim_end_matches('/').to_string(), dir, size));
    }
    Ok(build_tree(entries))
}

fn build_tree(entries: Vec<(String, bool, u64)>) -> Vec<Row> {
    #[derive(Default)]
    struct Node {
        dirs: std::collections::BTreeMap<String, Node>,
        files: Vec<(String, u64)>,
    }
    let mut root = Node::default();
    for (path, dir, size) in entries {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let Some((last, parents)) = parts.split_last() else { continue };
        let mut n = &mut root;
        for p in parents {
            n = n.dirs.entry(p.to_string()).or_default();
        }
        if dir {
            n.dirs.entry(last.to_string()).or_default();
        } else {
            n.files.push((last.to_string(), size));
        }
    }
    fn walk(n: &Node, depth: usize, out: &mut Vec<Row>) {
        let mut dirs: Vec<(&String, &Node)> = n.dirs.iter().collect();
        dirs.sort_by(|a, b| crate::fsinfo::natural_cmp(a.0, b.0));
        for (name, child) in dirs {
            out.push(Row { depth, name: name.clone(), dir: true, size: 0 });
            walk(child, depth + 1, out);
        }
        let mut files: Vec<&(String, u64)> = n.files.iter().collect();
        files.sort_by(|a, b| crate::fsinfo::natural_cmp(&a.0, &b.0));
        for (name, size) in files {
            out.push(Row { depth, name: name.clone(), dir: false, size: *size });
        }
    }
    let mut out = Vec::new();
    walk(&root, 0, &mut out);
    out
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn builds_a_tree_dirs_first() {
        let rows = build_tree(vec![
            ("b.txt".into(), false, 3),
            ("sub/inner.txt".into(), false, 2),
            ("a/".into(), true, 0),
            ("sub/deep/x".into(), false, 1),
        ]);
        let shown: Vec<(usize, &str)> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
        assert_eq!(shown, vec![(0, "a"), (0, "sub"), (1, "deep"), (2, "x"), (1, "inner.txt"), (0, "b.txt")]);
    }
}
