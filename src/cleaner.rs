use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use sysinfo::{Process, ProcessRefreshKind, RefreshKind, System, UpdateKind};

use crate::{format::format_bytes, safety::validated_target, scan::ProjectInfo};

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
        if let Some(message) = project_busy_reason(&project) {
            summary.skipped += 1;
            on_event(CleanEvent::Skipped {
                project,
                message: message.to_string(),
            });
            continue;
        }

        on_event(CleanEvent::Started {
            index: position + 1,
            total,
            project: project.clone(),
        });

        // Validate after the event callback, immediately before invoking Cargo.
        // The scan and the user's selection are not authority to delete an
        // unchecked directory later, nor to follow Cargo's configured paths.
        let target = match validated_target(&project) {
            Ok(Some(target)) => target,
            Ok(None) => {
                summary.cleaned += 1;
                on_event(CleanEvent::Cleaned {
                    project,
                    reclaimed: 0,
                });
                continue;
            }
            Err(err) => {
                summary.skipped += 1;
                on_event(CleanEvent::Skipped {
                    project,
                    message: format!("safety check: {err:#}"),
                });
                continue;
            }
        };
        let output = clean_command(&project, &target).output();

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

fn clean_command(project: &ProjectInfo, target: &Path) -> Command {
    let mut command = Command::new("cargo");
    // validated_target requires UTF-8 and rejects Cargo template delimiters.
    // A TOML string handles spaces, quotes and Windows backslashes safely.
    let build_dir = toml::Value::String(target.to_str().unwrap().to_owned());
    command
        .arg("clean")
        .arg("--manifest-path")
        .arg(&project.manifest_path)
        .arg("--target-dir")
        .arg(target)
        .arg("--config")
        .arg(format!("build.build-dir={build_dir}"))
        .arg("--frozen")
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_TARGET_DIR", target)
        .env("CARGO_BUILD_BUILD_DIR", target)
        .current_dir(&project.root);
    command
}

fn project_busy_reason(project: &ProjectInfo) -> Option<&'static str> {
    busy_reason(&project.root, &active_cargo_scope_roots())
}

fn busy_reason(root: &Path, scopes: &[Option<PathBuf>]) -> Option<&'static str> {
    if scopes
        .iter()
        .flatten()
        .any(|scope| paths_share_project_tree(scope, root))
    {
        Some(BUSY_CARGO_MESSAGE)
    } else if scopes.iter().any(Option::is_none) {
        Some("cannot determine the project of a running cargo process; refusing cleanup")
    } else {
        None
    }
}

fn active_cargo_scope_roots() -> Vec<Option<PathBuf>> {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .with_cwd(UpdateKind::Always),
        ),
    );
    system
        .processes()
        .values()
        .filter(|process| is_cargo_process(process))
        .map(cargo_scope_root)
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

    #[test]
    fn unreadable_cargo_scope_fails_closed() {
        let root = Path::new("/project");
        assert!(busy_reason(root, &[None]).is_some());
        assert!(busy_reason(root, &[Some(PathBuf::from("/other")), None]).is_some());
        assert!(busy_reason(root, &[Some(PathBuf::from("/other"))]).is_none());
        assert_eq!(
            busy_reason(root, &[Some(root.join("member"))]),
            Some(BUSY_CARGO_MESSAGE)
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_target_with_a_link_after_scan_is_rejected() {
        use crate::scan::{discover_projects, SortKey};
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(
            root.join("target/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let projects = discover_projects(&root, SortKey::Size).unwrap().projects;
        let precious = root.join("precious");
        fs::create_dir(&precious).unwrap();
        fs::write(precious.join("source.rs"), "precious source").unwrap();
        let mut skipped = false;
        let summary = clean_projects(CleanPlan::all(projects), |event| match event {
            CleanEvent::Started { .. } => {
                fs::rename(root.join("target"), root.join("old-target")).unwrap();
                symlink(&precious, root.join("target")).unwrap();
            }
            CleanEvent::Skipped { message, .. } => {
                assert!(message.contains("links/reparse"));
                skipped = true;
            }
            _ => panic!("unsafe clean must not run"),
        })
        .unwrap();
        assert!(skipped);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            fs::read_to_string(precious.join("source.rs")).unwrap(),
            "precious source"
        );
        assert!(root.join("Cargo.toml").exists());
    }
}
