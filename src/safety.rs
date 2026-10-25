//! Conservative, fail-closed checks for the one directory shown in the UI.
//! These checks are not a filesystem sandbox: callers must still pin Cargo's
//! output paths, and concurrent filesystem changes remain outside the guarantee.
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{self, Metadata},
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, ensure, Context, Result};

use crate::scan::{is_rust_manifest, ProjectInfo};

const CACHE_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";
const GIT_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const PIN_ATTEMPTS: usize = 8;

/// Directory and file names that mark a repository or foreign project living
/// inside what should be a pure build cache.
const REPOSITORY_MARKERS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".bzr",
    "_darcs",
    ".fossil",
    ".fslckout",
    "_fossil_",
    ".pijul",
];

/// Filesystem identity of a directory: device + inode on Unix, volume + file
/// index on Windows. Proves the directory being deleted is the same one that
/// passed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity(u64, u64);

/// A `target/` directory that passed every check. Carries the inode identity
/// captured at validation time so `pin_target` can detect a post-validation
/// swap of the path.
pub(crate) struct ValidatedTarget {
    pub(crate) path: PathBuf,
    identity: FileIdentity,
}

pub(crate) fn validated_target(project: &ProjectInfo) -> Result<Option<ValidatedTarget>> {
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
    // Capture identity before walking the tree, not after it: replacing the
    // directory during validation must not bless the replacement's identity.
    let identity = dir_identity(&target)?;
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
    ensure!(
        dir_identity(&target)? == identity,
        "target changed during validation; refusing cleanup"
    );
    Ok(Some(ValidatedTarget {
        path: target,
        identity,
    }))
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
            !REPOSITORY_MARKERS
                .iter()
                .any(|marker| name.eq_ignore_ascii_case(marker)),
            "target contains a project or repository: {}",
            path.display()
        );
        // `cargo package`/`cargo publish` extract and verify crates under
        // `target/package/`; those copies legitimately contain manifests and
        // nested crate fixtures. Repository metadata is always protected,
        // including inside package output.
        let in_package_output = path
            .strip_prefix(target)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == "package");
        ensure!(
            in_package_output || !name.eq_ignore_ascii_case("Cargo.toml"),
            "target contains a project or repository: {}",
            path.display()
        );
        if metadata.is_dir() {
            ensure!(
                !looks_like_git_dir(&path),
                "target contains a project or repository: {}",
                path.display()
            );
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

/// A repository cloned `--bare` or whose `.git` was renamed: `HEAD` plus
/// `objects/` and `refs/` is the smallest reliable signature.
fn looks_like_git_dir(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir()
}

fn protect_project_inputs(root: &Path, target: &Path) -> Result<()> {
    // A selected member can inherit inputs declared outside its own tree.
    // Inspect enclosing manifests as well as manifests discovered below root.
    for ancestor in root.ancestors().skip(1) {
        let manifest = ancestor.join("Cargo.toml");
        match fs::symlink_metadata(&manifest) {
            Ok(metadata) => {
                check_plain(&manifest, &metadata)?;
                ensure!(metadata.is_file(), "manifest is not a regular file");
                protect_manifest_inputs(&manifest, target)?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("cannot inspect enclosing manifest"),
        }
    }
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
                if ![
                    "target",
                    "node_modules",
                    ".git",
                    ".hg",
                    ".svn",
                    ".jj",
                    ".bzr",
                    "_darcs",
                    ".fossil",
                ]
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
    // Path dependencies declare sources the build reads; one pointing into
    // target makes the cache non-disposable.
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        collect_dependency_paths(manifest.get(key), &mut inputs);
    }
    if let Some(workspace) = manifest.get("workspace") {
        collect_dependency_paths(workspace.get("dependencies"), &mut inputs);
        if let Some(package) = workspace.get("package") {
            // Inherited paths are relative to the workspace root, even when
            // the member manifest only contains `readme.workspace = true`.
            for key in ["readme", "license-file"] {
                if let Some(path) = package.get(key).and_then(toml::Value::as_str) {
                    inputs.push(path);
                }
            }
        }
    }
    if let Some(patch) = manifest.get("patch").and_then(toml::Value::as_table) {
        for dependencies in patch.values() {
            collect_dependency_paths(Some(dependencies), &mut inputs);
        }
    }
    // [replace] is itself a dependency table.
    collect_dependency_paths(manifest.get("replace"), &mut inputs);
    if let Some(platforms) = manifest.get("target").and_then(toml::Value::as_table) {
        for platform in platforms.values() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                collect_dependency_paths(platform.get(key), &mut inputs);
            }
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

fn collect_dependency_paths<'a>(table: Option<&'a toml::Value>, inputs: &mut Vec<&'a str>) {
    let Some(table) = table.and_then(toml::Value::as_table) else {
        return;
    };
    for dependency in table.values() {
        if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
            inputs.push(path);
        }
    }
}

