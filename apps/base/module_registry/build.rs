//! Compile the pinned composition into the program. `scripts/discover-modules.py
//! --pin` writes `.local/composition/manifest.txt` (the Platform commit, its
//! version and release tag, and each mounted Domain or Extension's commit and
//! version); this script turns it into `composition.rs` for `src/composition.rs`.
//! Without a pin the program only knows the Platform commit, read from Git at
//! build time, and says so.
use std::{env, fs, path::Path, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("../../..");
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
        Ok(text) => pinned(&text),
        Err(_) => unpinned(&root),
    };
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("composition.rs");
    fs::write(out, generated).expect("write composition.rs");
}

/// `key = "value"` lines and `[containers."<layer>/<id>"]` headings, as
/// `--pin` writes them; unknown keys are ignored so the pin may grow.
fn pinned(text: &str) -> String {
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
    let mut code = format!(
        "pub const PIN: Option<Pin> = Some(Pin {{ platform_commit: {commit:?}, platform_version: {version:?}, platform_tag: {platform_tag:?}, containers: &[\n"
    );
    for (name, head, version) in containers {
        let head = head.unwrap_or_else(|| panic!("{name} in the pin has no head; run --pin again"));
        code.push_str(&format!(
            "    PinnedContainer {{ name: {name:?}, commit: {head:?}, version: {version:?} }},\n"
        ));
    }
    code.push_str("] });\npub const BUILT_FROM: Option<&str> = None;\n");
    code
}

fn unpinned(root: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    // Rerun when HEAD moves: the HEAD file and the branch file it points at
    // (absent when the branch is packed; a missing path would rerun every build).
    for path in git(&["rev-parse", "--git-path", "HEAD"])
        .into_iter()
        .chain(
            git(&["symbolic-ref", "-q", "HEAD"])
                .and_then(|reference| git(&["rev-parse", "--git-path", &reference])),
        )
        .map(|path| root.join(path))
        .filter(|path| path.is_file())
    {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let commit = git(&["rev-parse", "HEAD"]);
    format!(
        "pub const PIN: Option<Pin> = None;\npub const BUILT_FROM: Option<&str> = {commit:?};\n"
    )
}
