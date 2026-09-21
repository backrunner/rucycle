//! Conservative, fail-closed checks for the one directory shown in the UI.
//! These checks are not a filesystem sandbox: callers must still pin Cargo's
//! output paths, and concurrent filesystem changes remain outside the guarantee.
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{self, Metadata},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, ensure, Context, Result};

use crate::scan::{is_rust_manifest, ProjectInfo};

const CACHE_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

pub(crate) fn validated_target(project: &ProjectInfo) -> Result<Option<PathBuf>> {
    let root = &project.root;
    ensure!(root.is_absolute(), "project path is not absolute");
    ensure!(
        fs::canonicalize(root).context("cannot resolve project path")? == *root,
        "project path changed or contains a link; scan again"
    );
    let root_metadata = plain_metadata(root)?;
    ensure!(root_metadata.is_dir(), "project is not a directory");
    ensure!(
        project.manifest_path == root.join("Cargo.toml"),
        "manifest does not belong to the selected project"
    );
    ensure!(
        plain_metadata(&project.manifest_path)?.is_file()
            && is_rust_manifest(&project.manifest_path)?,
        "project no longer has a valid Cargo manifest"
    );

    let target = root.join("target");
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("cannot inspect local target directory"),
    };
    check_plain(&target, &metadata)?;
    ensure!(metadata.is_dir(), "local target is not a directory");
    ensure!(
        fs::canonicalize(&target)? == target,
        "target resolves outside the selected directory"
    );
    let target_text = target.to_str().context("target path is not UTF-8")?;
    ensure!(
        !target_text.contains(['{', '}']),
        "target path contains Cargo build-directory template delimiters"
    );

    // A directory named `target` alone is not evidence of a disposable cache.
    let tag = target.join("CACHEDIR.TAG");
    ensure!(plain_metadata(&tag)?.is_file(), "cache tag is not a file");
    let mut signature = [0; CACHE_SIGNATURE.len()];
    fs::File::open(&tag)
        .and_then(|mut file| file.read_exact(&mut signature))
        .context("target is missing a valid Cargo CACHEDIR.TAG")?;
    ensure!(
        signature == *CACHE_SIGNATURE,
        "target is missing a valid Cargo CACHEDIR.TAG"
    );

    validate_tree(&target, &root_metadata)?;
    protect_project_inputs(root, &target)?;
    protect_git_files(root, &target)?;
    Ok(Some(target))
}

fn plain_metadata(path: &Path) -> Result<Metadata> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    check_plain(path, &metadata)?;
    Ok(metadata)
}

fn check_plain(path: &Path, metadata: &Metadata) -> Result<()> {
    ensure!(
        !is_link(metadata),
        "links/reparse points are not safe to clean: {}",
        path.display()
    );
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "special file is not safe to clean: {}",
        path.display()
    );
    Ok(())
}

struct FileLinks {
    identity: (u64, u64),
    count: u64,
}