fn protect_git_files(root: &Path, target: &Path) -> Result<()> {
    // Check every enclosing repository, including an outer repository tracking
    // files inside a nested one. Never let inherited GIT_DIR/GIT_INDEX_FILE etc.
    // redirect this check away from the actual project. Git is resolved lazily:
    // a project outside any repository must not require it.
    let mut git_exe: Option<PathBuf> = None;
    for ancestor in root.ancestors() {
        let marker = ancestor.join(".git");
        let marker_metadata = match fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err).context("cannot inspect enclosing repository"),
        };
        // Only a real repository marker can carry an index listing target
        // files. Anything else (a FIFO, socket, device) would block or
        // confuse git, so it fails closed rather than being opened.
        let usable = if is_link(&marker_metadata) {
            match fs::canonicalize(&marker) {
                Ok(resolved) => resolved.is_dir() || resolved.is_file(),
                Err(err) if err.kind() == ErrorKind::NotFound => false,
                Err(err) => return Err(err).context("cannot resolve enclosing repository marker"),
            }
        } else {
            marker_metadata.is_dir() || marker_metadata.is_file()
        };
        ensure!(
            usable,
            "enclosing repository marker is not a directory or file: {}",
            marker.display()
        );
        let mut pathspec = OsString::from(":(top,literal)");
        let relative = target.strip_prefix(ancestor)?;
        #[cfg(windows)]
        pathspec.push(relative.to_string_lossy().replace('\\', "/"));
        #[cfg(not(windows))]
        pathspec.push(relative);
        if git_exe.is_none() {
            git_exe = Some(resolve_tool("git").context("cannot locate git; refusing cleanup")?);
        }
        let mut command = Command::new(git_exe.as_deref().unwrap());
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        command
            .args([
                "-c",
                "core.fsmonitor=false",
                "ls-files",
                "--cached",
                "-z",
                "--",
            ])
            .arg(pathspec)
            .current_dir(ancestor);
        let output = run_with_timeout(&mut command, GIT_CHECK_TIMEOUT)?;
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

fn dir_identity(path: &Path) -> Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect {}", path.display()))?;
        Ok(FileIdentity(metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS opens directories; OPEN_REPARSE_POINT
        // inspects a link itself rather than its destination.
        let file = fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(0x02000000 | 0x00200000)
            .open(path)
            .with_context(|| format!("cannot inspect directory identity: {}", path.display()))?;
        let information = winapi_util::file::information(&file)?;
        Ok(FileIdentity(
            information.volume_serial_number(),
            information.file_index(),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        bail!("directory identity checks are unsupported on this platform")
    }
}

/// Move the validated `target/` aside to an unoccupied sibling name before
/// handing it to `cargo clean`. The rename pins the exact inode that passed
/// validation: anything planted at the original `target` name afterwards is
/// left alone, and a swap between validation and pinning fails the identity
/// check instead of redirecting the deletion.
pub(crate) fn pin_target(validated: &ValidatedTarget) -> Result<PathBuf> {
    let target = &validated.path;
    let parent = target.parent().context("target has no parent directory")?;
    ensure!(
        dir_identity(target)? == validated.identity,
        "target changed after validation; refusing cleanup"
    );
    for attempt in 0..PIN_ATTEMPTS {
        let staged = parent.join(format!(
            ".rucycle-cleaning-{}-{attempt}",
            std::process::id()
        ));
        match rename_no_replace(target, &staged) {
            Ok(()) => match confirm_pinned(&staged, validated.identity) {
                Ok(()) => return Ok(staged),
                Err(err) => {
                    let restored = restore_target(&staged, target);
                    let message = if restored {
                        "pinned target failed re-verification; original path restored".to_string()
                    } else {
                        format!(
                            "pinned target failed re-verification; data retained at {}",
                            staged.display()
                        )
                    };
                    return Err(err.context(message));
                }
            },
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::AlreadyExists
                        | ErrorKind::DirectoryNotEmpty
                        | ErrorKind::NotADirectory
                        | ErrorKind::IsADirectory
                ) =>
            {
                continue;
            }
            Err(err) => return Err(err).context("cannot pin target directory"),
        }
    }
    bail!("cannot pin target directory: staging names are occupied")
}

fn confirm_pinned(staged: &Path, identity: FileIdentity) -> Result<()> {
    let metadata =
        fs::symlink_metadata(staged).context("cannot inspect pinned target directory")?;
    check_plain(staged, &metadata)?;
    ensure!(metadata.is_dir(), "pinned target is not a directory");
    ensure!(
        dir_identity(staged)? == identity,
        "pinned target is not the validated directory"
    );
    ensure!(
        fs::canonicalize(staged)? == staged,
        "pinned target resolves outside the selected directory"
    );
    Ok(())
}

/// Best-effort restore of a pinned directory to its original `target` name.
/// Returns false when the name is already taken or the move fails; callers
/// then surface the retained path instead of losing track of the data.
pub(crate) fn restore_target(staged: &Path, original: &Path) -> bool {
    rename_no_replace(staged, original).is_ok()
}

/// Both staging and rollback must refuse existing destinations atomically.
/// A separate exists() check followed by rename() would still overwrite an
/// empty directory created between those operations on Unix.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    {
        use rustix::fs::{renameat_with, RenameFlags, CWD};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "path contains NUL"));
        }
        // SAFETY: Both buffers are NUL-terminated and live through the call.
        // No REPLACE_EXISTING or COPY_ALLOWED flags: never overwrite or copy.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "atomic no-replace directory moves are unsupported on this platform",
        ))
    }
}

