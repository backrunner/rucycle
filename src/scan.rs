use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ScanPhase {
    #[default]
    Discovering,
    Measuring,
}

#[derive(Clone, Debug, Default)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub entries: u64,
    pub bytes: u64,
    pub found: usize,
    pub completed: usize,
    pub current_path: PathBuf,
}

#[derive(Clone, Default)]
pub struct ScanControl {
    progress: Arc<Mutex<ScanProgress>>,
    cancelled: Arc<AtomicBool>,
}

impl ScanControl {
    pub fn snapshot(&self) -> ScanProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn advance(&self, path: &Path, entries: u64, bytes: u64) {
        let mut progress = self.progress.lock().unwrap();
        progress.entries += entries;
        progress.bytes = progress.bytes.saturating_add(bytes);
        progress.current_path = path.to_path_buf();
    }
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
    discover_projects_with_progress(scan_root, sort, &ScanControl::default())
}

pub fn discover_projects_with_progress(
    scan_root: &Path,
    sort: SortKey,
    control: &ScanControl,
) -> Result<ScanReport> {
    let (candidates, mut warnings) = find_candidates(scan_root, control)?;

    let mut seen = HashSet::new();
    let unique: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.root.clone()))
        .collect();
    let project_roots = Arc::new(seen);

    *control.progress.lock().unwrap() = ScanProgress {
        phase: ScanPhase::Measuring,
        found: unique.len(),
        current_path: scan_root.to_path_buf(),
        ..ScanProgress::default()
    };

    let results: Vec<(ProjectInfo, Vec<ScanWarning>)> = unique
        .par_iter()
        .map(|candidate| {
            let stats = compute_stats(&candidate.root, &project_roots, control);
            control.progress.lock().unwrap().completed += 1;
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

    if control.is_cancelled() {
        bail!("scan cancelled");
    }

    let mut projects = Vec::with_capacity(results.len());
    for (project, project_warnings) in results {
        warnings.extend(project_warnings);
        projects.push(project);
    }

    // Each file is measured once. Roll nested project totals up afterwards,
    // keeping each project's local target size independent for cleanup.
    let by_root: HashMap<_, _> = projects
        .iter()
        .enumerate()
        .map(|(index, project)| (project.root.clone(), index))
        .collect();
    let mut deepest_first: Vec<_> = (0..projects.len()).collect();
    deepest_first
        .sort_by_key(|&index| std::cmp::Reverse(projects[index].root.components().count()));
    for index in deepest_first {
        if let Some(parent) = projects[index]
            .root
            .ancestors()
            .skip(1)
            .find_map(|path| by_root.get(path).copied())
        {
            projects[parent].total_size = projects[parent]
                .total_size
                .saturating_add(projects[index].total_size);
            projects[parent].last_modified = projects[parent]
                .last_modified
                .max(projects[index].last_modified);
        }
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

fn find_candidates(
    scan_root: &Path,
    control: &ScanControl,
) -> Result<(Vec<Candidate>, Vec<ScanWarning>)> {
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

    let mut entries = 0;
    for result in walker {
        if control.is_cancelled() {
            bail!("scan cancelled");
        }
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                warnings.push(ScanWarning::new(None, err.to_string()));
                continue;
            }
        };

        entries += 1;
        if entries == 128 {
            control.advance(entry.path(), entries, 0);
            entries = 0;
        }

        if !is_cargo_manifest(&entry) {
            continue;
        }

        let manifest_path = entry.path().to_path_buf();
        match is_rust_manifest(&manifest_path) {
            Ok(true) => {
                if let Some(root) = manifest_path.parent() {
                    let mut progress = control.progress.lock().unwrap();
                    progress.found = candidates.len() + 1;
                    progress.current_path = root.to_path_buf();
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

    control.advance(scan_root, entries, 0);

    Ok((candidates, warnings))
}

fn is_cargo_manifest(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .map(|file_type| file_type.is_file())
        .unwrap_or(false)
        && entry.file_name() == "Cargo.toml"
}

pub(crate) fn is_rust_manifest(path: &Path) -> Result<bool> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value.get("package").is_some_and(toml::Value::is_table)
        || value.get("workspace").is_some_and(toml::Value::is_table))
}

fn compute_stats(
    root: &Path,
    project_roots: &Arc<HashSet<PathBuf>>,
    control: &ScanControl,
) -> ProjectStats {
    let mut stats = ProjectStats::default();
    let target_root = root.join("target");
    control.advance(root, 0, 0);
    let project_roots = project_roots.clone();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            !is_stats_skip_dir(entry)
                && (entry.depth() == 0 || !project_roots.contains(entry.path()))
        })
        .build();

    let mut entries = 0;
    let mut bytes = 0_u64;
    for result in walker {
        if control.is_cancelled() {
            break;
        }
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                stats.warnings.push(ScanWarning::new(None, err.to_string()));
                continue;
            }
        };

        entries += 1;
        if entries == 128 {
            control.advance(entry.path(), entries, bytes);
            entries = 0;
            bytes = 0;
        }

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
            bytes = bytes.saturating_add(metadata.len());
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

    control.advance(root, entries, bytes);

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
            Some(
                "target"
                    | "node_modules"
                    | ".git"
                    | ".hg"
                    | ".svn"
                    | ".jj"
                    | ".bzr"
                    | "_darcs"
                    | ".fossil"
            )
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
        && matches!(
            entry.file_name().to_str(),
            Some(".git" | ".hg" | ".svn" | ".jj" | ".bzr" | "_darcs" | ".fossil")
        )
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

    #[test]
    fn nested_projects_are_measured_once_and_keep_separate_targets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let child = root.join("crates/member");
        let nested = child.join("nested");
        let manifest = "[workspace]\n";
        for (path, size) in [(&root, 11), (&child, 23), (&nested, 37)] {
            fs::create_dir_all(path.join("target/debug")).unwrap();
            fs::write(path.join("Cargo.toml"), manifest).unwrap();
            fs::write(path.join("target/debug/cache"), vec![0; size]).unwrap();
        }
        let control = ScanControl::default();
        let report = discover_projects_with_progress(temp.path(), SortKey::Size, &control).unwrap();
        let get = |path: &Path| {
            report
                .projects
                .iter()
                .find(|project| project.root == path)
                .unwrap()
        };
        assert_eq!(get(&root).target_size, 11);
        assert_eq!(get(&child).target_size, 23);
        assert_eq!(get(&nested).target_size, 37);
        assert_eq!(get(&root).total_size, 71 + 3 * manifest.len() as u64);
        assert_eq!(get(&child).total_size, 60 + 2 * manifest.len() as u64);
        assert!(get(&root).last_modified >= get(&nested).last_modified);
        let progress = control.snapshot();
        assert_eq!(progress.completed, 3);
        assert_eq!(progress.found, 3);
        assert_eq!(progress.bytes, get(&root).total_size);
        assert_eq!(progress.phase, ScanPhase::Measuring);
    }

    #[test]
    fn cancelled_scan_does_not_publish_a_partial_report() {
        let temp = tempfile::tempdir().unwrap();
        let control = ScanControl::default();
        control.cancel();
        let error =
            discover_projects_with_progress(temp.path(), SortKey::Size, &control).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
