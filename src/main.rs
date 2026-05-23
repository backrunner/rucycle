mod cleaner;
mod format;
mod scan;
mod ui;

use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    process,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};

use cleaner::{clean_projects, CleanPlan};
use format::{format_bytes, format_system_time};
use scan::{discover_projects, SortKey};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Directory to scan. Defaults to the current directory.
    #[arg(value_name = "PATH", default_value = ".")]
    path: PathBuf,

    /// Initial sort order for the project list.
    #[arg(long, value_enum, default_value_t = SortArg::Size)]
    sort: SortArg,

    /// Print the discovered projects and exit without opening the TUI.
    #[arg(long)]
    dry_run: bool,

    /// Skip the TUI and run cargo clean for every discovered project.
    #[arg(short = 'y', long)]
    yes: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SortArg {
    Size,
    Modified,
}

impl From<SortArg> for SortKey {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Size => SortKey::Size,
            SortArg::Modified => SortKey::Modified,
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("rucycle: {err:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let sort = SortKey::from(cli.sort);

    let scan_root = scan::resolve_scan_root(&cli.path)
        .with_context(|| format!("could not use '{}' as a scan path", cli.path.display()))?;

    eprintln!("Scanning {} ...", scan_root.display());
    let report = discover_projects(&scan_root, sort)?;

    if report.projects.is_empty() {
        println!("No Rust projects found under {}.", scan_root.display());
        if !report.warnings.is_empty() {
            print_warnings(&report.warnings);
        }
        return Ok(());
    }

    if cli.dry_run {
        print_project_table(&report.projects, &scan_root);
        if !report.warnings.is_empty() {
            print_warnings(&report.warnings);
        }
        return Ok(());
    }

    if cli.yes {
        let plan = CleanPlan::all(report.projects.clone());
        let summary = clean_projects(plan, |event| {
            println!("{event}");
        })?;
        println!(
            "Done. cleaned: {}, failed: {}, skipped: {}",
            summary.cleaned, summary.failed, summary.skipped
        );
        return if summary.failed > 0 {
            bail!("one or more cargo clean commands failed")
        } else {
            Ok(())
        };
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "interactive TUI requires a terminal; use --dry-run to inspect or --yes to clean all"
        );
    }

    ui::run(report, sort)
}

fn print_project_table(projects: &[scan::ProjectInfo], scan_root: &std::path::Path) {
    println!(
        "{:<10} {:<10} {:<17} PROJECT",
        "TARGET", "TOTAL", "MODIFIED"
    );
    for project in projects {
        let rel = project.display_path(scan_root);
        println!(
            "{:<10} {:<10} {:<17} {}",
            format_bytes(project.target_size),
            format_bytes(project.total_size),
            format_system_time(project.last_modified),
            rel.display()
        );
    }
}

fn print_warnings(warnings: &[scan::ScanWarning]) {
    eprintln!("Warnings:");
    for warning in warnings.iter().take(8) {
        match &warning.path {
            Some(path) => eprintln!("  {}: {}", path.display(), warning.message),
            None => eprintln!("  {}", warning.message),
        }
    }
    if warnings.len() > 8 {
        eprintln!("  ... and {} more", warnings.len() - 8);
    }
}