/// Resolve `git`/`cargo` ourselves instead of letting the OS search PATH:
/// the OS search can include the process working directory, and rucycle is
/// typically run inside the very directories under audit. A planted
/// `git.exe`/`cargo.exe` must never satisfy a safety check or a clean.
pub(crate) fn resolve_tool(name: &str) -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH").context("PATH is not set")?;
    resolve_tool_in(name, &path_var)
}

fn resolve_tool_in(name: &str, path_var: &OsStr) -> Result<PathBuf> {
    for directory in std::env::split_paths(path_var) {
        // Relative or empty PATH entries resolve against the caller's working
        // directory, which is exactly the untrusted location.
        if !directory.is_absolute() {
            continue;
        }
        for candidate in executable_candidates(&directory, name) {
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
    }
    bail!("{name} was not found on PATH")
}

fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let path_ext = std::env::var_os("PATHEXT")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let lower = name.to_lowercase();
        let has_extension = path_ext
            .split(';')
            .any(|extension| lower.ends_with(&extension.to_lowercase()));
        let mut candidates = Vec::new();
        if has_extension {
            candidates.push(directory.join(name));
        }
        for extension in path_ext
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            candidates.push(directory.join(format!("{name}{extension}")));
        }
        candidates
    }
    #[cfg(not(windows))]
    {
        vec![directory.join(name)]
    }
}

