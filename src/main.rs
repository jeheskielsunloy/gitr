mod ai;
mod config;
mod git;
mod ui;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use ui::{App, ConfirmActionType, Modal, Tab, TerminalGuard};

enum BgEvent {
    AiCommitDone(Result<String, String>, bool /* is_quick */),
    PushDone(Result<String, String>),
    RepoStateChanged {
        path: PathBuf,
        branch: String,
        changes: Vec<git::ChangeItem>,
        unpushed: usize,
        history: Vec<git::CommitItem>,
    },
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--help" | "-h" => {
                println!(
                    "gitr - Ultra-lean Git TUI for AI commit messages and pushing\n\n\
                    USAGE:\n    gitr [PATH]\n\n\
                    KEYS:\n    \
                    [1/2 / Tab]    Switch tabs (Changes / History)\n    \
                    [Space]        Toggle file selection\n    \
                    [Enter/h/l]    Fold/expand directory\n    \
                    [a]            Toggle select all\n    \
                    [d]            Discard changes of file/dir\n    \
                    [x / Del]      Delete file/dir\n    \
                    [c]            Generate AI commit message and review\n    \
                    [C]            Quick commit with AI (no review, no push)\n    \
                    [p]            Git push to remote\n    \
                    [o]            Open / switch repository\n    \
                    [r]            Refresh status\n    \
                    [q]            Quit"
                );
                return;
            }
            "--version" | "-v" => {
                println!("gitr v0.1.0");
                return;
            }
            _ => {}
        }
    }

    let initial_path = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };

    let config = config::load_config();
    let recents = config::load_recents();

    let mut app = if git::is_git_repo(&initial_path) {
        let root = git::get_repo_root(&initial_path).unwrap_or(initial_path);
        config::save_recent(root.to_str().unwrap_or("."));
        let branch = git::get_branch(&root);
        let mut a = App::new(root.clone(), branch, recents);
        refresh_repo_state(&mut a);
        a
    } else {
        let mut a = App::new(initial_path, "none".to_string(), recents);
        a.modal = Modal::RepoSwitcher {
            input: String::new(),
            selected_recent: 0,
        };
        a.set_status("Not a git repository. Open or enter a repository path.", true);
        a
    };

    let shared_repo_path = Arc::new(Mutex::new(app.repo_path.clone()));
    let is_modal_busy = Arc::new(AtomicBool::new(false));

    let _guard = TerminalGuard::init();
    let (tx, rx): (Sender<BgEvent>, Receiver<BgEvent>) = channel();

    start_repo_watcher(
        shared_repo_path.clone(),
        is_modal_busy.clone(),
        tx.clone(),
    );

    run_event_loop(&mut app, &config, tx, rx, shared_repo_path, is_modal_busy);
}

fn start_repo_watcher(
    shared_path: Arc<Mutex<PathBuf>>,
    busy: Arc<AtomicBool>,
    tx: Sender<BgEvent>,
) {
    thread::spawn(move || {
        let mut last_repo = PathBuf::new();
        let mut last_fingerprint = String::new();

        loop {
            thread::sleep(Duration::from_millis(800));

            if busy.load(Ordering::Relaxed) {
                continue;
            }

            let repo_path = match shared_path.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => continue,
            };

            if !git::is_git_repo(&repo_path) {
                continue;
            }

            let path_changed = repo_path != last_repo;

            let changes = match git::get_changes(&repo_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let branch = git::get_branch(&repo_path);
            let unpushed = git::get_unpushed_count(&repo_path);
            let head_commit = git::get_head_commit_hash(&repo_path);

            let fingerprint = format!(
                "{}:{}:{}:{}",
                branch,
                unpushed,
                head_commit.as_deref().unwrap_or(""),
                changes
                    .iter()
                    .map(|c| format!("{},{},{},{}", c.path, c.status, c.added, c.deleted))
                    .collect::<Vec<_>>()
                    .join(";")
            );

            if path_changed || fingerprint != last_fingerprint {
                last_repo = repo_path.clone();
                last_fingerprint = fingerprint;

                let history = git::get_history(&repo_path, 50).unwrap_or_default();
                let _ = tx.send(BgEvent::RepoStateChanged {
                    path: repo_path,
                    branch,
                    changes,
                    unpushed,
                    history,
                });
            }
        }
    });
}

