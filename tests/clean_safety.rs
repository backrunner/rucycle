//! Destructive regression tests operate exclusively on disposable fixtures.
//! Run the real CLI and real Cargo, then verify source and unrelated data bytes.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";

struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    project: PathBuf,
    cargo_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("demo");
        let cargo_home = base.join("cargo-home");
        fs::create_dir_all(&cargo_home).unwrap();
        write_project(&project);
        write_cache(&project.join("target"));
        fs::write(base.join("unrelated.txt"), "unrelated data").unwrap();
        Self {
            _temp: temp,
            base,
            project,
            cargo_home,
        }
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rucycle"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("CARGO_") {
                command.env_remove(key);
            }
        }
        command
            .arg(&self.project)
            .env("CARGO_HOME", &self.cargo_home)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .current_dir(&self.base);
        command
    }

    fn assert_sources(&self) {
        assert_eq!(fs::read(self.project.join("Cargo.toml")).unwrap(), MANIFEST);
        assert_eq!(fs::read(self.project.join("src/main.rs")).unwrap(), SOURCE);
        assert_eq!(
            fs::read(self.base.join("unrelated.txt")).unwrap(),
            b"unrelated data"
        );
    }
}

const MANIFEST: &[u8] = b"[package]\nname = 'demo'\nversion = '0.1.0'\nedition = '2021'\n";
const SOURCE: &[u8] = b"fn main() {}\n";

fn write_project(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("Cargo.toml"), MANIFEST).unwrap();
    fs::write(path.join("src/main.rs"), SOURCE).unwrap();
}

fn write_cache(path: &Path) {
    fs::create_dir_all(path.join("debug")).unwrap();
    fs::write(path.join("CACHEDIR.TAG"), TAG).unwrap();
    fs::write(path.join("debug/artifact"), "disposable artifact").unwrap();
}

fn diagnostics(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_cleaned(output: Output) {
    assert!(output.status.success(), "{}", diagnostics(&output));
    assert!(diagnostics(&output).contains("cleaned: 1, failed: 0, skipped: 0"));
}

fn assert_skipped(output: Output, reason: &str) {
    assert!(!output.status.success(), "{}", diagnostics(&output));
    let text = diagnostics(&output);
    assert!(text.contains("cleaned: 0, failed: 0, skipped: 1"), "{text}");
    assert!(text.contains(reason), "{text}");
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    files
}

#[test]
fn hostile_output_environment_cannot_delete_source_or_external_data() {
    let fixture = Fixture::new();
    let external = fixture.base.join("external-cache");
    write_cache(&external);
    let before = snapshot(&external);
    assert_cleaned(
        fixture
            .cli()
            .arg("--yes")
            .env("CARGO_TARGET_DIR", &fixture.project)
            .env("CARGO_BUILD_TARGET_DIR", &external)
            .env("CARGO_BUILD_BUILD_DIR", fixture.project.join("src"))
            .output()
            .unwrap(),
    );
    fixture.assert_sources();
    assert!(!fixture.project.join("target").exists());
    assert_eq!(snapshot(&external), before);
    assert!(!fixture.project.join("Cargo.lock").exists());
}

#[test]
fn real_cargo_build_artifacts_including_internal_hardlinks_can_be_cleaned() {
    let fixture = Fixture::new();
    let mut cargo = Command::new(env!("CARGO"));
    for (key, value) in fixture.cli().get_envs() {
        match value {
            Some(value) => {
                cargo.env(key, value);
            }
            None => {
                cargo.env_remove(key);
            }
        }
    }
    let output = cargo
        .args(["build", "--offline", "--target-dir"])
        .arg(fixture.project.join("target"))
        .env("CARGO_INCREMENTAL", "1")
        .current_dir(&fixture.project)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostics(&output));
    let executable = if cfg!(windows) { "demo.exe" } else { "demo" };
    assert!(fixture
        .project
        .join("target/debug")
        .join(executable)
        .is_file());
    let lockfile = fs::read(fixture.project.join("Cargo.lock")).unwrap();
    assert_cleaned(fixture.cli().arg("--yes").output().unwrap());
    assert!(!fixture.project.join("target").exists());
    fixture.assert_sources();
    assert_eq!(
        fs::read(fixture.project.join("Cargo.lock")).unwrap(),
        lockfile
    );
}