fn is_executable_file(path: &Path) -> bool {
    // fs::metadata follows links: tools may legitimately be symlinked.
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `Command::output` has no timeout; a blocked `git` (wedged filesystem,
/// a marker that was a FIFO at spawn time) must refuse the cleanup instead
/// of hanging forever. The deadline includes both output pipes: descendants
/// may inherit them and keep them open after the direct child exits.
fn run_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot check Git-tracked files; refusing cleanup")?;

    let stdout = read_pipe(child.stdout.take().context("missing stdout pipe")?);
    let stderr = read_pipe(child.stderr.take().context("missing stderr pipe")?);
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut status = None;
        let mut stdout_bytes = None;
        let mut stderr_bytes = None;
        loop {
            if status.is_none() {
                status = child
                    .try_wait()
                    .context("cannot wait for git; refusing cleanup")?;
            }
            for (receiver, bytes) in [(&stdout, &mut stdout_bytes), (&stderr, &mut stderr_bytes)] {
                if bytes.is_none() {
                    match receiver.try_recv() {
                        Ok(result) => {
                            *bytes =
                                Some(result.context("cannot read git output; refusing cleanup")?)
                        }
                        Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Disconnected) => {
                            bail!("git output reader stopped; refusing cleanup")
                        }
                    }
                }
            }
            if let Some(status) = status {
                if stdout_bytes.is_some() && stderr_bytes.is_some() {
                    return Ok(Output {
                        status,
                        stdout: stdout_bytes.take().unwrap(),
                        stderr: stderr_bytes.take().unwrap(),
                    });
                }
            }
            ensure!(
                Instant::now() < deadline,
                "git inspection timed out; refusing cleanup"
            );
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        // Bound reaping too. Do not join readers whose pipes may still be held
        // by descendants; they exit when those pipes close.
        for _ in 0..40 {
            if !matches!(child.try_wait(), Ok(None)) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    result
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        // A failed read is not an empty tracked-file list.
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn fixture() -> (tempfile::TempDir, ProjectInfo) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let project = ProjectInfo {
            root: root.clone(),
            manifest_path: root.join("Cargo.toml"),
            name: "demo".to_string(),
            total_size: 0,
            target_size: 0,
            last_modified: SystemTime::UNIX_EPOCH,
        };
        (temp, project)
    }

    #[test]
    fn pin_moves_only_the_validated_directory() {
        let (_temp, project) = fixture();
        let validated = validated_target(&project).unwrap().unwrap();
        let staged = pin_target(&validated).unwrap();
        assert!(staged.is_dir());
        assert!(!validated.path.exists());
        assert_eq!(staged.parent().unwrap(), project.root);
        assert!(restore_target(&staged, &validated.path));
        assert!(validated.path.join("CACHEDIR.TAG").exists());
    }

    #[test]
    fn pin_preserves_occupied_staging_names_even_when_empty() {
        let (_temp, project) = fixture();
        let validated = validated_target(&project).unwrap().unwrap();
        for attempt in 0..PIN_ATTEMPTS {
            fs::create_dir(project.root.join(format!(
                ".rucycle-cleaning-{}-{attempt}",
                std::process::id()
            )))
            .unwrap();
        }
        assert!(pin_target(&validated).is_err());
        assert!(validated.path.join("CACHEDIR.TAG").exists());
        for attempt in 0..PIN_ATTEMPTS {
            assert_eq!(
                fs::read_dir(project.root.join(format!(
                    ".rucycle-cleaning-{}-{attempt}",
                    std::process::id()
                )))
                .unwrap()
                .count(),
                0
            );
        }
    }

    #[test]
    fn restore_preserves_a_recreated_target_even_when_empty() {
        let (_temp, project) = fixture();
        let validated = validated_target(&project).unwrap().unwrap();
        let staged = pin_target(&validated).unwrap();
        fs::create_dir(&validated.path).unwrap();
        let recreated = dir_identity(&validated.path).unwrap();
        assert!(!restore_target(&staged, &validated.path));
        assert_eq!(dir_identity(&validated.path).unwrap(), recreated);
        assert!(staged.join("CACHEDIR.TAG").exists());
        assert_eq!(fs::read_dir(&validated.path).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn pin_rejects_a_post_validation_swap() {
        use std::os::unix::fs::symlink;
        let (_temp, project) = fixture();
        let validated = validated_target(&project).unwrap().unwrap();
        let precious = project.root.join("precious");
        fs::create_dir(&precious).unwrap();
        fs::write(precious.join("data"), "precious").unwrap();
        fs::rename(&validated.path, project.root.join("old-target")).unwrap();
        symlink(&precious, &validated.path).unwrap();
        assert!(pin_target(&validated).is_err());
        assert!(validated.path.is_symlink());
        assert_eq!(
            fs::read_to_string(precious.join("data")).unwrap(),
            "precious"
        );
    }

    #[test]
    fn resolve_tool_skips_relative_path_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("bin")).unwrap();
        let directory = temp.path().join("bin").canonicalize().unwrap();
        let exe = directory.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        fs::write(&exe, "fake").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(resolve_tool_in("tool", &OsString::from("relative-dir")).is_err());
        assert_eq!(resolve_tool_in("tool", directory.as_os_str()).unwrap(), exe);
        let mut combined = OsString::from("relative-dir");
        combined.push(if cfg!(windows) { ";" } else { ":" });
        combined.push(&directory);
        assert_eq!(resolve_tool_in("tool", &combined).unwrap(), exe);
    }

    #[cfg(unix)]
    #[test]
    fn git_timeout_also_bounds_inherited_output_pipes() {
        // The parent exits successfully, but its child keeps both pipes open.
        // Reaping the parent must not turn pipe collection into an unbounded wait.
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 0.4 & exit 0"]);
        let result = run_with_timeout(&mut command, Duration::from_millis(50));
        assert!(result.is_err(), "inherited pipes must obey the timeout");
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn pipe_read_errors_are_preserved_instead_of_becoming_empty_output() {
        struct BrokenPipe;
        impl Read for BrokenPipe {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("fixture read failure"))
            }
        }
        assert!(read_pipe(BrokenPipe)
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err());
    }

    #[test]
    fn git_output_is_collected_before_reporting_success() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = Command::new(resolve_tool("git").unwrap());
        command.arg("--version").current_dir(temp.path());
        let output = run_with_timeout(&mut command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("git version "));
    }
}
