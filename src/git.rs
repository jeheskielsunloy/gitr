use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeItem {
    pub path: String,
    pub status: String,
    pub selected: bool,
    pub added: usize,
    pub deleted: usize,
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

pub fn parse_numstat_path(s: &str) -> String {
    let s = s.trim_matches('"');
    if s.contains('{') && s.contains('}') {
        if let Some((before_brace, rest)) = s.split_once('{') {
            if let Some((inside, after_brace)) = rest.split_once('}') {
                if let Some((_, dest)) = inside.split_once(" => ") {
                    let mut res = format!("{}{}{}", before_brace, dest, after_brace);
                    while res.contains("//") {
                        res = res.replace("//", "/");
                    }
                    return res;
                }
            }
        }
    }
    if let Some((_, dest)) = s.split_once(" => ") {
        return dest.to_string();
    }
    s.to_string()
}

pub fn count_file_lines(path: &Path) -> (usize, usize) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (0, 0);
    };
    if !metadata.is_file() || metadata.len() > 50 * 1024 * 1024 {
        return (0, 0);
    }

    let Ok(bytes) = std::fs::read(path) else {
        return (0, 0);
    };

    if bytes.is_empty() {
        return (0, 0);
    }

    let check_len = bytes.len().min(8000);
    if bytes[..check_len].contains(&0) {
        return (0, 0);
    }

    let mut lines = 0;
    for &b in &bytes {
        if b == b'\n' {
            lines += 1;
        }
    }
    if bytes.last() != Some(&b'\n') {
        lines += 1;
    }

    (lines, 0)
}

fn parse_numstat_output(stdout: &[u8], stats: &mut HashMap<String, (usize, usize)>) {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let added = parts[0].parse::<usize>().unwrap_or(0);
            let deleted = parts[1].parse::<usize>().unwrap_or(0);
            let path_part = parts[2..].join("\t");
            let clean_path = parse_numstat_path(&path_part);
            let entry = stats.entry(clean_path).or_insert((0, 0));
            entry.0 += added;
            entry.1 += deleted;
        }
    }
}

pub fn get_numstats(path: &Path) -> HashMap<String, (usize, usize)> {
    let mut stats = HashMap::new();
    let path_str = path.to_str().unwrap_or(".");

    let head_exists = get_head_commit_hash(path).is_some();

    if head_exists {
        if let Ok(output) = Command::new("git")
            .args(["-C", path_str, "-c", "core.quotePath=false", "diff", "HEAD", "--numstat"])
            .output()
        {
            if output.status.success() {
                parse_numstat_output(&output.stdout, &mut stats);
            }
        }
    } else {
        if let Ok(output) = Command::new("git")
            .args(["-C", path_str, "-c", "core.quotePath=false", "diff", "--cached", "--numstat"])
            .output()
        {
            if output.status.success() {
                parse_numstat_output(&output.stdout, &mut stats);
            }
        }
        if let Ok(output) = Command::new("git")
            .args(["-C", path_str, "-c", "core.quotePath=false", "diff", "--numstat"])
            .output()
        {
            if output.status.success() {
                parse_numstat_output(&output.stdout, &mut stats);
            }
        }
    }

    stats
}

