//! Compile the pinned composition into the program. `scripts/discover-modules.py
//! --pin` writes `.local/composition/manifest.txt` (the Platform commit, its
//! version and release tag, and each mounted Domain or Extension's commit and
//! version); this script turns it into `composition.rs` for `src/composition.rs`.
//! The pin is checked against the checkouts being compiled: when Git is
//! available and the Platform or a pinned container is at another commit than
//! the pin recorded, the pin is left out and the reason compiled in instead.
//! Without a pin the program only knows the Platform commit, read from Git at
//! build time, and says so.
//! Keep Git watches in this executable host: putting these changing facts in
//! the shared registry recompiles every module after a commit or checkout.
//! Bilibi-specific composition contract.
use std::{env, fs, path::Path, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("../..");
    let folder = root.join(".local/composition");
    let manifest = folder.join("manifest.txt");
    // Cargo reruns a script whose watched path is missing on every build, so
    // watch the pin's folder (created empty here when absent, so a later
    // `--pin` is noticed) and never a path that does not exist.
    let _ = fs::create_dir_all(&folder);
    for path in [&folder, &manifest]
        .into_iter()
        .filter(|path| path.exists())
    {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let generated = match fs::read_to_string(&manifest) {
        Ok(text) => pinned(&text, &root),
        Err(_) => unpinned(&root),
    };
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("composition.rs");
    fs::write(out, generated).expect("write composition.rs");
}

/// `key = "value"` lines and `[containers."<layer>/<id>"]` headings, as
/// `--pin` writes them; unknown keys are ignored so the pin may grow.
fn pinned(text: &str, root: &Path) -> String {
    let mut platform_commit = None;
    let mut platform_version = None;
    let mut platform_tag = None;
    let mut containers: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix("[containers.\"")
            .and_then(|rest| rest.strip_suffix("\"]"))
        {
            containers.push((name.to_owned(), None, None));
            continue;
        }
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        let value = value.trim_matches('"').to_owned();
        match (containers.last_mut(), key) {
            (None, "platform") => platform_commit = Some(value),
            (None, "platform_version") => platform_version = Some(value),
            (None, "platform_tag") => platform_tag = Some(value),
            (Some(container), "head") => container.1 = Some(value),
            (Some(container), "version") => container.2 = Some(value),
            _ => {}
        }
    }
    let (Some(commit), Some(version)) = (platform_commit, platform_version) else {
        panic!(".local/composition/manifest.txt lacks platform or platform_version; run scripts/discover-modules.py --pin again");
    };
    let containers: Vec<_> = containers
        .into_iter()
        .map(|(name, head, version)| {
            let head =
                head.unwrap_or_else(|| panic!("{name} in the pin has no head; run --pin again"));
            (name, head, version)
        })
        .collect();
    if let Some(reason) = drift(root, &commit, &containers) {
        return format!(
            "pub const PIN: Option<Pin> = None;\npub const PIN_STALE: Option<&str> = Some({reason:?});\npub const BUILT_FROM: Option<&str> = None;\npub const BUILT_CONTAINERS: &[PinnedContainer] = &[];\n"
        );
    }
    let mut code = format!(
        "pub const PIN: Option<Pin> = Some(Pin {{ platform_commit: {commit:?}, platform_version: {version:?}, platform_tag: {platform_tag:?}, containers: &[\n"
    );
    for (name, head, version) in containers {
        code.push_str(&format!(
            "    PinnedContainer {{ name: {name:?}, commit: {head:?}, version: {version:?} }},\n"
        ));
    }
    code.push_str("] });\npub const PIN_STALE: Option<&str> = None;\npub const BUILT_FROM: Option<&str> = None;\npub const BUILT_CONTAINERS: &[PinnedContainer] = &[];\n");
    code
}

/// What differs between the pin and the checkouts being compiled, if Git can
/// tell. Without Git (or outside a Git checkout of the Platform) the pin is trusted.
fn drift(
    root: &Path,
    platform_commit: &str,
    containers: &[(String, String, Option<String>)],
) -> Option<String> {
    watch_head(root);
    let head = git(root, &["rev-parse", "HEAD"])?;
    if head != platform_commit {
        return Some(format!(
            "the Platform is at {} but the pin was made at {}",
            short(&head),
            short(platform_commit)
        ));
    }
    for (name, pinned, _) in containers {
        let folder = root.join("apps").join(name);
        watch_head(&folder);
        let own = git(&folder, &["rev-parse", "--show-toplevel"])
            .is_some_and(|top| fs::canonicalize(&folder).is_ok_and(|path| Path::new(&top) == path));
        if !own {
            return Some(format!("apps/{name} is not its own Git checkout"));
        }
        let head = git(&folder, &["rev-parse", "HEAD"])?;
        if &head != pinned {
            return Some(format!(
                "apps/{name} is at {} but the pin was made at {}",
                short(&head),
                short(pinned)
            ));
        }
    }
    None
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Rerun when HEAD moves: the HEAD file, the branch file it points at, the
/// packed refs, and the HEAD reflog. A packed branch has no loose file until
/// its next commit and the symbolic HEAD does not change, but Git appends to
/// `logs/HEAD` on every commit, checkout, reset and merge. A missing path would
/// rerun every build, so only existing paths are watched.
fn watch_head(dir: &Path) {
    let branch = git(dir, &["symbolic-ref", "-q", "HEAD"]);
    for path in ["HEAD", "logs/HEAD", "packed-refs"]
        .into_iter()
        .chain(branch.as_deref())
        .filter_map(|name| git(dir, &["rev-parse", "--git-path", name]))
        .map(|path| dir.join(path))
        .filter(|path| path.is_file())
    {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// A build without a pin: the Platform commit and each mounted checkout's own
/// commit (and release tag, when it is at one) are read from Git, so a
/// development build shows true facts instead of nothing.
fn unpinned(root: &Path) -> String {
    watch_head(root);
    let commit = git(root, &["rev-parse", "HEAD"]);
    let mut found = String::new();
    for layer in ["domains", "extensions"] {
        let Ok(entries) = fs::read_dir(root.join("apps").join(layer)) else {
            continue;
        };
        let mut dirs: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        dirs.sort();
        for dir in dirs.into_iter().filter(|dir| dir.join(".git").exists()) {
            let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            watch_head(&dir);
            let Some(head) = git(&dir, &["rev-parse", "HEAD"]) else {
                continue;
            };
            let version = git(
                &dir,
                &["describe", "--tags", "--exact-match", "--match", "v*"],
            )
            .map(|tag| tag.trim_start_matches('v').to_owned());
            found.push_str(&format!(
                "    PinnedContainer {{ name: \"{layer}/{name}\", commit: {head:?}, version: {version:?} }},\n"
            ));
        }
    }
    format!(
        "pub const PIN: Option<Pin> = None;\npub const PIN_STALE: Option<&str> = None;\npub const BUILT_FROM: Option<&str> = {commit:?};\npub const BUILT_CONTAINERS: &[PinnedContainer] = &[\n{found}];\n"
    )
}
