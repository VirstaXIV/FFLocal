//! Self-update: checks github.com/VirstaXIV/FFLocal's tags for a newer `vMAJOR.MINOR.PATCH`
//! than this build and, in a source checkout, downloads and applies it.
//!
//! Tag convention: a release is a `vX.Y.Z` (SemVer, matching `workspace.package.version` in
//! the root `Cargo.toml`) tag pushed to the repo. No GitHub "Release" object is required —
//! the check reads the tag list directly, so cutting a release is just
//! `git tag vX.Y.Z && git push origin vX.Y.Z`.

use std::io::Cursor;
use std::path::Path;

const TAGS_URL: &str = "https://api.github.com/repos/VirstaXIV/FFLocal/tags";
const USER_AGENT: &str = "fflocal-launcher";

/// Root-level paths an applied update must never touch: git metadata and everything
/// `.gitignore` keeps out of the versioned tree (build output, local settings, logs).
const PRESERVE: &[&str] = &[".git", "target", "screenshots", "config.toml"];

#[derive(serde::Deserialize)]
struct Tag {
    name: String,
    zipball_url: String,
}

#[derive(Clone)]
pub struct Update {
    pub version: semver::Version,
    pub tag: String,
    zipball_url: String,
}

/// Blocking network call: run it off the UI thread. `Ok(None)` means already current.
pub fn check(current: &semver::Version) -> anyhow::Result<Option<Update>> {
    let tags: Vec<Tag> = ureq::get(TAGS_URL).header("User-Agent", USER_AGENT).call()?.body_mut().read_json()?;
    let newest = tags
        .into_iter()
        .filter_map(|t| {
            let version = semver::Version::parse(t.name.strip_prefix('v')?).ok()?;
            Some((version, t))
        })
        .filter(|(v, _)| v > current)
        .max_by(|(a, _), (b, _)| a.cmp(b));
    Ok(newest.map(|(version, t)| Update { version, tag: t.name, zipball_url: t.zipball_url }))
}

/// Blocking: download the tagged source and overlay it onto `workspace`; run off the UI
/// thread. Leaves `PRESERVE`d paths untouched. Does not delete files the new tag removed —
/// stale files left from an old tree don't stop a Rust rebuild.
pub fn apply(update: &Update, workspace: &Path) -> anyhow::Result<()> {
    let bytes = ureq::get(&update.zipball_url)
        .header("User-Agent", USER_AGENT)
        .call()?
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;

    let extract_dir = std::env::temp_dir().join(format!("fflocal-update-{}", update.tag));
    let _ = std::fs::remove_dir_all(&extract_dir);
    archive.extract(&extract_dir)?;

    // GitHub zipballs wrap everything in one top-level `<owner>-<repo>-<sha>/` directory.
    let root = std::fs::read_dir(&extract_dir)?
        .filter_map(Result::ok)
        .find(|e| e.path().is_dir())
        .ok_or_else(|| anyhow::anyhow!("update archive was empty"))?
        .path();

    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        if PRESERVE.iter().any(|p| name.as_os_str() == std::ffi::OsStr::new(p)) {
            continue;
        }
        let from = entry.path();
        let to = workspace.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    std::fs::remove_dir_all(&extract_dir).ok();
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test against a repo known to have real `vX.Y.Z` tags; not run in CI.
    #[test]
    #[ignore]
    fn check_against_a_real_tagged_repo() {
        let tags: Vec<Tag> = ureq::get("https://api.github.com/repos/VirstaXIV/DynamicTextureManager/tags")
            .header("User-Agent", USER_AGENT)
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        assert!(tags.iter().any(|t| t.name == "v0.9.9"));
        let ancient = semver::Version::new(0, 0, 1);
        let newest = tags.into_iter().filter_map(|t| Some((semver::Version::parse(t.name.strip_prefix('v')?).ok()?, t))).filter(|(v, _)| *v > ancient).max_by(|(a, _), (b, _)| a.cmp(b));
        assert_eq!(newest.unwrap().0, semver::Version::new(0, 9, 9));
    }

    /// Network test: downloads a real small tag and applies it onto a scratch directory,
    /// checking extraction landed real files and `PRESERVE`d paths were left alone.
    #[test]
    #[ignore]
    fn apply_onto_a_scratch_workspace() {
        let update = Update {
            version: semver::Version::new(0, 1, 0),
            tag: "apply-test-v0.1.0".into(),
            zipball_url: "https://api.github.com/repos/VirstaXIV/DynamicTextureManager/zipball/refs/tags/v0.1.0".into(),
        };
        let workspace = std::env::temp_dir().join("fflocal-update-apply-test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/marker"), b"keep me").unwrap();
        std::fs::write(workspace.join("config.toml"), b"keep me too").unwrap();

        apply(&update, &workspace).unwrap();

        assert!(workspace.join("README.md").is_file(), "expected a real file from the archive to land");
        assert_eq!(std::fs::read(workspace.join(".git/marker")).unwrap(), b"keep me", "PRESERVEd .git must survive untouched");
        assert_eq!(std::fs::read(workspace.join("config.toml")).unwrap(), b"keep me too", "PRESERVEd config.toml must survive untouched");
        std::fs::remove_dir_all(&workspace).ok();
    }
}