pub fn get_changes(path: &Path) -> Result<Vec<ChangeItem>, String> {
    let output = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "-c", "core.quotePath=false", "status", "--porcelain=v1", "-uall"])
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = Vec::new();
    let numstats = get_numstats(path);

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
        let clean_path = clean_path.trim_matches('"').to_string();

        let (added, deleted) = if let Some(&(a, d)) = numstats.get(&clean_path) {
            (a, d)
        } else if status == "?" || status == "??" {
            count_file_lines(&path.join(&clean_path))
        } else {
            (0, 0)
        };

        changes.push(ChangeItem {
            path: clean_path,
            status,
            selected: true,
            added,
            deleted,
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

pub fn get_head_commit_hash(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", path.to_str().unwrap_or("."), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

pub fn get_unpushed_count(path: &Path) -> usize {
    let path_str = path.to_str().unwrap_or(".");

    // 1. Try checking against upstream tracking branch @{u}
    let output = Command::new("git")
        .args(["-C", path_str, "rev-list", "--count", "@{u}..HEAD"])
        .output();
    if let Ok(o) = output {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Ok(count) = s.parse::<usize>() {
                return count;
            }
        }
    }

    // 2. If no upstream, check if any remotes exist
    let remote_output = Command::new("git")
        .args(["-C", path_str, "remote"])
        .output();
    if let Ok(ro) = remote_output {
        if ro.status.success() {
            let remotes = String::from_utf8_lossy(&ro.stdout);
            if remotes.trim().is_empty() {
                return 0; // No remote configured, so 0 unpushed commits
            }
        } else {
            return 0;
        }
    } else {
        return 0;
    }

    // 3. If remotes exist, check HEAD commits not on any remote
    let rev_output = Command::new("git")
        .args(["-C", path_str, "rev-list", "--count", "HEAD", "--not", "--remotes"])
        .output();
    if let Ok(o) = rev_output {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Ok(count) = s.parse::<usize>() {
                return count;
            }
        }
    }

    0
}

pub fn push(path: &Path) -> Result<String, String> {
    let path_str = path.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", path_str, "push"])
        .output()
        .map_err(|e| format!("git push failed: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        if err.contains("no upstream branch") || err.contains("has no upstream branch") {
            let branch = get_branch(path);
            let upstream_output = Command::new("git")
                .args(["-C", path_str, "push", "--set-upstream", "origin", &branch])
                .output()
                .map_err(|e| format!("git push --set-upstream failed: {}", e))?;

            if upstream_output.status.success() {
                let msg = String::from_utf8_lossy(&upstream_output.stdout).to_string();
                let err_msg = String::from_utf8_lossy(&upstream_output.stderr).to_string();
                let combined = format!("{}\n{}", msg.trim(), err_msg.trim()).trim().to_string();
                return Ok(if combined.is_empty() {
                    "Pushed successfully (upstream set)".to_string()
                } else {
                    combined
                });
            }
        }
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

pub fn discard_changes(path: &Path, target: &str) -> Result<(), String> {
    let path_str = path.to_str().unwrap_or(".");

    // 1. Try git restore --staged --worktree -- <target>
    let restore_output = Command::new("git")
        .args(["-C", path_str, "restore", "--staged", "--worktree", "--", target])
        .output();

    if let Ok(ref out) = restore_output {
        if !out.status.success() {
            // Fallback for repos with no HEAD commit yet
            let _ = Command::new("git")
                .args(["-C", path_str, "rm", "--cached", "-r", "-f", "--", target])
                .output();
        }
    }

    // 2. Clean untracked files and directories
    let clean_output = Command::new("git")
        .args(["-C", path_str, "clean", "-fd", "--", target])
        .output()
        .map_err(|e| format!("git clean failed: {}", e))?;

    if !clean_output.status.success() {
        let stderr = String::from_utf8_lossy(&clean_output.stderr).trim().to_string();
        if !stderr.is_empty() {
            return Err(stderr);
        }
    }

    Ok(())
}

pub fn delete_path(path: &Path, target: &str) -> Result<(), String> {
    let full_path = path.join(target);
    let path_str = path.to_str().unwrap_or(".");

    if full_path.exists() {
        if full_path.is_dir() {
            std::fs::remove_dir_all(&full_path)
                .map_err(|e| format!("Failed to delete directory '{}': {}", target, e))?;
        } else {
            std::fs::remove_file(&full_path)
                .map_err(|e| format!("Failed to delete file '{}': {}", target, e))?;
        }
    } else {
        // If file doesn't exist on disk (already deleted in working tree), stage the deletion
        let _ = Command::new("git")
            .args(["-C", path_str, "add", "-A", "--", target])
            .output();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_discard_and_delete() {
        use std::fs;
        let temp_dir = std::env::temp_dir().join(format!("gitr_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        let _ = Command::new("git").args(["init", temp_dir.to_str().unwrap()]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "config", "user.name", "Test"]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "config", "user.email", "test@test.com"]).output();

        // 1. Initial commit
        let f1 = temp_dir.join("f1.txt");
        let _ = fs::write(&f1, "hello");
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "add", "f1.txt"]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "commit", "-m", "init"]).output();

        // 2. Modify f1.txt and create untracked f2.txt
        let _ = fs::write(&f1, "modified");
        let f2 = temp_dir.join("f2.txt");
        let _ = fs::write(&f2, "untracked");

        // Discard f2.txt (untracked)
        assert!(discard_changes(&temp_dir, "f2.txt").is_ok());
        assert!(!f2.exists());

        // Discard f1.txt (modified)
        assert!(discard_changes(&temp_dir, "f1.txt").is_ok());
        assert_eq!(fs::read_to_string(&f1).unwrap(), "hello");

        // 3. Delete f1.txt (tracked)
        assert!(delete_path(&temp_dir, "f1.txt").is_ok());
        assert!(!f1.exists());

        // Discard f1.txt to restore it
        assert!(discard_changes(&temp_dir, "f1.txt").is_ok());
        assert!(f1.exists());
        assert_eq!(fs::read_to_string(&f1).unwrap(), "hello");

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

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
        let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
        let diff = get_diff(current, &paths).unwrap();
        // Should succeed without error
        let _ = diff;
    }

    #[test]
    fn test_git_unpushed_count() {
        let current = Path::new(".");
        let count = get_unpushed_count(current);
        // Valid usize returned
        assert!(count < 100_000);
    }

    #[test]
    fn test_parse_numstat_path() {
        assert_eq!(parse_numstat_path("src/{old => new}/test.txt"), "src/new/test.txt");
        assert_eq!(parse_numstat_path("{old => new}/test.txt"), "new/test.txt");
        assert_eq!(parse_numstat_path("src/{old.txt => new.txt}"), "src/new.txt");
        assert_eq!(parse_numstat_path("old.txt => new.txt"), "new.txt");
        assert_eq!(parse_numstat_path("src/{dir/ => }test.txt"), "src/test.txt");
        assert_eq!(parse_numstat_path("normal/path/file.rs"), "normal/path/file.rs");
        assert_eq!(parse_numstat_path("\"file with spaces.txt\""), "file with spaces.txt");
    }

    #[test]
    fn test_count_file_lines() {
        use std::fs;
        let temp_dir = std::env::temp_dir().join(format!("gitr_lines_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        let f1 = temp_dir.join("f1.txt");
        fs::write(&f1, "a\nb\nc").unwrap();
        assert_eq!(count_file_lines(&f1), (3, 0));

        let f2 = temp_dir.join("f2.txt");
        fs::write(&f2, "hello\nworld\n").unwrap();
        assert_eq!(count_file_lines(&f2), (2, 0));

        let f_empty = temp_dir.join("empty.txt");
        fs::write(&f_empty, "").unwrap();
        assert_eq!(count_file_lines(&f_empty), (0, 0));

        let f_bin = temp_dir.join("bin.dat");
        fs::write(&f_bin, [0u8, 1, 2, 3]).unwrap();
        assert_eq!(count_file_lines(&f_bin), (0, 0));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_git_changes_line_counts() {
        use std::fs;
        let temp_dir = std::env::temp_dir().join(format!("gitr_changes_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        let _ = Command::new("git").args(["init", temp_dir.to_str().unwrap()]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "config", "user.name", "Test"]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "config", "user.email", "test@test.com"]).output();

        let f1 = temp_dir.join("tracked.txt");
        fs::write(&f1, "line1\nline2\nline3\n").unwrap();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "add", "tracked.txt"]).output();
        let _ = Command::new("git").args(["-C", temp_dir.to_str().unwrap(), "commit", "-m", "init"]).output();

        // Modify tracked.txt: delete line3, add line4 and line5
        fs::write(&f1, "line1\nline2\nline4\nline5\n").unwrap();

        // Create untracked file with 3 lines
        let f2 = temp_dir.join("untracked.txt");
        fs::write(&f2, "x\ny\nz\n").unwrap();

        let changes = get_changes(&temp_dir).unwrap();
        assert_eq!(changes.len(), 2);

        let tracked = changes.iter().find(|c| c.path == "tracked.txt").unwrap();
        assert_eq!(tracked.added, 2);
        assert_eq!(tracked.deleted, 1);

        let untracked = changes.iter().find(|c| c.path == "untracked.txt").unwrap();
        assert_eq!(untracked.added, 3);
        assert_eq!(untracked.deleted, 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_git_head_commit_hash() {
        let current = Path::new(".");
        let hash = get_head_commit_hash(current);
        assert!(hash.is_some());
        assert_eq!(hash.unwrap().len(), 40);
    }
}