fn file_links(path: &Path, metadata: &Metadata) -> Result<FileLinks> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = metadata;
        // FILE_FLAG_OPEN_REPARSE_POINT: inspect the entry itself if it changed.
        let file = fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(0x00200000)
            .open(path)
            .with_context(|| format!("cannot inspect file links: {}", path.display()))?;
        let information = winapi_util::file::information(&file)?;
        ensure!(
            information.file_attributes() & 0x400 == 0,
            "linked file is not safe to clean: {}",
            path.display()
        );
        Ok(FileLinks {
            identity: (information.volume_serial_number(), information.file_index()),
            count: information.number_of_links(),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        Ok(FileLinks {
            identity: (metadata.dev(), metadata.ino()),
            count: metadata.nlink(),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, metadata);
        bail!("file-link safety checks are unsupported on this platform")
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other reparse points, not only symlinks.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_tree(target: &Path, root_metadata: &Metadata) -> Result<()> {
    let mut pending = vec![target.to_path_buf()];
    let mut hardlinks: HashMap<(u64, u64), (u64, u64, PathBuf)> = HashMap::new();
    while let Some(path) = pending.pop() {
        let metadata = plain_metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.dev() == root_metadata.dev(),
                "target crosses a filesystem boundary: {}",
                path.display()
            );
        }
        #[cfg(not(unix))]
        let _ = root_metadata;

        let name = path.file_name().unwrap_or_default().to_string_lossy();
        ensure!(
            !["Cargo.toml", ".git", ".hg", ".svn"]
                .iter()
                .any(|marker| name.eq_ignore_ascii_case(marker)),
            "target contains a project or repository: {}",
            path.display()
        );
        if metadata.is_dir() {
            for entry in
                fs::read_dir(&path).with_context(|| format!("cannot read {}", path.display()))?
            {
                pending.push(entry?.path());
            }
        } else {
            let links = file_links(&path, &metadata)?;
            ensure!(
                links.count > 0,
                "cannot determine file links: {}",
                path.display()
            );
            if links.count > 1 {
                let (expected, found, _) = hardlinks
                    .entry(links.identity)
                    .or_insert_with(|| (links.count, 0, path.clone()));
                ensure!(
                    *expected == links.count,
                    "target links changed during validation"
                );
                *found += 1;
            }
        }
    }
    // Cargo uses hard links between incremental/deps artifacts. Allow those
    // only when every link to an inode was observed inside this target tree.
    for (expected, found, path) in hardlinks.into_values() {
        ensure!(
            expected == found,
            "hard-linked file has links outside target: {}",
            path.display()
        );
    }
    Ok(())
}

fn protect_project_inputs(root: &Path, target: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("cannot inspect project inputs in {}", directory.display()))?
        {
            let path = entry?.path();
            if path == target {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if is_link(&metadata) {
                match fs::canonicalize(&path) {
                    Ok(resolved) => ensure!(
                        !resolved.starts_with(target),
                        "target is referenced by a project input link: {}",
                        path.display()
                    ),
                    Err(err) if err.kind() == ErrorKind::NotFound => {}
                    Err(err) => return Err(err).context("cannot resolve project input link"),
                }
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if metadata.is_dir() {
                // Match discovery's exclusions. Inspect links before excluding
                // generated/dependency directories, so a linked source is seen.
                if !["target", "node_modules", ".git", ".hg", ".svn"]
                    .iter()
                    .any(|skip| name.eq_ignore_ascii_case(skip))
                {
                    pending.push(path);
                }
            } else if name.eq_ignore_ascii_case("Cargo.toml") {
                // A workspace member can declare ../target/source.rs even
                // when only its workspace root was selected for cleanup.
                ensure!(metadata.is_file(), "manifest is not a regular file");
                protect_manifest_inputs(&path, target)?;
            }
        }
    }
    Ok(())
}

fn protect_manifest_inputs(manifest_path: &Path, target: &Path) -> Result<()> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(manifest_path)?)?;
    let root = manifest_path.parent().context("manifest has no parent")?;
    let mut inputs = vec![
        "src",
        "src/main.rs",
        "src/lib.rs",
        "src/bin",
        "examples",
        "tests",
        "benches",
        "build.rs",
    ];
    if let Some(package) = manifest.get("package") {
        for key in ["build", "readme", "license-file"] {
            if let Some(path) = package.get(key).and_then(toml::Value::as_str) {
                inputs.push(path);
            }
        }
    }
    if let Some(path) = manifest
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str)
    {
        inputs.push(path);
    }
    for kind in ["bin", "example", "test", "bench"] {
        if let Some(entries) = manifest.get(kind).and_then(toml::Value::as_array) {
            inputs.extend(
                entries
                    .iter()
                    .filter_map(|entry| entry.get("path").and_then(toml::Value::as_str)),
            );
        }
    }
    for input in inputs {
        let path = root.join(input);
        match fs::canonicalize(&path) {
            Ok(resolved) if resolved.starts_with(target) => {
                bail!(
                    "target contains a declared project input: {}",
                    path.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("cannot resolve declared project input"),
        }
    }
    Ok(())
}

fn protect_git_files(root: &Path, target: &Path) -> Result<()> {
    // Check every enclosing repository, including an outer repository tracking
    // files inside a nested one. Never let inherited GIT_DIR/GIT_INDEX_FILE etc.
    // redirect this check away from the actual project.
    for ancestor in root.ancestors() {
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err).context("cannot inspect enclosing repository"),
        }
        let mut pathspec = OsString::from(":(top,literal)");
        let relative = target.strip_prefix(ancestor)?;
        #[cfg(windows)]
        pathspec.push(relative.to_string_lossy().replace('\\', "/"));
        #[cfg(not(windows))]
        pathspec.push(relative);
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        let output = command
            .args([
                "-c",
                "core.fsmonitor=false",
                "ls-files",
                "--cached",
                "-z",
                "--",
            ])
            .arg(pathspec)
            .current_dir(ancestor)
            .output()
            .context("cannot check Git-tracked files; refusing cleanup")?;
        ensure!(
            output.status.success(),
            "cannot check Git-tracked files; refusing cleanup"
        );
        ensure!(
            output.stdout.is_empty(),
            "target contains Git-tracked files"
        );
    }
    Ok(())
}
