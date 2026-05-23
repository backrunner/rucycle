use std::{
    borrow::Cow,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use ignore::{DirEntry, WalkBuilder};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Size,
    Modified,
}

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub name: String,
    pub total_size: u64,
    pub target_size: u64,
    pub last_modified: SystemTime,
}

impl ProjectInfo {
    pub fn display_path<'a>(&'a self, scan_root: &'a Path) -> Cow<'a, Path> {
        let relative = self.root.strip_prefix(scan_root).unwrap_or(&self.root);
        if relative.as_os_str().is_empty() {
            Cow::Borrowed(Path::new("."))
        } else {
            Cow::Borrowed(relative)
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScanWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

impl ScanWarning {
    fn new(path: Option<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScanReport {
    pub scan_root: PathBuf,
    pub projects: Vec<ProjectInfo>,
    pub warnings: Vec<ScanWarning>,
}

#[derive(Clone, Debug)]
struct Candidate {
    root: PathBuf,
    manifest_path: PathBuf,
}

#[derive(Default)]
struct ProjectStats {
    total_size: u64,
    target_size: u64,
    last_modified: Option<SystemTime>,
    warnings: Vec<ScanWarning>,
}

pub fn resolve_scan_root(input: &Path) -> Result<PathBuf> {
    if !input.exists() {
        bail!("path does not exist");
    }

    if input.is_file() {
        if input.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml") {
            return input
                .parent()
                .context("Cargo.toml path has no parent directory")?
                .canonicalize()
                .context("failed to canonicalize Cargo.toml parent directory");
        }
        bail!("path is a file; pass a directory or a Cargo.toml");
    }

    if !input.is_dir() {
        bail!("path is not a directory");
    }

    input
        .canonicalize()
        .context("failed to canonicalize scan directory")
}

pub fn discover_projects(scan_root: &Path, sort: SortKey) -> Result<ScanReport> {
    let (candidates, mut warnings) = find_candidates(scan_root)?;

    let mut seen = HashSet::new();
    let unique: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.root.clone()))
        .collect();

    let results: Vec<(ProjectInfo, Vec<ScanWarning>)> = unique
        .par_iter()
        .map(|candidate| {
            let stats = compute_stats(&candidate.root);
            let name = candidate
                .root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_else(|| candidate.root.as_os_str().to_str().unwrap_or("project"))
                .to_string();

            let last_modified = stats
                .last_modified
                .unwrap_or(SystemTime::UNIX_EPOCH + Duration::from_secs(0));

            let project = ProjectInfo {
                root: candidate.root.clone(),
                manifest_path: candidate.manifest_path.clone(),
                name,
                total_size: stats.total_size,
                target_size: stats.target_size,
                last_modified,
            };
            (project, stats.warnings)
        })
        .collect();

    let mut projects = Vec::with_capacity(results.len());
    for (project, project_warnings) in results {
        warnings.extend(project_warnings);
        projects.push(project);
    }

    sort_projects(&mut projects, sort);

    Ok(ScanReport {
        scan_root: scan_root.to_path_buf(),
        projects,
        warnings,
    })
}

pub fn sort_projects(projects: &mut [ProjectInfo], sort: SortKey) {
    match sort {
        SortKey::Size => projects.sort_by(|left, right| {
            right
                .target_size
                .cmp(&left.target_size)
                .then_with(|| right.total_size.cmp(&left.total_size))
                .then_with(|| right.last_modified.cmp(&left.last_modified))
                .then_with(|| left.root.cmp(&right.root))
        }),
        SortKey::Modified => projects.sort_by(|left, right| {
            right
                .last_modified
                .cmp(&left.last_modified)
                .then_with(|| right.target_size.cmp(&left.target_size))
                .then_with(|| left.root.cmp(&right.root))
        }),
    }
}

fn find_candidates(scan_root: &Path) -> Result<(Vec<Candidate>, Vec<ScanWarning>)> {
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();

    let walker = WalkBuilder::new(scan_root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| !is_discovery_skip_dir(entry))
        .build();

    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                warnings.push(ScanWarning::new(None, err.to_string()));
                continue;
            }
        };

        if !is_cargo_manifest(&entry) {
            continue;
        }

        let manifest_path = entry.path().to_path_buf();
        match is_rust_manifest(&manifest_path) {
            Ok(true) => {
                if let Some(root) = manifest_path.parent() {
                    candidates.push(Candidate {
                        root: root.to_path_buf(),
                        manifest_path,
                    });
                }
            }
            Ok(false) => {}
            Err(err) => warnings.push(ScanWarning::new(Some(manifest_path), err.to_string())),
        }
    }

    Ok((candidates, warnings))
}

fn is_cargo_manifest(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .map(|file_type| file_type.is_file())
        .unwrap_or(false)
        && entry.file_name() == "Cargo.toml"
}

fn is_rust_manifest(path: &Path) -> Result<bool> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value = contents
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value.get("package").is_some() || value.get("workspace").is_some())
}

fn compute_stats(root: &Path) -> ProjectStats {
    let mut stats = ProjectStats::default();
    let target_root = root.join("target");

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| !is_stats_skip_dir(entry))
        .build();

    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                stats.warnings.push(ScanWarning::new(None, err.to_string()));
                continue;
            }
        };

        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(err) => {
                stats.warnings.push(ScanWarning::new(
                    Some(entry.path().to_path_buf()),
                    err.to_string(),
                ));
                continue;
            }
        };

        if metadata.file_type().is_file() {
            stats.total_size = stats.total_size.saturating_add(metadata.len());
            if entry.path().starts_with(&target_root) {
                stats.target_size = stats.target_size.saturating_add(metadata.len());
            }
            if let Ok(modified) = metadata.modified() {
                stats.last_modified = Some(match stats.last_modified {
                    Some(current) => current.max(modified),
                    None => modified,
                });
            }
        }
    }

    stats
}

fn is_discovery_skip_dir(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }

    entry
        .file_type()
        .map(|file_type| file_type.is_dir())
        .unwrap_or(false)
        && matches!(
            entry.file_name().to_str(),
            Some("target" | "node_modules" | ".git" | ".hg" | ".svn")
        )
}

fn is_stats_skip_dir(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }

    entry
        .file_type()
        .map(|file_type| file_type.is_dir())
        .unwrap_or(false)
        && matches!(entry.file_name().to_str(), Some(".git" | ".hg" | ".svn"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_projects_and_sizes_target() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(project.join("target/debug/app"), vec![0_u8; 2048]).unwrap();

        let report = discover_projects(temp.path(), SortKey::Size).unwrap();
        assert_eq!(report.projects.len(), 1);
        assert_eq!(report.projects[0].name, "demo");
        assert_eq!(report.projects[0].target_size, 2048);
        assert!(report.projects[0].total_size >= 2048);
    }

    #[test]
    fn ignores_non_cargo_toml() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[not_package]\nvalue = true\n",
        )
        .unwrap();

        let report = discover_projects(temp.path(), SortKey::Size).unwrap();
        assert!(report.projects.is_empty());
    }
}