#[test]
fn project_ancestor_and_global_output_config_cannot_redirect_deletion() {
    for layer in ["project", "ancestor", "global", "legacy"] {
        let fixture = Fixture::new();
        let directory = match layer {
            "project" | "legacy" => fixture.project.join(".cargo"),
            "ancestor" => fixture.base.join(".cargo"),
            _ => fixture.cargo_home.clone(),
        };
        fs::create_dir_all(&directory).unwrap();
        let source = toml::Value::String(fixture.project.to_str().unwrap().to_string());
        let config = format!("[build]\ntarget-dir = {source}\nbuild-dir = {source}\n");
        let config_path = directory.join(if layer == "legacy" {
            "config"
        } else {
            "config.toml"
        });
        fs::write(&config_path, &config).unwrap();
        assert_cleaned(fixture.cli().arg("--yes").output().unwrap());
        fixture.assert_sources();
        assert_eq!(fs::read_to_string(config_path).unwrap(), config);
        assert!(!fixture.project.join("target").exists());
    }
}

#[test]
fn selecting_workspace_member_leaves_workspace_and_sibling_outputs_alone() {
    let fixture = Fixture::new();
    let workspace = fixture.base.join("Cargo.toml");
    fs::write(
        &workspace,
        "[workspace]\nmembers = ['demo']\nresolver = '2'\n",
    )
    .unwrap();
    write_cache(&fixture.base.join("target"));
    write_project(&fixture.base.join("sibling"));
    write_cache(&fixture.base.join("sibling/target"));
    let workspace_cache = snapshot(&fixture.base.join("target"));
    let sibling = snapshot(&fixture.base.join("sibling"));
    assert_cleaned(fixture.cli().arg("--yes").output().unwrap());
    fixture.assert_sources();
    assert!(!fixture.project.join("target").exists());
    assert_eq!(snapshot(&fixture.base.join("target")), workspace_cache);
    assert_eq!(snapshot(&fixture.base.join("sibling")), sibling);
    assert!(workspace.exists());
}

#[test]
fn no_local_target_is_a_noop_even_with_dangerous_environment() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.project.join("target")).unwrap();
    let before = snapshot(&fixture.base);
    assert_cleaned(
        fixture
            .cli()
            .arg("--yes")
            .env("CARGO_TARGET_DIR", &fixture.project)
            .env("CARGO_BUILD_BUILD_DIR", &fixture.base)
            .output()
            .unwrap(),
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[test]
fn dry_run_wins_over_yes_and_changes_nothing() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.base);
    let output = fixture
        .cli()
        .args(["--yes", "--dry-run"])
        .env("CARGO_TARGET_DIR", &fixture.project)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostics(&output));
    assert_eq!(snapshot(&fixture.base), before);
}

#[test]
fn missing_or_invalid_cache_marker_is_not_deleted() {
    for marker in [None, Some("not a Cargo cache")] {
        let fixture = Fixture::new();
        let tag = fixture.project.join("target/CACHEDIR.TAG");
        match marker {
            Some(contents) => fs::write(tag, contents).unwrap(),
            None => fs::remove_file(tag).unwrap(),
        }
        let before = snapshot(&fixture.base);
        assert_skipped(fixture.cli().arg("--yes").output().unwrap(), "CACHEDIR.TAG");
        assert_eq!(snapshot(&fixture.base), before);
    }
}