fn refresh_repo_state(app: &mut App) {
    if git::is_git_repo(&app.repo_path) {
        app.branch = git::get_branch(&app.repo_path);
        app.unpushed_commits = git::get_unpushed_count(&app.repo_path);
        if let Ok(changes) = git::get_changes(&app.repo_path) {
            app.update_changes(changes);
        }
        if let Ok(hist) = git::get_history(&app.repo_path, 50) {
            app.history = hist;
            if app.selected_history >= app.history.len() && !app.history.is_empty() {
                app.selected_history = app.history.len() - 1;
            }
        }
    }
}

fn run_event_loop(
    app: &mut App,
    config: &config::Config,
    tx: Sender<BgEvent>,
    rx: Receiver<BgEvent>,
    shared_repo_path: Arc<Mutex<PathBuf>>,
    is_modal_busy: Arc<AtomicBool>,
) {
    let mut needs_render = true;

    loop {
        is_modal_busy.store(!matches!(app.modal, Modal::None), Ordering::Relaxed);

        // Poll for background worker events
        while let Ok(event) = rx.try_recv() {
            needs_render = true;
            match event {
                BgEvent::RepoStateChanged {
                    path,
                    branch,
                    changes,
                    unpushed,
                    history,
                } => {
                    if path == app.repo_path && matches!(app.modal, Modal::None) {
                        app.branch = branch;
                        app.unpushed_commits = unpushed;
                        app.update_changes(changes);
                        app.history = history;
                        if app.selected_history >= app.history.len() && !app.history.is_empty() {
                            app.selected_history = app.history.len() - 1;
                        }
                    }
                }
                BgEvent::AiCommitDone(res, is_quick) => match res {
                    Ok(msg) => {
                        if is_quick {
                            let files = app.selected_files();
                            match git::commit(&app.repo_path, &files, &msg) {
                                Ok(_) => {
                                    let first_line = msg.lines().next().unwrap_or("committed");
                                    app.set_status(&format!("Committed: {}", first_line), false);
                                    refresh_repo_state(app);
                                }
                                Err(e) => {
                                    app.set_status(&format!("Commit failed: {}", e), true);
                                }
                            }
                            app.modal = Modal::None;
                        } else {
                            let len = msg.len();
                            app.modal = Modal::CommitReview {
                                message: msg,
                                cursor: len,
                            };
                        }
                    }
                    Err(e) => {
                        app.modal = Modal::None;
                        app.set_status(&format!("AI Error: {}", e), true);
                    }
                },
                BgEvent::PushDone(res) => {
                    app.modal = Modal::None;
                    match res {
                        Ok(msg) => {
                            let first_line = msg.lines().next().unwrap_or("Pushed successfully");
                            app.set_status(&format!("Push: {}", first_line), false);
                            refresh_repo_state(app);
                        }
                        Err(e) => {
                            let first_line = e.lines().next().unwrap_or("Failed to push");
                            app.set_status(&format!("Push error: {}", first_line), true);
                        }
                    }
                }
            }
        }

        // Render frame only when state changed
        if needs_render {
            ui::render(app);
            needs_render = false;
        }

        let is_loading = matches!(&app.modal, Modal::Loading { .. });
        let timeout = if is_loading {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(200)
        };

        if event::poll(timeout).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(key)) => {
                    if key.kind == KeyEventKind::Press {
                        needs_render = true;
                        if handle_key_event(app, config, &tx, &shared_repo_path, key.code, key.modifiers) {
                            break;
                        }
                    }
                }
                Ok(Event::Resize(_, _)) => {
                    needs_render = true;
                }
                _ => {}
            }
        } else if is_loading {
            app.spinner_frame = (app.spinner_frame + 1) % 10;
            needs_render = true;
        }
    }
}

