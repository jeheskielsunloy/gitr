use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct ChangeItem {
    pub path: String,
    pub status: String,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct CommitItem {
    pub hash: String,
    pub relative_time: String,
    pub author: String,
    pub summary: String,
}

pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn get_repo_root(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    None
}

pub fn get_branch(path: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "branch", "--show-current"])
        .output();
    if let Ok(o) = output {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }

    // Fallback if detached HEAD or no branch yet
    let hash_out = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "rev-parse", "--short", "HEAD"])
        .output();
    if let Ok(o) = hash_out {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return format!("detached:{}", s);
        }
    }

    "init".to_string()
}

pub fn get_changes(path: &Path) -> Result<Vec<ChangeItem>, String> {
    let output = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "status", "--porcelain=v1", "-uall"])
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = Vec::new();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[0..2].trim().to_string();
        let file_path = line[3..].trim();
        // Handle renamed files "old -> new"
        let clean_path = if let Some((_, new_p)) = file_path.split_once(" -> ") {
            new_p.trim().to_string()
        } else {
            file_path.to_string()
        };

        changes.push(ChangeItem {
            path: clean_path,
            status,
            selected: true,
        });
    }

    Ok(changes)
}

pub fn get_diff(path: &Path, files: &[String]) -> Result<String, String> {
    if files.is_empty() {
        return Ok(String::new());
    }

    let mut full_diff = String::new();
    let path_str = path.to_str().unwrap_or(".");

    // First try diff against HEAD
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str, "diff", "HEAD", "--"]);
    for f in files {
        cmd.arg(f);
    }
    if let Ok(out) = cmd.output() {
        if out.status.success() {
            full_diff.push_str(&String::from_utf8_lossy(&out.stdout));
        }
    }

    // If diff against HEAD is empty (e.g. newly staged on fresh repo or untracked), check cached and normal diff
    if full_diff.trim().is_empty() {
        let mut cmd_cached = Command::new("git");
        cmd_cached.args(["-C", path_str, "diff", "--cached", "--"]);
        for f in files {
            cmd_cached.arg(f);
        }
        if let Ok(out) = cmd_cached.output() {
            if out.status.success() {
                full_diff.push_str(&String::from_utf8_lossy(&out.stdout));
            }
        }

        let mut cmd_unstaged = Command::new("git");
        cmd_unstaged.args(["-C", path_str, "diff", "--"]);
        for f in files {
            cmd_unstaged.arg(f);
        }
        if let Ok(out) = cmd_unstaged.output() {
            if out.status.success() {
                full_diff.push_str(&String::from_utf8_lossy(&out.stdout));
            }
        }
    }

    // For any untracked file that didn't show in git diff, run git diff --no-index /dev/null
    for f in files {
        let full_file_path = path.join(f);
        if full_file_path.is_file() && !full_diff.contains(f) {
            let out = Command::new("git")
                .args(["diff", "--no-index", "/dev/null", full_file_path.to_str().unwrap_or("")])
                .output();
            if let Ok(o) = out {
                full_diff.push_str(&String::from_utf8_lossy(&o.stdout));
            }
        }
    }

    // Truncate diff if larger than ~60KB
    const MAX_DIFF_LEN: usize = 60_000;
    if full_diff.len() > MAX_DIFF_LEN {
        full_diff.truncate(MAX_DIFF_LEN);
        full_diff.push_str("\n\n[Diff truncated to 60KB to fit token limits...]\n");
    }

    Ok(full_diff)
}

pub fn get_history(path: &Path, count: usize) -> Result<Vec<CommitItem>, String> {
    let count_arg = format!("-n{}", count);
    let output = Command::new("git")
        .args([
            "-C",
            path.to_str().unwrap_or("."),
            "log",
            &count_arg,
            "--pretty=format:%h\t%ar\t%an\t%s",
        ])
        .output()
        .map_err(|e| format!("Failed to run git log: {}", e))?;

    if !output.status.success() {
        return Ok(Vec::new()); // might be a brand new repo with 0 commits
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut history = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 4 {
            history.push(CommitItem {
                hash: parts[0].to_string(),
                relative_time: parts[1].to_string(),
                author: parts[2].to_string(),
                summary: parts[3..].join(" "),
            });
        }
    }

    Ok(history)
}

pub fn commit(path: &Path, files: &[String], message: &str) -> Result<(), String> {
    if files.is_empty() {
        return Err("No files selected to commit".to_string());
    }

    let path_str = path.to_str().unwrap_or(".");

    // 1. Stage files
    let mut add_cmd = Command::new("git");
    add_cmd.args(["-C", path_str, "add", "-A", "--"]);
    for f in files {
        add_cmd.arg(f);
    }
    let add_output = add_cmd.output().map_err(|e| format!("git add failed: {}", e))?;
    if !add_output.status.success() {
        return Err(String::from_utf8_lossy(&add_output.stderr).to_string());
    }

    // 2. Commit
    let commit_output = Command::new("git")
        .args(["-C", path_str, "commit", "-m", message])
        .output()
        .map_err(|e| format!("git commit failed: {}", e))?;

    if !commit_output.status.success() {
        return Err(String::from_utf8_lossy(&commit_output.stderr).to_string());
    }

    Ok(())
}

pub fn push(path: &Path) -> Result<String, String> {
    let path_str = path.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", path_str, "push"])
        .output()
        .map_err(|e| format!("git push failed: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        if err.trim().is_empty() {
            return Err(String::from_utf8_lossy(&output.stdout).to_string());
        }
        return Err(err);
    }

    let msg = String::from_utf8_lossy(&output.stdout).to_string();
    let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}\n{}", msg.trim(), err_msg.trim()).trim().to_string();
    if combined.is_empty() {
        Ok("Pushed successfully".to_string())
    } else {
        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_repo_detection() {
        let current = Path::new(".");
        assert!(is_git_repo(current));
        let root = get_repo_root(current);
        assert!(root.is_some());
    }

    #[test]
    fn test_git_changes_detection() {
        let current = Path::new(".");
        let changes = get_changes(current).unwrap();
        // We have untracked/modified files in this repo
        assert!(!changes.is_empty());
        let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
        let diff = get_diff(current, &paths).unwrap();
        assert!(!diff.is_empty());
    }
}
