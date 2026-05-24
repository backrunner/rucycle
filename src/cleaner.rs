use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use sysinfo::{Process, System};

use crate::{format::format_bytes, scan::ProjectInfo};

const BUSY_CARGO_MESSAGE: &str = "cargo is already running in this project tree";

#[derive(Clone, Debug)]
pub struct CleanPlan {
    projects: Vec<ProjectInfo>,
}

impl CleanPlan {
    pub fn all(projects: Vec<ProjectInfo>) -> Self {
        Self { projects }
    }

    pub fn selected(projects: &[ProjectInfo], selected: &[bool]) -> Self {
        let projects = projects
            .iter()
            .zip(selected.iter())
            .filter(|(_, selected)| **selected)
            .map(|(project, _)| project.clone())
            .collect();
        Self { projects }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CleanSummary {
    pub cleaned: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug)]
pub enum CleanEvent {
    Started {
        index: usize,
        total: usize,
        project: ProjectInfo,
    },
    Cleaned {
        project: ProjectInfo,
        reclaimed: u64,
    },
    Skipped {
        project: ProjectInfo,
        message: String,
    },
    Failed {
        project: ProjectInfo,
        message: String,
    },
}

impl fmt::Display for CleanEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CleanEvent::Started {
                index,
                total,
                project,
            } => write!(
                formatter,
                "[{index}/{total}] cleaning {} ({})",
                project.root.display(),
                format_bytes(project.target_size)
            ),
            CleanEvent::Cleaned { project, reclaimed } => write!(
                formatter,
                "cleaned {} (about {} reclaimable)",
                project.root.display(),
                format_bytes(*reclaimed)
            ),
            CleanEvent::Skipped { project, message } => {
                write!(formatter, "skipped {}: {message}", project.root.display())
            }
            CleanEvent::Failed { project, message } => {
                write!(formatter, "failed {}: {message}", project.root.display())
            }
        }
    }
}

pub fn clean_projects(
    plan: CleanPlan,
    mut on_event: impl FnMut(CleanEvent),
) -> Result<CleanSummary> {
    let mut summary = CleanSummary::default();
    let total = plan.projects.len();

    for (position, project) in plan.projects.into_iter().enumerate() {
        if project_has_active_cargo(&project) {
            summary.skipped += 1;
            on_event(CleanEvent::Skipped {
                project,
                message: BUSY_CARGO_MESSAGE.to_string(),
            });
            continue;
        }

        on_event(CleanEvent::Started {
            index: position + 1,
            total,
            project: project.clone(),
        });

        let output = Command::new("cargo")
            .arg("clean")
            .arg("--manifest-path")
            .arg(&project.manifest_path)
            .current_dir(&project.root)
            .output();

        match output {
            Ok(output) if output.status.success() => {
                summary.cleaned += 1;
                on_event(CleanEvent::Cleaned {
                    reclaimed: project.target_size,
                    project,
                });
            }
            Ok(output) => {
                summary.failed += 1;
                let message = command_failure_message(&output.stderr, &output.stdout);
                on_event(CleanEvent::Failed { project, message });
            }
            Err(err) => {
                summary.failed += 1;
                on_event(CleanEvent::Failed {
                    project,
                    message: err.to_string(),
                });
            }
        }
    }

    Ok(summary)
}

fn project_has_active_cargo(project: &ProjectInfo) -> bool {
    active_cargo_scope_roots()
        .iter()
        .any(|scope_root| paths_share_project_tree(scope_root, &project.root))
}

fn active_cargo_scope_roots() -> Vec<PathBuf> {
    let system = System::new_all();
    system
        .processes()
        .values()
        .filter(|process| is_cargo_process(process))
        .filter_map(cargo_scope_root)
        .collect()
}

fn is_cargo_process(process: &Process) -> bool {
    process_name_is_cargo(process.name())
        || process
            .exe()
            .and_then(|path| path.file_name())
            .is_some_and(process_name_is_cargo)
}

fn process_name_is_cargo(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.eq_ignore_ascii_case("cargo") || name.eq_ignore_ascii_case("cargo.exe")
}

fn cargo_scope_root(process: &Process) -> Option<PathBuf> {
    let cwd = process.cwd().map(canonicalize_or_original);
    manifest_scope_root(process.cmd(), cwd.as_deref()).or(cwd)
}

fn manifest_scope_root(command: &[OsString], cwd: Option<&Path>) -> Option<PathBuf> {
    let mut args = command.iter().skip(1);

    while let Some(arg) = args.next() {
        if arg == OsStr::new("--manifest-path") {
            return args
                .next()
                .and_then(|manifest_path| manifest_directory(manifest_path, cwd));
        }

        let arg = arg.to_string_lossy();
        if let Some(manifest_path) = arg.strip_prefix("--manifest-path=") {
            return manifest_directory(OsStr::new(manifest_path), cwd);
        }
    }

    None
}

fn manifest_directory(manifest_path: &OsStr, cwd: Option<&Path>) -> Option<PathBuf> {
    let manifest_path = Path::new(manifest_path);
    let resolved = if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        cwd?.join(manifest_path)
    };
    let resolved = canonicalize_or_original(&resolved);

    if resolved.is_dir() {
        Some(resolved)
    } else {
        resolved.parent().map(canonicalize_or_original)
    }
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn paths_share_project_tree(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn command_failure_message(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr.is_empty() {
        return compact_message(&stderr);
    }

    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout.is_empty() {
        return compact_message(&stdout);
    }

    "cargo clean exited with a non-zero status".to_string()
}

fn compact_message(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or(message).trim();
    const MAX: usize = 180;
    if first_line.chars().count() <= MAX {
        first_line.to_string()
    } else {
        let mut shortened: String = first_line.chars().take(MAX - 1).collect();
        shortened.push('.');
        shortened
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn manifest_scope_root_resolves_separate_argument() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();

        let command = os_args(&["cargo", "build", "--manifest-path", "demo/Cargo.toml"]);
        let scope = manifest_scope_root(&command, Some(temp.path())).unwrap();

        assert_eq!(scope, fs::canonicalize(project).unwrap());
    }

    #[test]
    fn manifest_scope_root_resolves_equals_argument() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("demo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();

        let command = os_args(&["cargo", "test", "--manifest-path=demo/Cargo.toml"]);
        let scope = manifest_scope_root(&command, Some(temp.path())).unwrap();

        assert_eq!(scope, fs::canonicalize(project).unwrap());
    }

    #[test]
    fn project_tree_match_blocks_ancestors_and_descendants_only() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let member = workspace.join("member");
        let sibling = temp.path().join("sibling");
        fs::create_dir_all(&member).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        let workspace = fs::canonicalize(workspace).unwrap();
        let member = fs::canonicalize(member).unwrap();
        let sibling = fs::canonicalize(sibling).unwrap();

        assert!(paths_share_project_tree(&workspace, &member));
        assert!(paths_share_project_tree(&member, &workspace));
        assert!(!paths_share_project_tree(&member, &sibling));
    }
}
