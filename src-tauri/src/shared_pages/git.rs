//! Publishing through the system `git` and the user's own credential helper.
//!
//! Every write replaces the remote's `main` with one orphan commit holding
//! the whole tree, so removed ciphertext leaves the reachable history. The
//! push is fenced with `--force-with-lease` on the commit fetched just
//! before it, and a lost race is retried once from the fetch.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use super::site::MARKER;
use super::{Code, SharedPagesError};

const GIT: &str = "/usr/bin/git";
const COMMIT_MESSAGE: &str = "Update shared pages";

/// Fetches `remote` into the clone at `dir`, resets the working tree to
/// what is live, lets `apply` rewrite it, and pushes the result.
pub(crate) fn publish(
    dir: &Path,
    remote: &str,
    mut apply: impl FnMut(&Path) -> Result<(), SharedPagesError>,
) -> Result<(), SharedPagesError> {
    prepare_clone(dir, remote)?;
    let mut retried = false;
    loop {
        let live = fetch(dir)?;
        apply(dir)?;
        git(dir, &["add", "--all"])?;
        let tree = text(git(dir, &["write-tree"])?);
        let commit = text(git(dir, &["commit-tree", &tree, "-m", COMMIT_MESSAGE])?);
        let lease = format!(
            "--force-with-lease=main:{}",
            live.as_deref().unwrap_or_default()
        );
        let refspec = format!("{commit}:refs/heads/main");
        let pushed = run(dir, &["push", "--quiet", &lease, "origin", &refspec])?;
        if pushed.status.success() {
            let upkeep: [&[&str]; 4] = [
                &["update-ref", "refs/remotes/origin/main", commit.as_str()],
                &[
                    "checkout",
                    "--quiet",
                    "--force",
                    "-B",
                    "main",
                    commit.as_str(),
                ],
                &["reflog", "expire", "--expire=now", "--all"],
                &["gc", "--prune=now", "--quiet"],
            ];
            for args in upkeep {
                if let Err(error) = git(dir, args) {
                    log::warn!(
                        "Shared pages clone upkeep after a push failed: {}",
                        error.message
                    );
                }
            }
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&pushed.stderr);
        if !retried && stderr.contains("stale info") {
            retried = true;
            continue;
        }
        return Err(push_failed(&stderr));
    }
}

fn prepare_clone(dir: &Path, remote: &str) -> Result<(), SharedPagesError> {
    if !dir.join(".git").is_dir() {
        fs::create_dir_all(dir).map_err(|error| {
            SharedPagesError::new(
                Code::Failed,
                format!("The site clone could not be made: {error}"),
            )
        })?;
        git(dir, &["init", "--quiet", "--initial-branch=main"])?;
    }
    if run(dir, &["remote", "get-url", "origin"])?.status.success() {
        git(dir, &["remote", "set-url", "origin", remote])?;
    } else {
        git(dir, &["remote", "add", "origin", remote])?;
    }
    Ok(())
}

/// The commit `main` is live at, with the working tree reset to it; `None`
/// and an empty working tree when the remote has no `main` yet.
fn fetch(dir: &Path) -> Result<Option<String>, SharedPagesError> {
    let fetched = run(dir, &["fetch", "--quiet", "--prune", "origin"])?;
    if !fetched.status.success() {
        return Err(push_failed(&String::from_utf8_lossy(&fetched.stderr)));
    }
    let live = run(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main^{commit}",
        ],
    )?;
    if !live.status.success() {
        clear_working_tree(dir)?;
        return Ok(None);
    }
    let live = text(live);
    let marker = format!("{live}:{MARKER}");
    if !run(dir, &["cat-file", "-e", &marker])?.status.success() {
        return Err(SharedPagesError::new(
            Code::RemoteNotSonaSite,
            "The site repository has files Sona did not write. Point Sona at an empty repository.",
        ));
    }
    git(
        dir,
        &["checkout", "--quiet", "--force", "-B", "main", &live],
    )?;
    git(dir, &["clean", "--quiet", "--force", "--force", "-d", "-x"])?;
    Ok(Some(live))
}

fn clear_working_tree(dir: &Path) -> Result<(), SharedPagesError> {
    let cleared = fs::read_dir(dir).and_then(|entries| {
        for entry in entries {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    });
    cleared.map_err(|error| {
        SharedPagesError::new(
            Code::Failed,
            format!("The site clone could not be cleared: {error}"),
        )
    })
}

fn run(dir: &Path, args: &[&str]) -> Result<Output, SharedPagesError> {
    Command::new(GIT)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| SharedPagesError::new(Code::Failed, format!("git could not run: {error}")))
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, SharedPagesError> {
    let output = run(dir, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(SharedPagesError::new(
        Code::PushFailed,
        format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    ))
}

fn text(output: Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn push_failed(stderr: &str) -> SharedPagesError {
    SharedPagesError::new(
        Code::PushFailed,
        format!("The push was refused: {}", stderr.trim()),
    )
}
