use crate::git::{ChangeItem, CommitItem};
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use std::collections::{BTreeMap, HashSet};
use std::io::{stdout, Write};
use std::panic;
use std::path::PathBuf;

pub struct TerminalGuard;

impl TerminalGuard {
    pub fn init() -> Self {
        enable_raw_mode().expect("Failed to enable raw mode");
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, Hide, EnableMouseCapture)
            .expect("Failed to init terminal");

        // Restore terminal on panic
        let default_panic = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let mut out = stdout();
            let _ = execute!(out, LeaveAlternateScreen, Show, DisableMouseCapture);
            let _ = disable_raw_mode();
            default_panic(panic_info);
        }));

        TerminalGuard
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show, DisableMouseCapture);
        let _ = disable_raw_mode();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Changes,
    History,
}

#[derive(Clone, Debug)]
pub enum TreeRow {
    Dir {
        name: String,
        full_path: String,
        depth: usize,
        expanded: bool,
        file_indices: Vec<usize>,
    },
    File {
        name: String,
        depth: usize,
        change_idx: usize,
    },
}

pub enum Modal {
    None,
    CommitReview {
        message: String,
        cursor: usize,
    },
    RepoSwitcher {
        input: String,
        selected_recent: usize,
    },
    Loading {
        title: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedPath {
    Dir(String),
    File(String),
}

pub struct App {
    pub repo_path: PathBuf,
    pub branch: String,
    pub unpushed_commits: usize,
    pub active_tab: Tab,
    pub changes: Vec<ChangeItem>,
    pub tree_rows: Vec<TreeRow>,
    pub selected_row: usize,
    pub collapsed_dirs: HashSet<String>,
    pub history: Vec<CommitItem>,
    pub selected_history: usize,
    pub modal: Modal,
    pub status: Option<(String, bool)>, // (message, is_error)
    pub spinner_frame: usize,
    pub recents: Vec<String>,
}

impl App {
    pub fn new(repo_path: PathBuf, branch: String, recents: Vec<String>) -> Self {
        Self {
            repo_path,
            branch,
            unpushed_commits: 0,
            active_tab: Tab::Changes,
            changes: Vec::new(),
            tree_rows: Vec::new(),
            selected_row: 0,
            collapsed_dirs: HashSet::new(),
            history: Vec::new(),
            selected_history: 0,
            modal: Modal::None,
            status: None,
            spinner_frame: 0,
            recents,
        }
    }

    pub fn set_status(&mut self, msg: &str, is_error: bool) {
        self.status = Some((msg.to_string(), is_error));
    }

    pub fn clear_status(&mut self) {
        self.status = None;
    }

    pub fn selected_files(&self) -> Vec<String> {
        self.changes
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.path.clone())
            .collect()
    }

    pub fn get_selected_tree_path(&self) -> Option<SelectedPath> {
        if self.selected_row < self.tree_rows.len() {
            match &self.tree_rows[self.selected_row] {
                TreeRow::Dir { full_path, .. } => Some(SelectedPath::Dir(full_path.clone())),
                TreeRow::File { change_idx, .. } => {
                    self.changes.get(*change_idx).map(|c| SelectedPath::File(c.path.clone()))
                }
            }
        } else {
            None
        }
    }

    pub fn restore_selected_tree_path(&mut self, target: &SelectedPath) {
        for (i, row) in self.tree_rows.iter().enumerate() {
            match (target, row) {
                (SelectedPath::Dir(prev_dir), TreeRow::Dir { full_path, .. }) => {
                    if prev_dir == full_path {
                        self.selected_row = i;
                        return;
                    }
                }
                (SelectedPath::File(prev_file), TreeRow::File { change_idx, .. }) => {
                    if let Some(c) = self.changes.get(*change_idx) {
                        if &c.path == prev_file {
                            self.selected_row = i;
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        if self.selected_row >= self.tree_rows.len() && !self.tree_rows.is_empty() {
            self.selected_row = self.tree_rows.len() - 1;
        }
    }

    pub fn update_changes(&mut self, new_changes: Vec<ChangeItem>) {
        let old_selected: std::collections::HashMap<String, bool> = self
            .changes
            .iter()
            .map(|c| (c.path.clone(), c.selected))
            .collect();

        let mut updated = new_changes;
        for item in &mut updated {
            if let Some(&sel) = old_selected.get(&item.path) {
                item.selected = sel;
            }
        }

        let prev_selected_path = self.get_selected_tree_path();

        self.changes = updated;
        self.rebuild_tree();

        if let Some(target) = prev_selected_path {
            self.restore_selected_tree_path(&target);
        }
    }

    pub fn rebuild_tree(&mut self) {
        self.tree_rows = build_tree_rows(&self.changes, &self.collapsed_dirs);
        if self.selected_row >= self.tree_rows.len() && !self.tree_rows.is_empty() {
            self.selected_row = self.tree_rows.len() - 1;
        }
    }

    pub fn toggle_current(&mut self) {
        if self.selected_row < self.tree_rows.len() {
            match &self.tree_rows[self.selected_row] {
                TreeRow::File { change_idx, .. } => {
                    let idx = *change_idx;
                    if idx < self.changes.len() {
                        self.changes[idx].selected = !self.changes[idx].selected;
                    }
                }
                TreeRow::Dir { file_indices, .. } => {
                    let any_unselected =
                        file_indices.iter().any(|&fi| !self.changes[fi].selected);
                    for &fi in file_indices {
                        self.changes[fi].selected = any_unselected;
                    }
                }
            }
        }
    }

    pub fn toggle_collapse_current(&mut self) {
        if self.selected_row < self.tree_rows.len() {
            if let TreeRow::Dir {
                full_path, expanded, ..
            } = &self.tree_rows[self.selected_row]
            {
                if *expanded {
                    self.collapsed_dirs.insert(full_path.clone());
                } else {
                    self.collapsed_dirs.remove(full_path);
                }
                self.rebuild_tree();
            }
        }
    }

    pub fn toggle_all(&mut self) {
        let any_unselected = self.changes.iter().any(|c| !c.selected);
        for item in &mut self.changes {
            item.selected = any_unselected;
        }
    }
}

// Hierarchical directory tree builder
struct DirNode {
    name: String,
    full_path: String,
    subdirs: BTreeMap<String, DirNode>,
    files: Vec<(String, usize)>, // (file_name, change_idx)
}

impl DirNode {
    fn new(name: String, full_path: String) -> Self {
        Self {
            name,
            full_path,
            subdirs: BTreeMap::new(),
            files: Vec::new(),
        }
    }

    fn collect_file_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        for (_, idx) in &self.files {
            indices.push(*idx);
        }
        for (_, sub) in &self.subdirs {
            indices.extend(sub.collect_file_indices());
        }
        indices
    }
}

pub fn build_tree_rows(
    changes: &[ChangeItem],
    collapsed_dirs: &HashSet<String>,
) -> Vec<TreeRow> {
    let mut root = DirNode::new("".to_string(), "".to_string());

    for (idx, item) in changes.iter().enumerate() {
        let parts: Vec<&str> = item.path.split('/').collect();
        if parts.len() <= 1 {
            root.files.push((item.path.clone(), idx));
        } else {
            let mut current = &mut root;
            let mut current_path = String::new();
            for i in 0..parts.len() - 1 {
                if !current_path.is_empty() {
                    current_path.push('/');
                }
                current_path.push_str(parts[i]);

                let part_name = parts[i].to_string();
                let path_so_far = current_path.clone();
                current = current
                    .subdirs
                    .entry(part_name.clone())
                    .or_insert_with(|| DirNode::new(part_name, path_so_far));
            }
            let file_name = parts[parts.len() - 1].to_string();
            current.files.push((file_name, idx));
        }
    }

    let mut rows = Vec::new();
    flatten_dir(&root, 0, collapsed_dirs, &mut rows, true);
    rows
}

fn flatten_dir(
    dir: &DirNode,
    depth: usize,
    collapsed_dirs: &HashSet<String>,
    rows: &mut Vec<TreeRow>,
    is_root: bool,
) {
    if !is_root {
        let is_expanded = !collapsed_dirs.contains(&dir.full_path);
        let file_indices = dir.collect_file_indices();
        rows.push(TreeRow::Dir {
            name: format!("{}/", dir.name),
            full_path: dir.full_path.clone(),
            depth,
            expanded: is_expanded,
            file_indices,
        });

        if !is_expanded {
            return;
        }
    }

    let next_depth = if is_root { 0 } else { depth + 1 };

    // Group subdirectories first
    for (_, sub) in &dir.subdirs {
        flatten_dir(sub, next_depth, collapsed_dirs, rows, false);
    }

    // Then files in this folder
    for (file_name, change_idx) in &dir.files {
        rows.push(TreeRow::File {
            name: file_name.clone(),
            depth: next_depth,
            change_idx: *change_idx,
        });
    }
}

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render(app: &App) {
    let (width, height) = size().unwrap_or((100, 30));
    let mut out = stdout();

    let mut lines: Vec<String> = Vec::with_capacity(height as usize);

    let w = width as usize;
    let h = height as usize;

    // Header
    let path_str = app.repo_path.to_str().unwrap_or("?");
    let unpushed_badge = if app.unpushed_commits > 0 {
        format!(" ↑{}", app.unpushed_commits)
    } else {
        String::new()
    };
    let header_content = format!(
        " gitr  │  branch: [{}{}]  │  {} ",
        app.branch, unpushed_badge, path_str
    );
    let header_line = truncate_pad(&header_content, w);
    lines.push(format!("\x1b[7m\x1b[1m{}\x1b[0m", header_line));

    // Tab Bar
    let selected_count = app.changes.iter().filter(|c| c.selected).count();
    let total_count = app.changes.len();

    let tab_changes_str = format!(" [1] Changes ({}/{}) ", selected_count, total_count);
    let tab_history_str = " [2] History ";

    let (c_style, h_style) = match app.active_tab {
        Tab::Changes => ("\x1b[1m\x1b[36m", "\x1b[2m"),
        Tab::History => ("\x1b[2m", "\x1b[1m\x1b[36m"),
    };

    let tab_line_raw = format!(
        "{}{}\x1b[0m │ {}{}\x1b[0m",
        c_style, tab_changes_str, h_style, tab_history_str
    );
    let visible_tab_len = tab_changes_str.len() + 3 + tab_history_str.len();
    let tab_padding = if w > visible_tab_len {
        " ".repeat(w - visible_tab_len)
    } else {
        String::new()
    };
    lines.push(format!("{}{}", tab_line_raw, tab_padding));

    // Separator
    lines.push("\x1b[2m".to_string() + &"─".repeat(w) + "\x1b[0m");

    // Content Height = h - 6
    let content_height = if h > 6 { h - 6 } else { 1 };

    match app.active_tab {
        Tab::Changes => {
            if app.tree_rows.is_empty() {
                lines.push(truncate_pad(
                    "   (No changes detected. Working tree clean)",
                    w,
                ));
                for _ in 1..content_height {
                    lines.push(" ".repeat(w));
                }
            } else {
                let scroll =
                    calculate_scroll(app.selected_row, app.tree_rows.len(), content_height);
                for i in 0..content_height {
                    let idx = scroll + i;
                    if idx < app.tree_rows.len() {
                        let item = &app.tree_rows[idx];
                        let is_cursor = idx == app.selected_row;
                        let cursor_mark = if is_cursor { ">" } else { " " };

                        let (row_content, clean_len) = match item {
                            TreeRow::Dir {
                                name,
                                depth,
                                expanded,
                                file_indices,
                                ..
                            } => {
                                let all_sel = file_indices
                                    .iter()
                                    .all(|&fi| app.changes[fi].selected);
                                let any_sel = file_indices
                                    .iter()
                                    .any(|&fi| app.changes[fi].selected);

                                let check_mark = if all_sel {
                                    "\x1b[32m[x]\x1b[0m"
                                } else if any_sel {
                                    "\x1b[33m[-]\x1b[0m"
                                } else {
                                    "\x1b[90m[ ]\x1b[0m"
                                };

                                let arrow = if *expanded { "▾" } else { "▸" };
                                let indent = "  ".repeat(*depth);
                                let count_note = if !*expanded {
                                    format!(" ({} files)", file_indices.len())
                                } else {
                                    String::new()
                                };

                                let content = format!(
                                    " {} {}{} {} \x1b[1m\x1b[34m{}\x1b[0m\x1b[2m{}\x1b[0m",
                                    cursor_mark, indent, check_mark, arrow, name, count_note
                                );
                                let plain_len = 1
                                    + 1
                                    + indent.len()
                                    + 3
                                    + 1
                                    + 1
                                    + 1
                                    + name.len()
                                    + count_note.len();
                                (content, plain_len)
                            }
                            TreeRow::File {
                                name,
                                depth,
                                change_idx,
                            } => {
                                let change = &app.changes[*change_idx];
                                let check_mark = if change.selected {
                                    "\x1b[32m[x]\x1b[0m"
                                } else {
                                    "\x1b[90m[ ]\x1b[0m"
                                };

                                let status_tag = match change.status.as_str() {
                                    "M" | " M" | "M " => "\x1b[33mM\x1b[0m",
                                    "?" | "??" => "\x1b[32m?\x1b[0m",
                                    "D" | " D" | "D " => "\x1b[31mD\x1b[0m",
                                    "A" | "A " => "\x1b[32mA\x1b[0m",
                                    "R" | "R " => "\x1b[36mR\x1b[0m",
                                    other => other,
                                };

                                let indent = "  ".repeat(*depth);
                                let content = format!(
                                    " {} {}{} {}  {}",
                                    cursor_mark, indent, check_mark, status_tag, name
                                );
                                let plain_len = 1
                                    + 1
                                    + indent.len()
                                    + 3
                                    + 1
                                    + change.status.len()
                                    + 2
                                    + name.len();
                                (content, plain_len)
                            }
                        };

                        let pad = if w > clean_len {
                            " ".repeat(w - clean_len)
                        } else {
                            String::new()
                        };

                        if is_cursor {
                            lines.push(format!("\x1b[48;5;236m{}{}\x1b[0m", row_content, pad));
                        } else {
                            lines.push(format!("{}{}", row_content, pad));
                        }
                    } else {
                        lines.push(" ".repeat(w));
                    }
                }
            }
        }
        Tab::History => {
            if app.history.is_empty() {
                lines.push(truncate_pad("   (No commit history found)", w));
                for _ in 1..content_height {
                    lines.push(" ".repeat(w));
                }
            } else {
                let scroll =
                    calculate_scroll(app.selected_history, app.history.len(), content_height);
                for i in 0..content_height {
                    let idx = scroll + i;
                    if idx < app.history.len() {
                        let item = &app.history[idx];
                        let is_cursor = idx == app.selected_history;
                        let cursor_mark = if is_cursor { ">" } else { " " };

                        let row_raw = format!(
                            " {} \x1b[33m{:<7}\x1b[0m  \x1b[36m{:<12}\x1b[0m  \x1b[32m{:<14}\x1b[0m  {}",
                            cursor_mark,
                            truncate_pad(&item.hash, 7),
                            truncate_pad(&item.relative_time, 12),
                            truncate_pad(&item.author, 14),
                            item.summary
                        );
                        let clean_len =
                            1 + 1 + 7 + 2 + 12 + 2 + 14 + 2 + item.summary.len();
                        let pad = if w > clean_len {
                            " ".repeat(w - clean_len)
                        } else {
                            String::new()
                        };

                        if is_cursor {
                            lines.push(format!("\x1b[48;5;236m{}{}\x1b[0m", row_raw, pad));
                        } else {
                            lines.push(format!("{}{}", row_raw, pad));
                        }
                    } else {
                        lines.push(" ".repeat(w));
                    }
                }
            }
        }
    }

    // Bottom separator
    lines.push("\x1b[2m".to_string() + &"─".repeat(w) + "\x1b[0m");

    // Footer Line 1: Key hints
    let push_hint = if app.unpushed_commits > 0 {
        format!(
            "\x1b[1m\x1b[33m[p] Push ({} unpushed)\x1b[0m\x1b[2m",
            app.unpushed_commits
        )
    } else {
        "[p] Push (0)".to_string()
    };

    let footer_hints = match app.active_tab {
        Tab::Changes => format!(
            " [Space] Toggle │ [Enter] Fold │ [a] All │ [c] Commit │ [C] Quick │ {} │ [o] Repo │ [1/2] Tabs │ [q] Quit",
            push_hint
        ),
        Tab::History => format!(
            " [j/k] Navigate │ {} │ [o] Repo │ [r] Refresh │ [1/2] Tabs │ [q] Quit",
            push_hint
        ),
    };
    lines.push(pad_or_truncate_ansi(&format!("\x1b[2m{}\x1b[0m", footer_hints), w));

    // Footer Line 2: Status
    let status_line = if let Some((msg, is_err)) = &app.status {
        if *is_err {
            format!("\x1b[31m ✖ {}\x1b[0m", msg)
        } else {
            format!("\x1b[32m ✔ {}\x1b[0m", msg)
        }
    } else {
        "\x1b[90m Ready\x1b[0m".to_string()
    };
    lines.push(truncate_pad(&status_line, w));

    // Overlays if active modal
    overlay_modal(&mut lines, app, w, h);

    // Single atomic flush output
    let mut buffer = String::with_capacity(lines.len() * 128);
    // Begin synchronized update (prevents tearing/flickering)
    buffer.push_str("\x1b[?2025h");

    for (i, line) in lines.iter().enumerate() {
        if i >= h {
            break;
        }
        // Jump directly to row i+1, column 1 without using \r\n (which causes terminal scrolling)
        buffer.push_str(&format!("\x1b[{};1H{}\x1b[K", i + 1, line));
    }

    // End synchronized update
    buffer.push_str("\x1b[?2025l");

    let _ = out.write_all(buffer.as_bytes());
    let _ = out.flush();
}

fn calculate_scroll(selected: usize, total: usize, viewport_height: usize) -> usize {
    if total <= viewport_height {
        return 0;
    }
    if selected < viewport_height / 2 {
        0
    } else if selected + viewport_height / 2 >= total {
        total - viewport_height
    } else {
        selected - viewport_height / 2
    }
}

fn truncate_pad(s: &str, target_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count >= target_len {
        s.chars().take(target_len).collect()
    } else {
        let mut res = s.to_string();
        res.push_str(&" ".repeat(target_len - char_count));
        res
    }
}

fn overlay_modal(lines: &mut [String], app: &App, width: usize, height: usize) {
    match &app.modal {
        Modal::None => {}
        Modal::Loading { title } => {
            let box_w = 46.min(width.saturating_sub(4));
            let box_h = 5;
            let start_x = (width.saturating_sub(box_w)) / 2;
            let start_y = (height.saturating_sub(box_h)) / 2;

            let spinner_char = SPINNER[app.spinner_frame % SPINNER.len()];
            let content = format!(" {}  {}", spinner_char, title);

            draw_box(lines, start_x, start_y, box_w, box_h, " Please Wait ");
            draw_text_in_box(lines, start_x + 2, start_y + 2, box_w - 4, &content);
        }
        Modal::CommitReview { message, cursor } => {
            let box_w = 76.min(width.saturating_sub(4));
            let box_h = 14.min(height.saturating_sub(4));
            let start_x = (width.saturating_sub(box_w)) / 2;
            let start_y = (height.saturating_sub(box_h)) / 2;

            draw_box(
                lines,
                start_x,
                start_y,
                box_w,
                box_h,
                " Review Commit Message (Enter to commit, Esc to cancel) ",
            );

            let inner_w = box_w.saturating_sub(4);
            let inner_h = box_h.saturating_sub(4);

            let mut msg_with_cursor = message.clone();
            let c_pos = (*cursor).min(msg_with_cursor.len());
            msg_with_cursor.insert(c_pos, '│');

            let mut row = 0;
            for line in msg_with_cursor.lines() {
                if row >= inner_h {
                    break;
                }
                draw_text_in_box(lines, start_x + 2, start_y + 2 + row, inner_w, line);
                row += 1;
            }

            let tip = "[Enter] Commit   [Esc] Cancel   [Type/Backspace] Edit";
            draw_text_in_box(
                lines,
                start_x + 2,
                start_y + box_h - 2,
                inner_w,
                &format!("\x1b[2m{}\x1b[0m", tip),
            );
        }
        Modal::RepoSwitcher {
            input,
            selected_recent,
        } => {
            let box_w = 68.min(width.saturating_sub(4));
            let box_h = 14.min(height.saturating_sub(4));
            let start_x = (width.saturating_sub(box_w)) / 2;
            let start_y = (height.saturating_sub(box_h)) / 2;

            draw_box(lines, start_x, start_y, box_w, box_h, " Switch Project Repository ");

            let inner_w = box_w.saturating_sub(4);
            let input_line = format!("Path: {}\x1b[7m \x1b[0m", input);
            draw_text_in_box(lines, start_x + 2, start_y + 2, inner_w, &input_line);
            let separator = format!("\x1b[2m{}\x1b[0m", "─".repeat(inner_w));
            draw_text_in_box(lines, start_x + 2, start_y + 3, inner_w, &separator);
            draw_text_in_box(
                lines,
                start_x + 2,
                start_y + 4,
                inner_w,
                "\x1b[1mRecent Repositories:\x1b[0m",
            );

            let max_recents = box_h.saturating_sub(8);
            for (idx, r) in app.recents.iter().take(max_recents).enumerate() {
                let is_sel = idx == *selected_recent;
                let prefix = if is_sel { "\x1b[36m> " } else { "  " };
                let suffix = if is_sel { "\x1b[0m" } else { "" };
                let item_str = format!("{}{}{}", prefix, r, suffix);
                draw_text_in_box(lines, start_x + 2, start_y + 5 + idx, inner_w, &item_str);
            }

            let hint = "[Enter] Open   [Tab] Complete Path   [j/k] Select Recent   [Esc] Cancel";
            draw_text_in_box(
                lines,
                start_x + 2,
                start_y + box_h - 2,
                inner_w,
                &format!("\x1b[2m{}\x1b[0m", hint),
            );
        }
    }
}

fn draw_box(lines: &mut [String], x: usize, y: usize, w: usize, h: usize, title: &str) {
    if y >= lines.len() || w < 4 || h < 3 {
        return;
    }

    let title_clean = if title.len() + 4 > w {
        &title[..w - 4]
    } else {
        title
    };
    let right_border_len = w.saturating_sub(3 + title_clean.len());
    let top_line = format!(
        "┌─\x1b[1m{}\x1b[0m{}┐",
        title_clean,
        "─".repeat(right_border_len)
    );
    embed_in_line(lines, y, x, w, &top_line);

    for i in 1..h - 1 {
        let mid = format!("│{}│", " ".repeat(w.saturating_sub(2)));
        embed_in_line(lines, y + i, x, w, &mid);
    }

    let bot_line = format!("└{}┘", "─".repeat(w.saturating_sub(2)));
    embed_in_line(lines, y + h - 1, x, w, &bot_line);
}

fn draw_text_in_box(lines: &mut [String], x: usize, y: usize, max_len: usize, text: &str) {
    embed_in_line(lines, y, x, max_len, text);
}

fn embed_in_line(lines: &mut [String], y: usize, x: usize, target_w: usize, content: &str) {
    if y >= lines.len() {
        return;
    }

    let current = &lines[y];
    let current_plain = strip_ansi_len(current);
    if current_plain < x + target_w {
        let diff = (x + target_w).saturating_sub(current_plain);
        lines[y].push_str(&" ".repeat(diff));
    }

    let cursor_jump = format!("\x1b[{}G{}", x + 1, content);
    lines[y].push_str(&cursor_jump);
}

pub fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

pub fn pad_or_truncate_ansi(s: &str, target_w: usize) -> String {
    let visible_len = strip_ansi_len(s);
    if visible_len < target_w {
        let mut res = s.to_string();
        res.push_str(&" ".repeat(target_w - visible_len));
        res
    } else if visible_len == target_w {
        s.to_string()
    } else {
        truncate_ansi(s, target_w)
    }
}

pub fn truncate_ansi(s: &str, max_visible: usize) -> String {
    let mut res = String::new();
    let mut visible_count = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            res.push(c);
        } else if in_escape {
            res.push(c);
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if visible_count < max_visible {
            visible_count += 1;
            res.push(c);
        }
    }
    res.push_str("\x1b[0m");
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_and_pad() {
        let text = "\x1b[1m\x1b[33m[p] Push (2 unpushed)\x1b[0m";
        let visible_len = strip_ansi_len(text);
        assert_eq!(visible_len, 21);

        let padded = pad_or_truncate_ansi(text, 25);
        assert_eq!(strip_ansi_len(&padded), 25);

        let truncated = pad_or_truncate_ansi(text, 10);
        assert_eq!(strip_ansi_len(&truncated), 10);
    }

    #[test]
    fn test_update_changes_preserves_selection_and_cursor() {
        let mut app = App::new(PathBuf::from("."), "master".to_string(), Vec::new());
        let initial_changes = vec![
            ChangeItem {
                path: "src/main.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
            ChangeItem {
                path: "src/ui.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
        ];
        app.changes = initial_changes;
        app.rebuild_tree();

        // Cursor on ui.rs (row 2: src/ is 0, main.rs is 1, ui.rs is 2)
        app.selected_row = 2;
        // User unselects ui.rs
        app.changes[1].selected = false;

        // New changes arrived with an added file
        let new_changes = vec![
            ChangeItem {
                path: "Cargo.toml".to_string(),
                status: "M".to_string(),
                selected: true,
            },
            ChangeItem {
                path: "src/main.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
            ChangeItem {
                path: "src/ui.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
        ];

        app.update_changes(new_changes);

        // ui.rs must stay unselected!
        let ui_change = app.changes.iter().find(|c| c.path == "src/ui.rs").unwrap();
        assert_eq!(ui_change.selected, false);

        // Cursor must still point to ui.rs!
        let selected_row_item = &app.tree_rows[app.selected_row];
        match selected_row_item {
            TreeRow::File { change_idx, .. } => {
                assert_eq!(app.changes[*change_idx].path, "src/ui.rs");
            }
            _ => panic!("Expected cursor to stay on ui.rs"),
        }
    }

    #[test]
    fn test_tree_rows_nesting() {
        let changes = vec![
            ChangeItem {
                path: "src/main.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
            ChangeItem {
                path: "src/ui.rs".to_string(),
                status: "M".to_string(),
                selected: true,
            },
            ChangeItem {
                path: "Cargo.toml".to_string(),
                status: "M".to_string(),
                selected: true,
            },
        ];

        let collapsed = HashSet::new();
        let rows = build_tree_rows(&changes, &collapsed);

        // We expect:
        // 1. Dir "src/" (depth 0)
        // 2. File "main.rs" (depth 1)
        // 3. File "ui.rs" (depth 1)
        // 4. File "Cargo.toml" (depth 0)
        assert_eq!(rows.len(), 4);

        match &rows[0] {
            TreeRow::Dir { name, depth, .. } => {
                assert_eq!(name, "src/");
                assert_eq!(*depth, 0);
            }
            _ => panic!("Expected Dir row at index 0"),
        }

        match &rows[1] {
            TreeRow::File { name, depth, .. } => {
                assert_eq!(name, "main.rs");
                assert_eq!(*depth, 1);
            }
            _ => panic!("Expected File row at index 1"),
        }

        match &rows[2] {
            TreeRow::File { name, depth, .. } => {
                assert_eq!(name, "ui.rs");
                assert_eq!(*depth, 1);
            }
            _ => panic!("Expected File row at index 2"),
        }

        match &rows[3] {
            TreeRow::File { name, depth, .. } => {
                assert_eq!(name, "Cargo.toml");
                assert_eq!(*depth, 0);
            }
            _ => panic!("Expected File row at index 3"),
        }
    }

    #[test]
    fn test_push_hint_formatting() {
        let mut app = App::new(PathBuf::from("."), "master".to_string(), Vec::new());
        app.unpushed_commits = 0;
        let push_hint_zero = if app.unpushed_commits > 0 {
            format!("\x1b[1m\x1b[33m[p] Push ({} unpushed)\x1b[0m\x1b[2m", app.unpushed_commits)
        } else {
            "[p] Push (0)".to_string()
        };
        assert_eq!(push_hint_zero, "[p] Push (0)");

        app.unpushed_commits = 3;
        let push_hint_three = if app.unpushed_commits > 0 {
            format!("\x1b[1m\x1b[33m[p] Push ({} unpushed)\x1b[0m\x1b[2m", app.unpushed_commits)
        } else {
            "[p] Push (0)".to_string()
        };
        assert!(push_hint_three.contains("[p] Push (3 unpushed)"));
        assert_eq!(strip_ansi_len(&push_hint_three), "[p] Push (3 unpushed)".len());
    }
}
