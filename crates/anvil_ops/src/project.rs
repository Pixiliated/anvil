//! An Anvil project: a directory with `anvil.toml`, naming the files that belong to it.
//!
//! ```toml
//! [project]
//! name = "orders"
//! files = ["src/*.anv", "tools/report.anv"]   # `*` matches within one directory
//!
//! [golden]
//! dir = "golden"                              # optional; this is the default
//! ```
//!
//! Unknown keys, patterns that match nothing and files listed twice are errors.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE: &str = "anvil.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Toml {
    project: ProjectToml,
    golden: Option<GoldenToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectToml {
    name: String,
    files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenToml {
    dir: String,
}

pub struct Project {
    pub root: PathBuf,
    pub name: String,
    /// Relative to the root, with `/`, sorted.
    pub files: Vec<String>,
    /// Relative to the root.
    pub golden_dir: String,
}

impl Project {
    /// The nearest `anvil.toml` at or above `start`.
    pub fn discover(start: &Path) -> Result<Project, String> {
        let start = plain(start.canonicalize().map_err(|e| format!("cannot read {}: {e}", start.display()))?);
        let mut dir = Some(start.as_path());
        while let Some(d) = dir {
            if d.join(FILE).is_file() {
                return Project::load(d);
            }
            dir = d.parent();
        }
        Err(format!(
            "no {FILE} in {} or any directory above it; create one:\n[project]\nname = \"my_project\"\nfiles = [\"src/*.anv\"]",
            start.display()
        ))
    }

    pub fn load(root: &Path) -> Result<Project, String> {
        let path = root.join(FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let t: Toml = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if t.project.files.is_empty() {
            return Err(format!("{}: [project] files is empty; list the .anv files or patterns", path.display()));
        }
        let mut files = Vec::new();
        for pattern in &t.project.files {
            let matched = expand(root, pattern)?;
            if matched.is_empty() {
                return Err(format!("{}: `{pattern}` matches no file", path.display()));
            }
            for f in matched {
                if files.contains(&f) {
                    return Err(format!("{}: `{f}` is listed twice (again by `{pattern}`)", path.display()));
                }
                files.push(f);
            }
        }
        files.sort();
        let golden_dir = t.golden.map(|g| g.dir).unwrap_or_else(|| "golden".to_string());
        Ok(Project { root: root.to_path_buf(), name: t.project.name, files, golden_dir })
    }

    pub fn contains(&self, file: &str) -> bool {
        self.files.iter().any(|f| f == file)
    }

    /// The project-relative name of a path given by the user (relative to `cwd` or absolute).
    pub fn relative(&self, cwd: &Path, file: &str) -> Result<String, String> {
        let full = cwd.join(file);
        let full = plain(full.canonicalize().map_err(|e| format!("cannot read {file}: {e}"))?);
        let rel = full
            .strip_prefix(&self.root)
            .map_err(|_| format!("{file} is outside the project at {}", self.root.display()))?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    }
}

/// Files matching `pattern`: a path whose last part may contain `*`; `**` is not supported.
fn expand(root: &Path, pattern: &str) -> Result<Vec<String>, String> {
    if pattern.contains("**") || pattern.contains('\\') {
        return Err(format!("`{pattern}`: patterns use `/` and `*` within one directory; `**` is not supported"));
    }
    let (dir, name) = pattern.rsplit_once('/').unwrap_or(("", pattern));
    if dir.contains('*') {
        return Err(format!("`{pattern}`: `*` may only appear in the file name"));
    }
    if !name.ends_with(".anv") {
        return Err(format!("`{pattern}`: project files are .anv files"));
    }
    let base = root.join(dir);
    let mut out = Vec::new();
    if !base.is_dir() {
        return Ok(out);
    }
    if !name.contains('*') {
        if base.join(name).is_file() {
            out.push(pattern.to_string());
        }
        return Ok(out);
    }
    let entries = std::fs::read_dir(&base).map_err(|e| format!("`{pattern}`: cannot read {}: {e}", base.display()))?;
    for e in entries {
        let e = e.map_err(|e| e.to_string())?;
        let n = e.file_name().to_string_lossy().to_string();
        if e.path().is_file() && wildcard(name, &n) {
            out.push(if dir.is_empty() { n } else { format!("{dir}/{n}") });
        }
    }
    out.sort();
    Ok(out)
}

/// Without Windows' `\\?\` prefix, which canonicalize adds and people do not write.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// `*` matches any run of characters; everything else matches itself.
fn wildcard(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => {
            let Some(tail) = text.strip_prefix(head) else { return false };
            (0..=tail.len()).any(|i| tail.is_char_boundary(i) && wildcard(rest, &tail[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn wildcards() {
        assert!(wildcard("*.anv", "a.anv"));
        assert!(wildcard("panic_*.anv", "panic_index.anv"));
        assert!(!wildcard("panic_*.anv", "arith.anv"));
        assert!(!wildcard("*.anv", "a.anv.out"));
        assert!(wildcard("a*b*c.anv", "abxbc.anv"));
    }
}
