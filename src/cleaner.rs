use std::{fmt, process::Command};

use anyhow::Result;

use crate::{format::format_bytes, scan::ProjectInfo};

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