#[test]
fn nested_project_or_repository_is_not_deleted() {
    for name in ["Cargo.toml", "cArGo.ToMl", ".git", ".hg", ".svn"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.project.join("target/debug").join(name),
            "precious data",
        )
        .unwrap();
        let before = snapshot(&fixture.base);
        assert_skipped(
            fixture.cli().arg("--yes").output().unwrap(),
            "project or repository",
        );
        assert_eq!(snapshot(&fixture.base), before);
    }
}

#[test]
fn cargo_template_characters_in_path_are_rejected() {
    let mut fixture = Fixture::new();
    let renamed = fixture.base.join("demo-{workspace-root}");
    fs::rename(&fixture.project, &renamed).unwrap();
    fixture.project = renamed;
    let before = snapshot(&fixture.base);
    assert_skipped(
        fixture.cli().arg("--yes").output().unwrap(),
        "template delimiters",
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[test]
fn space_and_unicode_paths_are_cleaned_without_broadening_scope() {
    let mut fixture = Fixture::new();
    let renamed = fixture.base.join("项目 with spaces");
    fs::rename(&fixture.project, &renamed).unwrap();
    fixture.project = renamed;
    assert_cleaned(fixture.cli().arg("--yes").output().unwrap());
    fixture.assert_sources();
    assert!(!fixture.project.join("target").exists());
}

#[test]
fn corrupt_git_index_fails_closed() {
    let fixture = Fixture::new();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&fixture.project)
        .status()
        .unwrap()
        .success());
    fs::write(fixture.project.join(".git/index"), "corrupt index").unwrap();
    let before = snapshot(&fixture.base);
    assert_skipped(
        fixture.cli().arg("--yes").output().unwrap(),
        "cannot check Git-tracked",
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[cfg(unix)]
#[test]
fn default_source_linked_into_target_is_protected() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.project.join("src"),
        fixture.project.join("target/source"),
    )
    .unwrap();
    std::os::unix::fs::symlink("target/source", fixture.project.join("src")).unwrap();
    let before = snapshot(&fixture.base);
    assert_skipped(
        fixture.cli().arg("--yes").output().unwrap(),
        "project input",
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[cfg(unix)]
#[test]
fn nested_source_link_into_target_is_protected() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.project.join("src/nested")).unwrap();
    fs::write(
        fixture.project.join("target/module.rs"),
        "pub fn precious() {}\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../../target/module.rs",
        fixture.project.join("src/nested/module.rs"),
    )
    .unwrap();
    let before = snapshot(&fixture.base);
    assert_skipped(
        fixture.cli().arg("--yes").output().unwrap(),
        "project input link",
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[test]
fn workspace_member_source_in_root_target_is_protected() {
    let fixture = Fixture::new();
    fs::write(
        fixture.project.join("Cargo.toml"),
        "[workspace]\nmembers = ['member']\nresolver = '2'\n",
    )
    .unwrap();
    let member = fixture.project.join("member");
    write_project(&member);
    fs::write(
        member.join("Cargo.toml"),
        format!(
            "{}\n[lib]\npath = '../target/shared.rs'\n",
            String::from_utf8_lossy(MANIFEST)
        ),
    )
    .unwrap();
    fs::write(
        fixture.project.join("target/shared.rs"),
        "pub fn precious() {}\n",
    )
    .unwrap();
    let before = snapshot(&fixture.base);
    let output = fixture.cli().arg("--yes").output().unwrap();
    assert!(!output.status.success(), "{}", diagnostics(&output));
    assert!(
        diagnostics(&output).contains("declared project input"),
        "{}",
        diagnostics(&output)
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[cfg(windows)]
#[test]
fn windows_junction_target_is_not_followed() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.project.join("target")).unwrap();
    let destination = fixture.base.join("precious");
    write_project(&destination);
    let before = snapshot(&destination);
    let output = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(fixture.project.join("target"))
        .arg(&destination)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", diagnostics(&output));
    assert_skipped(
        fixture.cli().arg("--yes").output().unwrap(),
        "links/reparse",
    );
    assert_eq!(snapshot(&destination), before);
    fixture.assert_sources();
}

#[cfg(unix)]
#[test]
fn active_cargo_process_prevents_cleanup() {
    struct RunningProcess(std::process::Child);
    impl Drop for RunningProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    // An actual native process named cargo exercises the OS process inspection.
    let executable = fixture.base.join("cargo");
    let source = fixture.base.join("busy.rs");
    let ready = fixture.base.join("ready");
    fs::write(
        &source,
        r#"fn main() {
        std::fs::write(std::env::args().nth(1).unwrap(), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }"#,
    )
    .unwrap();
    assert!(Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .current_dir(&fixture.base)
        .status()
        .unwrap()
        .success());
    let mut process = RunningProcess(
        Command::new(executable)
            .arg(&ready)
            .current_dir(&fixture.project)
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "fixture exited before becoming ready"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "fixture did not become ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let before = snapshot(&fixture.base);
    let output = fixture.cli().arg("--yes").output().unwrap();
    assert!(process.0.try_wait().unwrap().is_none());
    assert_skipped(output, "cargo is already running");
    assert_eq!(snapshot(&fixture.base), before);
}

#[test]
fn declared_source_in_target_is_not_deleted() {
    for entry in [
        "[lib]\npath = 'target/code.rs'",
        "[[bin]]\nname = 'other'\npath = 'target/code.rs'",
        "[package]\nbuild = 'target/code.rs'",
    ] {
        let fixture = Fixture::new();
        let manifest = if entry.starts_with("[package]") {
            format!(
                "{}build = 'target/code.rs'\n",
                String::from_utf8_lossy(MANIFEST)
            )
        } else {
            format!("{}\n{entry}\n", String::from_utf8_lossy(MANIFEST))
        };
        fs::write(fixture.project.join("Cargo.toml"), manifest).unwrap();
        fs::write(fixture.project.join("target/code.rs"), "precious source").unwrap();
        let before = snapshot(&fixture.base);
        assert_skipped(
            fixture.cli().arg("--yes").output().unwrap(),
            "declared project input",
        );
        assert_eq!(snapshot(&fixture.base), before);
    }
}

#[test]
fn git_tracked_target_data_is_not_deleted_even_with_redirected_git_environment() {
    let fixture = Fixture::new();
    for args in [
        vec!["init", "-q"],
        vec!["add", "demo/target/debug/artifact"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&fixture.base)
            .status()
            .unwrap()
            .success());
    }
    let before = snapshot(&fixture.base);
    assert_skipped(
        fixture
            .cli()
            .arg("--yes")
            .env("GIT_DIR", fixture.base.join("nonexistent.git"))
            .env("GIT_INDEX_FILE", fixture.base.join("nonexistent-index"))
            .output()
            .unwrap(),
        "Git-tracked",
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[cfg(unix)]
#[test]
fn target_or_nested_symlinks_cannot_delete_their_destination() {
    use std::os::unix::fs::symlink;
    for nested in [false, true] {
        let fixture = Fixture::new();
        let destination = fixture.base.join("precious");
        write_project(&destination);
        let before = snapshot(&destination);
        let link = if nested {
            fixture.project.join("target/debug/link")
        } else {
            fs::remove_dir_all(fixture.project.join("target")).unwrap();
            fixture.project.join("target")
        };
        symlink(&destination, &link).unwrap();
        assert_skipped(
            fixture.cli().arg("--yes").output().unwrap(),
            "links/reparse",
        );
        assert_eq!(snapshot(&destination), before);
        fixture.assert_sources();
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }
}

#[cfg(any(unix, windows))]
#[test]
fn hardlinked_cache_file_is_not_overwritten_or_deleted() {
    let fixture = Fixture::new();
    fs::hard_link(
        fixture.base.join("unrelated.txt"),
        fixture.project.join("target/.rustc_info.json"),
    )
    .unwrap();
    let before = snapshot(&fixture.base);
    assert_skipped(fixture.cli().arg("--yes").output().unwrap(), "hard-linked");
    assert_eq!(snapshot(&fixture.base), before);
}