fn handle_key_event(
    app: &mut App,
    config: &config::Config,
    tx: &Sender<BgEvent>,
    shared_repo_path: &Arc<Mutex<PathBuf>>,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    // Global quit on Ctrl+C
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return true;
    }

    match &mut app.modal {
        Modal::Loading { .. } => {
            if code == KeyCode::Esc || code == KeyCode::Char('q') {
                app.modal = Modal::None;
                app.set_status("Cancelled operation", false);
            }
        }
        Modal::CommitReview { message, cursor } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Enter => {
                let msg_to_commit = message.trim().to_string();
                if msg_to_commit.is_empty() {
                    app.set_status("Cannot commit empty message", true);
                } else {
                    let files = app.selected_files();
                    match git::commit(&app.repo_path, &files, &msg_to_commit) {
                        Ok(_) => {
                            let first = msg_to_commit.lines().next().unwrap_or("committed");
                            app.set_status(&format!("Committed: {}", first), false);
                            app.modal = Modal::None;
                            refresh_repo_state(app);
                        }
                        Err(e) => {
                            let first = e.lines().next().unwrap_or("Commit failed");
                            app.set_status(&format!("Commit failed: {}", first), true);
                        }
                    }
                }
            }
            KeyCode::Left => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
            KeyCode::Right => {
                if *cursor < message.len() {
                    *cursor += 1;
                }
            }
            KeyCode::Home => {
                *cursor = 0;
            }
            KeyCode::End => {
                *cursor = message.len();
            }
            KeyCode::Backspace => {
                if *cursor > 0 && *cursor <= message.len() {
                    message.remove(*cursor - 1);
                    *cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if *cursor < message.len() {
                    message.remove(*cursor);
                }
            }
            KeyCode::Char(c) => {
                if *cursor <= message.len() {
                    message.insert(*cursor, c);
                    *cursor += 1;
                }
            }
            _ => {}
        },
        Modal::RepoSwitcher {
            input,
            selected_recent,
        } => match code {
            KeyCode::Esc => {
                if git::is_git_repo(&app.repo_path) {
                    app.modal = Modal::None;
                }
            }
            KeyCode::Down | KeyCode::Char('j')
                if !modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
            {
                if !app.recents.is_empty() && *selected_recent + 1 < app.recents.len() {
                    *selected_recent += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k')
                if !modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
            {
                if *selected_recent > 0 {
                    *selected_recent -= 1;
                }
            }
            KeyCode::Tab => {
                if !input.is_empty() {
                    let completed = autocomplete_path(input);
                    *input = completed;
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => {
                input.push(c);
            }
            KeyCode::Enter => {
                let target_str = if !input.trim().is_empty() {
                    resolve_tilde(input.trim())
                } else if !app.recents.is_empty() && *selected_recent < app.recents.len() {
                    app.recents[*selected_recent].clone()
                } else {
                    String::new()
                };

                if !target_str.is_empty() {
                    let target_path = PathBuf::from(&target_str);
                    if git::is_git_repo(&target_path) {
                        let root = git::get_repo_root(&target_path).unwrap_or(target_path);
                        config::save_recent(root.to_str().unwrap_or("."));
                        app.repo_path = root.clone();
                        if let Ok(mut guard) = shared_repo_path.lock() {
                            *guard = root;
                        }
                        app.recents = config::load_recents();
                        app.modal = Modal::None;
                        app.clear_status();
                        refresh_repo_state(app);
                    } else {
                        app.set_status(&format!("'{}' is not a git repository", target_str), true);
                    }
                }
            }
            _ => {}
        },
        Modal::ConfirmAction {
            action,
            target_path,
            is_dir: _,
            file_count: _,
        } => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let action_type = *action;
                let path = target_path.clone();
                app.modal = Modal::None;
                match action_type {
                    ConfirmActionType::Discard => {
                        match git::discard_changes(&app.repo_path, &path) {
                            Ok(_) => {
                                app.set_status(&format!("Discarded changes in {}", path), false);
                                refresh_repo_state(app);
                            }
                            Err(e) => {
                                app.set_status(&format!("Discard failed: {}", e), true);
                            }
                        }
                    }
                    ConfirmActionType::Delete => {
                        match git::delete_path(&app.repo_path, &path) {
                            Ok(_) => {
                                app.set_status(&format!("Deleted {}", path), false);
                                refresh_repo_state(app);
                            }
                            Err(e) => {
                                app.set_status(&format!("Delete failed: {}", e), true);
                            }
                        }
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Operation cancelled", false);
            }
            _ => {}
        },
        Modal::None => match code {
            KeyCode::Char('q') => {
                return true;
            }
            KeyCode::Tab => {
                app.active_tab = match app.active_tab {
                    Tab::Changes => Tab::History,
                    Tab::History => Tab::Changes,
                };
            }
            KeyCode::Char('1') => {
                app.active_tab = Tab::Changes;
            }
            KeyCode::Char('2') => {
                app.active_tab = Tab::History;
            }
            KeyCode::Char('r') => {
                refresh_repo_state(app);
                app.set_status("Refreshed git status", false);
            }
            KeyCode::Char('o') => {
                app.modal = Modal::RepoSwitcher {
                    input: String::new(),
                    selected_recent: 0,
                };
            }
            KeyCode::Char('p') => {
                let path = app.repo_path.clone();
                let tx_clone = tx.clone();
                app.modal = Modal::Loading {
                    title: "Pushing commits to remote...".to_string(),
                };
                thread::spawn(move || {
                    let res = git::push(&path);
                    let _ = tx_clone.send(BgEvent::PushDone(res));
                });
            }
            KeyCode::Down | KeyCode::Char('j') => match app.active_tab {
                Tab::Changes => {
                    if !app.tree_rows.is_empty() && app.selected_row + 1 < app.tree_rows.len() {
                        app.selected_row += 1;
                    }
                }
                Tab::History => {
                    if !app.history.is_empty() && app.selected_history + 1 < app.history.len() {
                        app.selected_history += 1;
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match app.active_tab {
                Tab::Changes => {
                    if app.selected_row > 0 {
                        app.selected_row -= 1;
                    }
                }
                Tab::History => {
                    if app.selected_history > 0 {
                        app.selected_history -= 1;
                    }
                }
            },
            KeyCode::Enter
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
                if app.active_tab == Tab::Changes =>
            {
                app.toggle_collapse_current();
            }
            KeyCode::Char(' ') if app.active_tab == Tab::Changes => {
                app.toggle_current();
            }
            KeyCode::Char('a') if app.active_tab == Tab::Changes => {
                app.toggle_all();
            }
            KeyCode::Char('c') if app.active_tab == Tab::Changes => {
                start_ai_commit(app, config, tx, false);
            }
            KeyCode::Char('C') if app.active_tab == Tab::Changes => {
                start_ai_commit(app, config, tx, true);
            }
            KeyCode::Char('d') if app.active_tab == Tab::Changes => {
                if let Some(target) = app.get_selected_target() {
                    app.modal = Modal::ConfirmAction {
                        action: ConfirmActionType::Discard,
                        target_path: target.path,
                        is_dir: target.is_dir,
                        file_count: target.file_count,
                    };
                } else {
                    app.set_status("No file or directory selected to discard", true);
                }
            }
            KeyCode::Char('x') | KeyCode::Char('D') | KeyCode::Delete
                if app.active_tab == Tab::Changes =>
            {
                if let Some(target) = app.get_selected_target() {
                    app.modal = Modal::ConfirmAction {
                        action: ConfirmActionType::Delete,
                        target_path: target.path,
                        is_dir: target.is_dir,
                        file_count: target.file_count,
                    };
                } else {
                    app.set_status("No file or directory selected to delete", true);
                }
            }
            _ => {}
        },
    }

    false
}

fn start_ai_commit(
    app: &mut App,
    config: &config::Config,
    tx: &Sender<BgEvent>,
    is_quick: bool,
) {
    let files = app.selected_files();
    if files.is_empty() {
        app.set_status("No files selected to commit", true);
        return;
    }

    match git::get_diff(&app.repo_path, &files) {
        Ok(diff) => {
            if diff.trim().is_empty() {
                app.set_status("No changes found in selected files", true);
                return;
            }

            let cfg = config.clone();
            let tx_clone = tx.clone();
            let loading_title = if is_quick {
                "Generating with AI & committing..."
            } else {
                "Generating commit message with AI..."
            };

            app.modal = Modal::Loading {
                title: loading_title.to_string(),
            };

            thread::spawn(move || {
                let res = ai::generate_commit_message(&cfg, &diff);
                let _ = tx_clone.send(BgEvent::AiCommitDone(res, is_quick));
            });
        }
        Err(e) => {
            app.set_status(&format!("Diff error: {}", e), true);
        }
    }
}

fn resolve_tilde(path_str: &str) -> String {
    if path_str.starts_with("~/") {
        if let Ok(home) = env::var("HOME") {
            return format!("{}/{}", home, &path_str[2..]);
        }
    }
    path_str.to_string()
}

fn autocomplete_path(input: &str) -> String {
    let expanded = resolve_tilde(input);
    let path = Path::new(&expanded);

    let (dir_to_read, prefix) = if expanded.ends_with('/') {
        (path, "")
    } else {
        (
            path.parent().unwrap_or_else(|| Path::new(".")),
            path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(""),
        )
    };

    if let Ok(entries) = fs::read_dir(dir_to_read) {
        let mut matches: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            if let Ok(file_name) = entry.file_name().into_string() {
                if file_name.starts_with(prefix) {
                    let mut full = entry.path().to_string_lossy().to_string();
                    if entry.path().is_dir() {
                        full.push('/');
                    }
                    matches.push(full);
                }
            }
        }

        if matches.len() == 1 {
            let res = matches[0].clone();
            if input.starts_with("~/") {
                if let Ok(home) = env::var("HOME") {
                    if res.starts_with(&home) {
                        return format!("~{}", &res[home.len()..]);
                    }
                }
            }
            return res;
        }
    }

    input.to_string()
}
