use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::buffer::TextBuffer;
use crate::commands::{AppCommand, Motion, MouseAction, MouseActionKind, SearchDirection};
use crate::config::AppConfig;
use crate::session::SessionData;
use crate::ui::{self, UiMetadata};
use crate::workspace::{TreeActivation, WorkspaceState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusArea {
    Tree,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub kind: StatusKind,
    pub sticky: bool,
}

#[derive(Debug, Clone)]
pub struct TextField {
    pub value: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Find,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    Query,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    SaveAs,
    GotoLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCommand {
    Save,
    SaveAs,
    CloseBuffer,
    ToggleSidebar,
    OpenParentFolder,
    ToggleHidden,
    ReloadWorkspace,
    OpenRecentWorkspace,
    GotoLine,
    RevertBuffer,
}

#[derive(Debug, Clone)]
pub enum PickerAction {
    OpenFile(PathBuf),
    Command(PaletteCommand),
}

#[derive(Debug, Clone)]
pub struct PickerItem {
    pub label: String,
    pub detail: String,
    pub action: PickerAction,
}

#[derive(Debug, Clone)]
pub struct PickerState {
    pub title: String,
    pub query: TextField,
    pub selected: usize,
    pub items: Vec<PickerItem>,
}

#[derive(Debug, Clone)]
pub struct SearchState {
    pub mode: SearchMode,
    pub query: TextField,
    pub replacement: TextField,
    pub active_field: SearchField,
    pub case_sensitive: bool,
}

#[derive(Debug, Clone)]
pub struct PromptState {
    pub kind: PromptKind,
    pub title: String,
    pub input: TextField,
}

#[derive(Debug, Clone)]
pub enum OverlayState {
    QuickOpen(PickerState),
    CommandPalette(PickerState),
    Search(SearchState),
    Prompt(PromptState),
    Help,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub workspace: WorkspaceState,
    pub focus: FocusArea,
    pub overlay: Option<OverlayState>,
    pub show_sidebar: bool,
    pub status: StatusMessage,
    pub buffers: BTreeMap<PathBuf, TextBuffer>,
    pub open_order: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub recent_workspace: Option<PathBuf>,
}

pub struct App {
    pub state: AppState,
    syntax_set: SyntaxSet,
    theme: Theme,
    clipboard: Option<Clipboard>,
    last_ui: UiMetadata,
    dirty_timestamps: BTreeMap<PathBuf, Instant>,
    should_quit: bool,
}

#[derive(Debug)]
struct StartupContext {
    config: AppConfig,
    workspace_root: PathBuf,
    initial_files: Vec<PathBuf>,
    active_file: Option<PathBuf>,
    recent_workspace: Option<PathBuf>,
}

impl Default for StatusMessage {
    fn default() -> Self {
        Self {
            text: "Ready".to_string(),
            kind: StatusKind::Info,
            sticky: false,
        }
    }
}

impl TextField {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn insert(&mut self, ch: char) {
        let byte_index = char_to_byte_index(&self.value, self.cursor);
        self.value.insert(byte_index, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = char_to_byte_index(&self.value, self.cursor);
        let start = char_to_byte_index(&self.value, self.cursor - 1);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.value.chars().count() {
            return;
        }
        let start = char_to_byte_index(&self.value, self.cursor);
        let end = char_to_byte_index(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }
}

impl AppState {
    pub fn active_buffer(&self) -> Option<&TextBuffer> {
        self.active.as_ref().and_then(|path| self.buffers.get(path))
    }

    pub fn active_buffer_mut(&mut self) -> Option<&mut TextBuffer> {
        let active = self.active.clone()?;
        self.buffers.get_mut(&active)
    }

    pub fn can_focus_tree(&self) -> bool {
        self.show_sidebar && !self.workspace.visible_nodes().is_empty()
    }

    #[cfg(test)]
    pub fn test_state() -> Self {
        let workspace = WorkspaceState::load(std::env::current_dir().unwrap(), true).unwrap();
        Self {
            config: AppConfig::default(),
            workspace,
            focus: FocusArea::Editor,
            overlay: None,
            show_sidebar: true,
            status: StatusMessage::default(),
            buffers: BTreeMap::new(),
            open_order: Vec::new(),
            active: None,
            recent_workspace: None,
        }
    }
}

pub fn run() -> Result<()> {
    let config_load = AppConfig::load_or_default();
    let session_load = SessionData::load();
    let session_warning = session_load
        .as_ref()
        .err()
        .map(|error| format!("Unable to load session: {error}"));
    let session = session_load.unwrap_or_default();

    let startup = StartupContext::resolve(config_load.config.clone(), session.clone())?;
    let mut app = App::new(startup, config_load.warning.or(session_warning))?;
    app.run_event_loop()?;
    app.persist_session()?;
    Ok(())
}

impl StartupContext {
    fn resolve(config: AppConfig, session: SessionData) -> Result<Self> {
        let cli_target = std::env::args_os().nth(1).map(PathBuf::from);
        let cwd = std::env::current_dir().context("Unable to resolve current working directory")?;

        let (workspace_root, cli_active_file) = match cli_target {
            Some(path) => {
                let path = fs::canonicalize(&path).unwrap_or(path);
                if path.is_dir() {
                    (path, None)
                } else if path.is_file() {
                    let parent = path
                        .parent()
                        .map(Path::to_path_buf)
                        .context("Input file has no parent directory")?;
                    (parent, Some(path))
                } else {
                    bail!("Path does not exist: {}", path.display());
                }
            }
            None => {
                let workspace = session
                    .workspace
                    .clone()
                    .filter(|path| path.is_dir())
                    .unwrap_or(cwd);
                let active = session.active_file.clone().filter(|path| path.is_file());
                (workspace, active)
            }
        };

        let sanitized = session.sanitize_for_workspace(&workspace_root);
        let mut initial_files = sanitized.open_files;
        if let Some(active) = cli_active_file.clone() {
            if !initial_files.contains(&active) {
                initial_files.push(active.clone());
            }
        }

        let active_file = cli_active_file.or(sanitized.active_file);
        Ok(Self {
            config,
            workspace_root,
            initial_files,
            active_file,
            recent_workspace: session.workspace,
        })
    }
}

impl App {
    fn new(startup: StartupContext, startup_warning: Option<String>) -> Result<Self> {
        let workspace =
            WorkspaceState::load(startup.workspace_root.clone(), startup.config.show_hidden)
                .with_context(|| {
                    format!(
                        "Unable to open workspace {}",
                        startup.workspace_root.display()
                    )
                })?;

        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set
            .themes
            .get(&startup.config.theme)
            .cloned()
            .or_else(|| theme_set.themes.get("base16-ocean.dark").cloned())
            .or_else(|| theme_set.themes.get("Solarized (dark)").cloned())
            .or_else(|| theme_set.themes.get("InspiredGitHub").cloned())
            .or_else(|| theme_set.themes.values().next().cloned())
            .context("No syntax themes available")?;

        let mut app = Self {
            state: AppState {
                config: startup.config,
                workspace,
                focus: FocusArea::Tree,
                overlay: None,
                show_sidebar: true,
                status: StatusMessage::default(),
                buffers: BTreeMap::new(),
                open_order: Vec::new(),
                active: None,
                recent_workspace: startup.recent_workspace,
            },
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme,
            clipboard: Clipboard::new().ok(),
            last_ui: UiMetadata::default(),
            dirty_timestamps: BTreeMap::new(),
            should_quit: false,
        };

        for path in startup.initial_files {
            let _ = app.open_buffer(path);
        }

        if let Some(active_file) = startup.active_file {
            let _ = app.open_buffer(active_file);
        }

        if app.state.active.is_some() {
            app.state.focus = FocusArea::Editor;
        } else if !app.state.can_focus_tree() {
            app.state.focus = FocusArea::Editor;
        }

        if let Some(message) = startup_warning {
            app.set_status(StatusKind::Warning, message, false);
        }

        Ok(app)
    }

    fn run_event_loop(&mut self) -> Result<()> {
        let mut terminal = TerminalSession::enter()?;
        while !self.should_quit {
            terminal.terminal.draw(|frame| {
                self.last_ui = ui::render(frame, &self.state, &self.syntax_set, &self.theme);
            })?;

            self.tick_autosave();
            if event::poll(Duration::from_millis(50))? {
                let event = event::read()?;
                if let Some(command) = self.map_event_to_command(event) {
                    self.dispatch(command)?;
                }
            }
        }
        Ok(())
    }

    fn map_event_to_command(&self, event: Event) -> Option<AppCommand> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.map_key_to_command(key),
            Event::Mouse(mouse) => self.map_mouse_to_command(mouse),
            _ => None,
        }
    }

    fn map_mouse_to_command(&self, event: MouseEvent) -> Option<AppCommand> {
        let kind = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => MouseActionKind::Down,
            MouseEventKind::ScrollUp => MouseActionKind::ScrollUp,
            MouseEventKind::ScrollDown => MouseActionKind::ScrollDown,
            _ => return None,
        };
        Some(AppCommand::Mouse(MouseAction {
            kind,
            column: event.column,
            row: event.row,
        }))
    }

    fn map_key_to_command(&self, key: KeyEvent) -> Option<AppCommand> {
        let modifiers = key.modifiers;
        let code = key.code;
        let overlay_open = self.state.overlay.is_some();

        if matches!(code, KeyCode::F(1)) {
            return Some(AppCommand::OpenHelp);
        }

        if matches!(code, KeyCode::Tab) && modifiers.contains(KeyModifiers::CONTROL) {
            return Some(AppCommand::CycleTab(
                if modifiers.contains(KeyModifiers::SHIFT) {
                    -1
                } else {
                    1
                },
            ));
        }

        if modifiers.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Left => return Some(AppCommand::CycleTab(-1)),
                KeyCode::Right => return Some(AppCommand::CycleTab(1)),
                _ => {}
            }
        }

        if modifiers.contains(KeyModifiers::CONTROL) {
            return match normalized_char(code) {
                Some('q') => Some(AppCommand::Quit),
                Some('s') => Some(AppCommand::Save),
                Some('.') => Some(AppCommand::ToggleHiddenFiles),
                Some('p') if modifiers.contains(KeyModifiers::SHIFT) => {
                    Some(AppCommand::OpenCommandPalette)
                }
                Some('p') => Some(AppCommand::OpenQuickOpen),
                Some('f') => Some(AppCommand::OpenFind),
                Some('h') => Some(AppCommand::OpenReplace),
                Some('w') if !overlay_open => Some(AppCommand::CloseBuffer),
                Some('z') if !overlay_open => Some(AppCommand::Undo),
                Some('y') if !overlay_open => Some(AppCommand::Redo),
                Some('c') if !overlay_open => Some(AppCommand::Copy),
                Some('x') if !overlay_open => Some(AppCommand::Cut),
                Some('v') if !overlay_open => Some(AppCommand::Paste),
                Some('r') if matches!(self.state.overlay, Some(OverlayState::Search(_))) => {
                    Some(AppCommand::ReplaceCurrent)
                }
                Some('a') if matches!(self.state.overlay, Some(OverlayState::Search(_))) => {
                    Some(AppCommand::ReplaceAll)
                }
                Some('g') => Some(AppCommand::PromptGotoLine),
                _ => None,
            };
        }

        if overlay_open {
            return self.map_overlay_key(key);
        }

        match code {
            KeyCode::Tab => match self.state.focus {
                FocusArea::Editor if self.state.active.is_some() => Some(AppCommand::InsertTab),
                _ => Some(AppCommand::FocusNext),
            },
            KeyCode::BackTab => Some(AppCommand::FocusPrev),
            KeyCode::F(8) => Some(AppCommand::ToggleHiddenFiles),
            KeyCode::Up
                if self.state.focus == FocusArea::Tree && modifiers.contains(KeyModifiers::ALT) =>
            {
                Some(AppCommand::OpenParentDirectory)
            }
            KeyCode::Up => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::TreeMove(-1)),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: Motion::Up,
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::Down => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::TreeMove(1)),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: Motion::Down,
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::Left => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::OpenParentDirectory),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: if modifiers.contains(KeyModifiers::ALT) {
                        Motion::WordLeft
                    } else {
                        Motion::Left
                    },
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::Right => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::ActivateFocusedItem),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: if modifiers.contains(KeyModifiers::ALT) {
                        Motion::WordRight
                    } else {
                        Motion::Right
                    },
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::Home => Some(AppCommand::EditMotion {
                motion: Motion::Home,
                extend_selection: modifiers.contains(KeyModifiers::SHIFT),
            }),
            KeyCode::End => Some(AppCommand::EditMotion {
                motion: Motion::End,
                extend_selection: modifiers.contains(KeyModifiers::SHIFT),
            }),
            KeyCode::PageUp => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::TreePage(-1)),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: Motion::PageUp,
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::PageDown => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::TreePage(1)),
                FocusArea::Editor => Some(AppCommand::EditMotion {
                    motion: Motion::PageDown,
                    extend_selection: modifiers.contains(KeyModifiers::SHIFT),
                }),
            },
            KeyCode::Enter => Some(AppCommand::ActivateFocusedItem),
            KeyCode::Backspace => match self.state.focus {
                FocusArea::Tree => Some(AppCommand::OpenParentDirectory),
                FocusArea::Editor => Some(AppCommand::Backspace),
            },
            KeyCode::Delete => Some(AppCommand::DeleteForward),
            KeyCode::Char(ch) if !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER) => {
                Some(AppCommand::InsertChar(ch))
            }
            _ => None,
        }
    }

    fn map_overlay_key(&self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Esc => Some(AppCommand::CancelOverlay),
            KeyCode::Enter => Some(AppCommand::OverlaySubmit),
            KeyCode::Up => Some(AppCommand::PickerMove(-1)),
            KeyCode::Down => Some(AppCommand::PickerMove(1)),
            KeyCode::Backspace => Some(AppCommand::OverlayBackspace),
            KeyCode::Delete => Some(AppCommand::OverlayDelete),
            KeyCode::Left => Some(AppCommand::OverlayMoveLeft),
            KeyCode::Right => Some(AppCommand::OverlayMoveRight),
            KeyCode::Tab => Some(AppCommand::OverlayTab),
            KeyCode::F(6) => Some(AppCommand::OverlayToggleCase),
            KeyCode::F(3) => Some(AppCommand::Search(SearchDirection::Next)),
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                Some(AppCommand::InsertChar(ch))
            }
            _ => None,
        }
    }

    fn dispatch(&mut self, command: AppCommand) -> Result<()> {
        match command {
            AppCommand::None => {}
            AppCommand::Quit => self.request_quit(),
            AppCommand::Save => self.save_active_buffer()?,
            AppCommand::PromptSaveAs => self.open_save_as_prompt(),
            AppCommand::OpenQuickOpen => self.open_quick_open(),
            AppCommand::OpenCommandPalette => self.open_command_palette(),
            AppCommand::OpenHelp => self.toggle_help_overlay(),
            AppCommand::OpenFind => self.open_search(SearchMode::Find),
            AppCommand::OpenReplace => self.open_search(SearchMode::Replace),
            AppCommand::PromptGotoLine => self.open_prompt(PromptKind::GotoLine),
            AppCommand::ToggleSidebar => self.toggle_sidebar(),
            AppCommand::OpenParentDirectory => self.open_parent_directory()?,
            AppCommand::ToggleHiddenFiles => self.toggle_hidden_files()?,
            AppCommand::ReloadWorkspace => self.reload_workspace()?,
            AppCommand::OpenRecentWorkspace => self.open_recent_workspace()?,
            AppCommand::CloseBuffer => self.close_active_buffer()?,
            AppCommand::CycleTab(delta) => self.cycle_tab(delta)?,
            AppCommand::RevertBuffer => self.revert_active_buffer()?,
            AppCommand::FocusNext => self.focus_next(),
            AppCommand::FocusPrev => self.focus_prev(),
            AppCommand::TreeMove(delta) => self
                .state
                .workspace
                .move_selection(delta, self.tree_viewport_height()),
            AppCommand::TreeExpand => {
                if self
                    .state
                    .workspace
                    .selected_node()
                    .is_some_and(|node| node.is_parent_link)
                {
                    self.open_parent_directory()?;
                } else {
                    self.state
                        .workspace
                        .expand_selected(self.tree_viewport_height());
                }
            }
            AppCommand::TreeCollapse => self
                .state
                .workspace
                .collapse_selected(self.tree_viewport_height()),
            AppCommand::TreeScroll(delta) => self
                .state
                .workspace
                .scroll_by(delta, self.tree_viewport_height()),
            AppCommand::TreePage(direction) => self
                .state
                .workspace
                .page_selection(direction, self.tree_viewport_height()),
            AppCommand::PickerMove(delta) => self.move_picker_selection(delta),
            AppCommand::ActivateFocusedItem => self.activate_focused_item()?,
            AppCommand::CancelOverlay => self.cancel_overlay(),
            AppCommand::OverlayBackspace => self.overlay_backspace(),
            AppCommand::OverlayDelete => self.overlay_delete(),
            AppCommand::OverlayMoveLeft => self.overlay_move_left(),
            AppCommand::OverlayMoveRight => self.overlay_move_right(),
            AppCommand::OverlaySubmit => self.submit_overlay()?,
            AppCommand::OverlayTab => self.overlay_tab(),
            AppCommand::OverlayToggleCase => self.toggle_search_case(),
            AppCommand::ReplaceCurrent => self.replace_current_match(),
            AppCommand::ReplaceAll => self.replace_all_matches(),
            AppCommand::Search(direction) => self.run_search(direction),
            AppCommand::EditMotion {
                motion,
                extend_selection,
            } => self.apply_motion(motion, extend_selection),
            AppCommand::InsertChar(ch) => self.insert_char(ch),
            AppCommand::InsertNewline => self.insert_newline(),
            AppCommand::InsertTab => self.insert_tab(),
            AppCommand::Backspace => self.backspace(),
            AppCommand::DeleteForward => self.delete_forward(),
            AppCommand::Undo => self.undo(),
            AppCommand::Redo => self.redo(),
            AppCommand::Copy => self.copy_selection(),
            AppCommand::Cut => self.cut_selection(),
            AppCommand::Paste => self.paste_clipboard(),
            AppCommand::Mouse(action) => self.handle_mouse(action)?,
        }

        self.ensure_active_cursor_visible();
        Ok(())
    }

    fn request_quit(&mut self) {
        if self.save_all_dirty_buffers().is_ok() {
            self.should_quit = true;
        }
    }

    fn focus_next(&mut self) {
        if self.state.overlay.is_some() {
            self.overlay_tab();
            return;
        }

        self.state.focus = match self.state.focus {
            FocusArea::Tree if self.state.active.is_some() => FocusArea::Editor,
            FocusArea::Tree => FocusArea::Tree,
            FocusArea::Editor if self.state.can_focus_tree() => FocusArea::Tree,
            FocusArea::Editor => FocusArea::Editor,
        };
    }

    fn focus_prev(&mut self) {
        self.focus_next();
    }

    fn open_quick_open(&mut self) {
        let items = self
            .state
            .workspace
            .files()
            .iter()
            .map(|path| PickerItem {
                label: relative_path(&self.state.workspace.root, path),
                detail: path.display().to_string(),
                action: PickerAction::OpenFile(path.clone()),
            })
            .collect();
        self.state.overlay = Some(OverlayState::QuickOpen(PickerState {
            title: "Quick Open".to_string(),
            query: TextField::new(""),
            selected: 0,
            items,
        }));
    }

    fn open_command_palette(&mut self) {
        let items = vec![
            palette_item("Save", "Write active buffer", PaletteCommand::Save),
            palette_item("Save As", "Write to a new path", PaletteCommand::SaveAs),
            palette_item(
                "Close Buffer",
                "Close the active file",
                PaletteCommand::CloseBuffer,
            ),
            palette_item(
                "Toggle Sidebar",
                "Show or hide the file tree",
                PaletteCommand::ToggleSidebar,
            ),
            palette_item(
                if self.state.config.show_hidden {
                    "Hide Hidden Files"
                } else {
                    "Show Hidden Files"
                },
                if self.state.config.show_hidden {
                    "Filter dotfiles and ignored paths"
                } else {
                    "Reveal dotfiles and ignored paths"
                },
                PaletteCommand::ToggleHidden,
            ),
            palette_item(
                "Open Parent Folder",
                "Move the browser up one directory",
                PaletteCommand::OpenParentFolder,
            ),
            palette_item(
                "Reload Workspace",
                "Refresh files from disk",
                PaletteCommand::ReloadWorkspace,
            ),
            palette_item(
                "Open Recent Workspace",
                "Reopen the last workspace",
                PaletteCommand::OpenRecentWorkspace,
            ),
            palette_item(
                "Go to Line",
                "Jump to a line number",
                PaletteCommand::GotoLine,
            ),
            palette_item(
                "Revert Buffer",
                "Discard unsaved changes",
                PaletteCommand::RevertBuffer,
            ),
        ];
        self.state.overlay = Some(OverlayState::CommandPalette(PickerState {
            title: "Command Palette".to_string(),
            query: TextField::new(""),
            selected: 0,
            items,
        }));
    }

    fn open_search(&mut self, mode: SearchMode) {
        let query = self
            .state
            .active_buffer()
            .and_then(|buffer| buffer.selected_text())
            .unwrap_or_default();
        self.state.overlay = Some(OverlayState::Search(SearchState {
            mode,
            query: TextField::new(query),
            replacement: TextField::new(""),
            active_field: SearchField::Query,
            case_sensitive: false,
        }));
    }

    fn toggle_help_overlay(&mut self) {
        if matches!(self.state.overlay, Some(OverlayState::Help)) {
            self.state.overlay = None;
        } else {
            self.state.overlay = Some(OverlayState::Help);
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        let (title, seed) = match kind {
            PromptKind::GotoLine => ("Go to Line".to_string(), String::new()),
            PromptKind::SaveAs => {
                let current = self
                    .state
                    .active_buffer()
                    .and_then(|buffer| buffer.path.clone())
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                ("Save As".to_string(), current)
            }
        };
        self.state.overlay = Some(OverlayState::Prompt(PromptState {
            kind,
            title,
            input: TextField::new(seed),
        }));
    }

    fn open_save_as_prompt(&mut self) {
        self.open_prompt(PromptKind::SaveAs);
    }

    fn toggle_sidebar(&mut self) {
        self.state.show_sidebar = !self.state.show_sidebar;
        if !self.state.show_sidebar {
            self.state.focus = FocusArea::Editor;
        }
    }

    fn change_workspace_root(
        &mut self,
        new_root: PathBuf,
        preferred_selection: Option<PathBuf>,
    ) -> Result<()> {
        let new_root = fs::canonicalize(&new_root).unwrap_or(new_root);
        if new_root == self.state.workspace.root {
            return Ok(());
        }

        let previous_root = self.state.workspace.root.clone();
        self.state.workspace =
            WorkspaceState::load(new_root.clone(), self.state.config.show_hidden)?;
        self.state.recent_workspace = Some(previous_root.clone());
        self.state.focus = if self.state.show_sidebar {
            FocusArea::Tree
        } else {
            FocusArea::Editor
        };

        if let Some(path) = preferred_selection
            .filter(|path| path.starts_with(&new_root))
            .or_else(|| {
                self.state
                    .active
                    .clone()
                    .filter(|path| path.starts_with(&new_root))
            })
        {
            self.state
                .workspace
                .reveal_path(&path, self.tree_viewport_height());
        }

        self.set_status(
            StatusKind::Info,
            format!("Browsing {}", self.state.workspace.root.display()),
            false,
        );
        Ok(())
    }

    fn open_parent_directory(&mut self) -> Result<()> {
        let Some(parent) = self.state.workspace.parent_root() else {
            return Ok(());
        };
        let current_root = self.state.workspace.root.clone();
        self.change_workspace_root(parent, Some(current_root))
    }

    fn tree_viewport_height(&self) -> usize {
        self.last_ui.tree_inner.height.max(1) as usize
    }

    fn toggle_hidden_files(&mut self) -> Result<()> {
        self.state.config.show_hidden = !self.state.config.show_hidden;
        self.reload_workspace()?;
        let visibility = if self.state.config.show_hidden {
            "Showing hidden files"
        } else {
            "Hiding hidden files"
        };
        self.set_status(StatusKind::Info, visibility.to_string(), false);
        Ok(())
    }

    fn reload_workspace(&mut self) -> Result<()> {
        self.state.workspace.reload(self.state.config.show_hidden)?;
        if let Some(active) = self.state.active.clone() {
            self.state
                .workspace
                .reveal_path(&active, self.tree_viewport_height());
        }
        self.set_status(StatusKind::Info, "Workspace reloaded".to_string(), false);
        Ok(())
    }

    fn open_recent_workspace(&mut self) -> Result<()> {
        let Some(path) = self.state.recent_workspace.clone() else {
            self.set_status(
                StatusKind::Warning,
                "No recent workspace available".to_string(),
                false,
            );
            return Ok(());
        };
        self.save_all_dirty_buffers()?;
        let previous_workspace = self.state.workspace.root.clone();
        let session = SessionData::load()
            .unwrap_or_default()
            .sanitize_for_workspace(&path);
        self.state.workspace = WorkspaceState::load(path.clone(), self.state.config.show_hidden)?;
        self.state.buffers.clear();
        self.state.open_order.clear();
        self.state.active = None;
        self.dirty_timestamps.clear();
        self.state.focus = if self.state.can_focus_tree() {
            FocusArea::Tree
        } else {
            FocusArea::Editor
        };

        for file in session.open_files {
            let _ = self.open_buffer(file);
        }
        if let Some(active) = session.active_file {
            let _ = self.open_buffer(active);
        }

        self.state.recent_workspace = Some(previous_workspace);
        self.set_status(
            StatusKind::Info,
            "Reopened recent workspace".to_string(),
            false,
        );
        Ok(())
    }

    fn activate_focused_item(&mut self) -> Result<()> {
        if self.state.overlay.is_some() {
            return self.submit_overlay();
        }

        match self.state.focus {
            FocusArea::Tree => match self.state.workspace.activate_selected() {
                TreeActivation::OpenFile(path) => self.open_buffer(path)?,
                TreeActivation::ChangeRoot(path) => {
                    let current_root = self.state.workspace.root.clone();
                    self.change_workspace_root(path, Some(current_root))?;
                }
                TreeActivation::ToggleDirectory(_) | TreeActivation::None => {}
            },
            FocusArea::Editor => self.insert_newline(),
        }
        Ok(())
    }

    fn cancel_overlay(&mut self) {
        self.state.overlay = None;
    }

    fn overlay_backspace(&mut self) {
        match self.state.overlay.as_mut() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                picker.query.backspace();
                picker.selected = 0;
            }
            Some(OverlayState::Search(search)) => search_field_mut(search).backspace(),
            Some(OverlayState::Prompt(prompt)) => prompt.input.backspace(),
            Some(OverlayState::Help) => {}
            None => {}
        }
    }

    fn overlay_delete(&mut self) {
        match self.state.overlay.as_mut() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                picker.query.delete()
            }
            Some(OverlayState::Search(search)) => search_field_mut(search).delete(),
            Some(OverlayState::Prompt(prompt)) => prompt.input.delete(),
            Some(OverlayState::Help) => {}
            None => {}
        }
    }

    fn overlay_move_left(&mut self) {
        match self.state.overlay.as_mut() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                picker.query.move_left()
            }
            Some(OverlayState::Search(search)) => search_field_mut(search).move_left(),
            Some(OverlayState::Prompt(prompt)) => prompt.input.move_left(),
            Some(OverlayState::Help) => {}
            None => {}
        }
    }

    fn overlay_move_right(&mut self) {
        match self.state.overlay.as_mut() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                picker.query.move_right()
            }
            Some(OverlayState::Search(search)) => search_field_mut(search).move_right(),
            Some(OverlayState::Prompt(prompt)) => prompt.input.move_right(),
            Some(OverlayState::Help) => {}
            None => {}
        }
    }

    fn overlay_tab(&mut self) {
        if let Some(OverlayState::Search(search)) = self.state.overlay.as_mut() {
            search.active_field = match search.active_field {
                SearchField::Query if search.mode == SearchMode::Replace => {
                    SearchField::Replacement
                }
                _ => SearchField::Query,
            };
        }
    }

    fn toggle_search_case(&mut self) {
        if let Some(OverlayState::Search(search)) = self.state.overlay.as_mut() {
            search.case_sensitive = !search.case_sensitive;
        }
    }

    fn move_picker_selection(&mut self, delta: i32) {
        let Some(picker) = self.current_picker_mut() else {
            return;
        };
        let filtered = filtered_items(picker);
        if filtered.is_empty() {
            picker.selected = 0;
            return;
        }
        let max = filtered.len().saturating_sub(1) as i32;
        picker.selected = (picker.selected as i32 + delta).clamp(0, max) as usize;
    }

    fn submit_overlay(&mut self) -> Result<()> {
        match self.state.overlay.clone() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                let filtered = filtered_items(&picker);
                if let Some(item) = filtered.get(picker.selected).cloned() {
                    self.state.overlay = None;
                    match item.action {
                        PickerAction::OpenFile(path) => self.open_buffer(path)?,
                        PickerAction::Command(command) => self.run_palette_command(command)?,
                    }
                }
            }
            Some(OverlayState::Search(_)) => self.run_search(SearchDirection::Next),
            Some(OverlayState::Prompt(prompt)) => {
                self.state.overlay = None;
                self.submit_prompt(prompt)?;
            }
            Some(OverlayState::Help) => {
                self.state.overlay = None;
            }
            None => {}
        }
        Ok(())
    }

    fn run_palette_command(&mut self, command: PaletteCommand) -> Result<()> {
        match command {
            PaletteCommand::Save => self.save_active_buffer()?,
            PaletteCommand::SaveAs => self.open_save_as_prompt(),
            PaletteCommand::CloseBuffer => self.close_active_buffer()?,
            PaletteCommand::ToggleSidebar => self.toggle_sidebar(),
            PaletteCommand::OpenParentFolder => self.open_parent_directory()?,
            PaletteCommand::ToggleHidden => self.toggle_hidden_files()?,
            PaletteCommand::ReloadWorkspace => self.reload_workspace()?,
            PaletteCommand::OpenRecentWorkspace => self.open_recent_workspace()?,
            PaletteCommand::GotoLine => self.open_prompt(PromptKind::GotoLine),
            PaletteCommand::RevertBuffer => self.revert_active_buffer()?,
        }
        Ok(())
    }

    fn submit_prompt(&mut self, prompt: PromptState) -> Result<()> {
        match prompt.kind {
            PromptKind::SaveAs => {
                let input = prompt.input.value.trim();
                if input.is_empty() {
                    self.set_status(StatusKind::Warning, "Save path is empty".to_string(), false);
                    return Ok(());
                }
                self.save_active_buffer_as(PathBuf::from(input))?;
            }
            PromptKind::GotoLine => {
                if let Ok(line) = prompt.input.value.trim().parse::<usize>() {
                    if let Some(buffer) = self.state.active_buffer_mut() {
                        buffer.goto_line(line);
                    }
                } else {
                    self.set_status(
                        StatusKind::Warning,
                        "Invalid line number".to_string(),
                        false,
                    );
                }
            }
        }
        Ok(())
    }

    fn insert_char(&mut self, ch: char) {
        if let Some(overlay) = self.state.overlay.as_mut() {
            match overlay {
                OverlayState::QuickOpen(picker) | OverlayState::CommandPalette(picker) => {
                    picker.query.insert(ch);
                    picker.selected = 0;
                }
                OverlayState::Search(search) => search_field_mut(search).insert(ch),
                OverlayState::Prompt(prompt) => prompt.input.insert(ch),
                OverlayState::Help => {}
            }
            return;
        }

        if self.state.focus != FocusArea::Editor {
            return;
        }
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.insert_char(ch);
            self.mark_active_buffer_dirty();
        }
    }

    fn insert_newline(&mut self) {
        if self.state.overlay.is_some() || self.state.focus != FocusArea::Editor {
            return;
        }
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.insert_newline();
            self.mark_active_buffer_dirty();
        }
    }

    fn insert_tab(&mut self) {
        if self.state.overlay.is_some() || self.state.focus != FocusArea::Editor {
            return;
        }
        let tab_width = self.state.config.tab_width;
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.insert_indent(tab_width);
            self.mark_active_buffer_dirty();
        }
    }

    fn backspace(&mut self) {
        if self.state.overlay.is_some() || self.state.focus != FocusArea::Editor {
            return;
        }
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.backspace();
            self.mark_active_buffer_dirty();
        }
    }

    fn delete_forward(&mut self) {
        if self.state.overlay.is_some() || self.state.focus != FocusArea::Editor {
            return;
        }
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.delete_forward();
            self.mark_active_buffer_dirty();
        }
    }

    fn undo(&mut self) {
        if let Some(buffer) = self.state.active_buffer_mut() {
            if buffer.undo() {
                self.mark_active_buffer_dirty();
            }
        }
    }

    fn redo(&mut self) {
        if let Some(buffer) = self.state.active_buffer_mut() {
            if buffer.redo() {
                self.mark_active_buffer_dirty();
            }
        }
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self
            .state
            .active_buffer()
            .and_then(|buffer| buffer.selected_text())
        {
            if let Some(clipboard) = self.clipboard.as_mut() {
                let _ = clipboard.set_text(text);
            }
        }
    }

    fn cut_selection(&mut self) {
        let has_selection = self
            .state
            .active_buffer()
            .and_then(|buffer| buffer.selected_text())
            .is_some();
        if !has_selection {
            return;
        }
        self.copy_selection();
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.backspace();
            self.mark_active_buffer_dirty();
        }
    }

    fn paste_clipboard(&mut self) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            return;
        };
        let Ok(text) = clipboard.get_text() else {
            return;
        };
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.insert_str(&text.replace("\r\n", "\n"));
            self.mark_active_buffer_dirty();
        }
    }

    fn apply_motion(&mut self, motion: Motion, extend_selection: bool) {
        if self.state.focus != FocusArea::Editor {
            return;
        }
        let page_rows = self.last_ui.editor_inner.height.saturating_sub(1) as usize;
        if let Some(buffer) = self.state.active_buffer_mut() {
            match motion {
                Motion::Left => buffer.move_left(extend_selection),
                Motion::Right => buffer.move_right(extend_selection),
                Motion::Up => buffer.move_up(extend_selection),
                Motion::Down => buffer.move_down(extend_selection),
                Motion::Home => buffer.move_home(extend_selection),
                Motion::End => buffer.move_end(extend_selection),
                Motion::PageUp => buffer.move_page_up(page_rows.max(1), extend_selection),
                Motion::PageDown => buffer.move_page_down(page_rows.max(1), extend_selection),
                Motion::WordLeft => buffer.move_word_left(extend_selection),
                Motion::WordRight => buffer.move_word_right(extend_selection),
            }
        }
    }

    fn run_search(&mut self, direction: SearchDirection) {
        let Some(OverlayState::Search(search)) = self.state.overlay.as_ref() else {
            return;
        };
        let query = search.query.value.clone();
        let case_sensitive = search.case_sensitive;
        let result = if let Some(buffer) = self.state.active_buffer_mut() {
            match direction {
                SearchDirection::Next => buffer.search_next(&query, case_sensitive),
                SearchDirection::Previous => buffer.search_previous(&query, case_sensitive),
            }
        } else {
            None
        };
        if result.is_none() && !query.is_empty() {
            self.set_status(StatusKind::Warning, "No matches found".to_string(), false);
        }
    }

    fn replace_current_match(&mut self) {
        let Some(OverlayState::Search(search)) = self.state.overlay.as_ref() else {
            return;
        };
        let query = search.query.value.clone();
        let replacement = search.replacement.value.clone();
        let case_sensitive = search.case_sensitive;
        if let Some(buffer) = self.state.active_buffer_mut() {
            if buffer.replace_current(&query, &replacement, case_sensitive) {
                self.mark_active_buffer_dirty();
            }
        }
    }

    fn replace_all_matches(&mut self) {
        let Some(OverlayState::Search(search)) = self.state.overlay.as_ref() else {
            return;
        };
        let query = search.query.value.clone();
        let replacement = search.replacement.value.clone();
        let case_sensitive = search.case_sensitive;
        if let Some(buffer) = self.state.active_buffer_mut() {
            let count = buffer.replace_all(&query, &replacement, case_sensitive);
            if count > 0 {
                self.mark_active_buffer_dirty();
                self.set_status(StatusKind::Info, format!("Replaced {count} matches"), false);
            }
        }
    }

    fn handle_mouse(&mut self, action: MouseAction) -> Result<()> {
        if self.state.overlay.is_some() {
            return Ok(());
        }
        match action.kind {
            MouseActionKind::Down => {
                if self
                    .last_ui
                    .tab_bar_area
                    .contains((action.column, action.row).into())
                {
                    if let Some(hit) = self
                        .last_ui
                        .tab_hits
                        .iter()
                        .find(|hit| hit.area.contains((action.column, action.row).into()))
                    {
                        self.open_buffer(hit.path.clone())?;
                        return Ok(());
                    }
                }
                if self.state.show_sidebar
                    && self
                        .last_ui
                        .tree_inner
                        .contains((action.column, action.row).into())
                {
                    let row_offset = action.row.saturating_sub(self.last_ui.tree_inner.y) as usize;
                    let index = self.last_ui.tree_scroll + row_offset;
                    if index < self.state.workspace.visible_nodes().len() {
                        self.state
                            .workspace
                            .set_selected_index(index, self.tree_viewport_height());
                        self.state.focus = FocusArea::Tree;
                        match self.state.workspace.activate_selected() {
                            TreeActivation::OpenFile(path) => self.open_buffer(path)?,
                            TreeActivation::ChangeRoot(path) => {
                                let current_root = self.state.workspace.root.clone();
                                self.change_workspace_root(path, Some(current_root))?;
                            }
                            TreeActivation::ToggleDirectory(_) | TreeActivation::None => {}
                        }
                    }
                } else if self
                    .last_ui
                    .editor_inner
                    .contains((action.column, action.row).into())
                {
                    self.state.focus = FocusArea::Editor;
                    let gutter = self.last_ui.gutter_width;
                    if let Some(buffer) = self.state.active_buffer_mut() {
                        let line = buffer.scroll_y
                            + action.row.saturating_sub(self.last_ui.editor_inner.y) as usize;
                        let col = buffer.scroll_x
                            + action
                                .column
                                .saturating_sub(self.last_ui.editor_inner.x + gutter)
                                as usize;
                        buffer.set_cursor_from_screen(line, col);
                    }
                }
            }
            MouseActionKind::ScrollUp => {
                if self.state.show_sidebar
                    && self
                        .last_ui
                        .tree_inner
                        .contains((action.column, action.row).into())
                {
                    self.state
                        .workspace
                        .scroll_by(-3, self.tree_viewport_height());
                    self.state.focus = FocusArea::Tree;
                } else if let Some(buffer) = self.state.active_buffer_mut() {
                    buffer.scroll_lines(-3);
                }
            }
            MouseActionKind::ScrollDown => {
                if self.state.show_sidebar
                    && self
                        .last_ui
                        .tree_inner
                        .contains((action.column, action.row).into())
                {
                    self.state
                        .workspace
                        .scroll_by(3, self.tree_viewport_height());
                    self.state.focus = FocusArea::Tree;
                } else if let Some(buffer) = self.state.active_buffer_mut() {
                    buffer.scroll_lines(3);
                }
            }
        }
        Ok(())
    }

    fn open_buffer(&mut self, path: PathBuf) -> Result<()> {
        self.save_active_if_needed()?;
        let path = fs::canonicalize(&path).unwrap_or(path);
        if !self.state.buffers.contains_key(&path) {
            match TextBuffer::from_file(path.clone()) {
                Ok(buffer) => {
                    self.state.buffers.insert(path.clone(), buffer);
                }
                Err(error) => {
                    let message = match error.kind() {
                        io::ErrorKind::InvalidData => format!(
                            "Cannot open {}: unsupported or binary file",
                            relative_path(&self.state.workspace.root, &path)
                        ),
                        _ => format!(
                            "Cannot open {}: {}",
                            relative_path(&self.state.workspace.root, &path),
                            error
                        ),
                    };
                    self.set_status(StatusKind::Error, message, false);
                    return Ok(());
                }
            }
            self.state.open_order.push(path.clone());
        }
        self.state.active = Some(path.clone());
        self.state.focus = FocusArea::Editor;
        self.state
            .workspace
            .reveal_path(&path, self.tree_viewport_height());
        self.set_status(
            StatusKind::Info,
            format!(
                "Opened {}",
                relative_path(&self.state.workspace.root, &path)
            ),
            false,
        );
        Ok(())
    }

    fn cycle_tab(&mut self, delta: i32) -> Result<()> {
        if self.state.open_order.is_empty() {
            return Ok(());
        }
        let current = self
            .state
            .active
            .as_ref()
            .and_then(|active| self.state.open_order.iter().position(|path| path == active))
            .unwrap_or_else(|| self.state.open_order.len().saturating_sub(1));
        let len = self.state.open_order.len() as i32;
        let next = (current as i32 + delta).rem_euclid(len) as usize;
        let path = self.state.open_order[next].clone();
        self.open_buffer(path)
    }

    fn save_active_if_needed(&mut self) -> Result<()> {
        if self
            .state
            .active_buffer()
            .is_some_and(|buffer| buffer.dirty)
        {
            self.save_active_buffer()?;
        }
        Ok(())
    }

    fn save_active_buffer(&mut self) -> Result<()> {
        let Some(path) = self.state.active.clone() else {
            return Ok(());
        };
        self.save_buffer(&path)
    }

    fn save_active_buffer_as(&mut self, path: PathBuf) -> Result<()> {
        let Some(active) = self.state.active.clone() else {
            return Ok(());
        };

        let requested_path = path;
        let old = active.clone();
        let buffer = self
            .state
            .buffers
            .get_mut(&active)
            .context("No active buffer available")?;
        if let Err(error) = buffer.save_as(requested_path.clone()) {
            let message = format!("Save failed for {}: {error}", requested_path.display());
            buffer.last_error = Some(message.clone());
            self.set_status(StatusKind::Error, message, true);
            bail!("save failed");
        }

        let path = fs::canonicalize(&requested_path).unwrap_or(requested_path);
        let buffer = self.state.buffers.remove(&old).unwrap();
        self.state.buffers.insert(path.clone(), buffer);
        self.state.open_order.retain(|open| open != &old);
        self.state.open_order.push(path.clone());
        self.state.active = Some(path.clone());
        self.dirty_timestamps.remove(&old);
        self.set_status(
            StatusKind::Info,
            format!("Saved {}", relative_path(&self.state.workspace.root, &path)),
            false,
        );
        Ok(())
    }

    fn save_buffer(&mut self, path: &Path) -> Result<()> {
        let Some(buffer) = self.state.buffers.get_mut(path) else {
            return Ok(());
        };
        if let Err(error) = buffer.save() {
            let message = format!("Save failed for {}: {error}", path.display());
            buffer.last_error = Some(message.clone());
            self.set_status(StatusKind::Error, message, true);
            bail!("save failed");
        }
        self.dirty_timestamps.remove(path);
        self.set_status(
            StatusKind::Info,
            format!("Saved {}", relative_path(&self.state.workspace.root, path)),
            false,
        );
        Ok(())
    }

    fn save_all_dirty_buffers(&mut self) -> Result<()> {
        let dirty = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.dirty)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();

        for path in dirty {
            self.save_buffer(&path)?;
        }
        Ok(())
    }

    fn close_active_buffer(&mut self) -> Result<()> {
        self.save_active_if_needed()?;
        let Some(active) = self.state.active.clone() else {
            return Ok(());
        };
        let closed_name = relative_path(&self.state.workspace.root, &active);
        let closed_index = self
            .state
            .open_order
            .iter()
            .position(|path| path == &active)
            .unwrap_or(0);
        self.state.buffers.remove(&active);
        self.state.open_order.retain(|path| path != &active);
        self.state.active = if self.state.open_order.is_empty() {
            None
        } else {
            let next_index = closed_index.min(self.state.open_order.len().saturating_sub(1));
            self.state.open_order.get(next_index).cloned().or_else(|| {
                self.state
                    .open_order
                    .get(next_index.saturating_sub(1))
                    .cloned()
            })
        };
        if self.state.active.is_none() {
            self.state.focus = FocusArea::Tree;
        }
        self.set_status(StatusKind::Info, format!("Closed {closed_name}"), false);
        Ok(())
    }

    fn revert_active_buffer(&mut self) -> Result<()> {
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.reload_from_disk()?;
            self.set_status(
                StatusKind::Info,
                "Reverted buffer from disk".to_string(),
                false,
            );
        }
        Ok(())
    }

    fn mark_active_buffer_dirty(&mut self) {
        if let Some(active) = self.state.active.clone() {
            let is_dirty = self
                .state
                .buffers
                .get(&active)
                .is_some_and(|buffer| buffer.dirty);
            if is_dirty {
                self.dirty_timestamps.insert(active, Instant::now());
            } else {
                self.dirty_timestamps.remove(&active);
            }
        }
    }

    fn tick_autosave(&mut self) {
        if !self.state.config.autosave {
            return;
        }
        let now = Instant::now();
        let due_paths = self
            .dirty_timestamps
            .iter()
            .filter(|(_, timestamp)| {
                now.duration_since(**timestamp).as_millis() >= self.state.config.autosave_ms as u128
            })
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();

        for path in due_paths {
            let _ = self.save_buffer(&path);
        }
    }

    fn ensure_active_cursor_visible(&mut self) {
        if let Some(buffer) = self.state.active_buffer_mut() {
            buffer.ensure_cursor_visible(
                self.last_ui.editor_inner.height as usize,
                self.last_ui.editor_inner.width as usize,
                self.last_ui.gutter_width as usize,
            );
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: String, sticky: bool) {
        self.state.status = StatusMessage { text, kind, sticky };
    }

    fn persist_session(&self) -> Result<()> {
        let session = SessionData {
            workspace: Some(self.state.workspace.root.clone()),
            open_files: self.state.open_order.clone(),
            active_file: self.state.active.clone(),
        };
        session.save()
    }

    fn current_picker_mut(&mut self) -> Option<&mut PickerState> {
        match self.state.overlay.as_mut() {
            Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
                Some(picker)
            }
            _ => None,
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

fn search_field_mut(search: &mut SearchState) -> &mut TextField {
    match search.active_field {
        SearchField::Query => &mut search.query,
        SearchField::Replacement => &mut search.replacement,
    }
}

fn palette_item(label: &str, detail: &str, command: PaletteCommand) -> PickerItem {
    PickerItem {
        label: label.to_string(),
        detail: detail.to_string(),
        action: PickerAction::Command(command),
    }
}

pub fn filtered_items(picker: &PickerState) -> Vec<PickerItem> {
    if picker.query.value.is_empty() {
        return picker.items.clone();
    }
    let query = picker.query.value.to_lowercase();
    picker
        .items
        .iter()
        .filter(|item| {
            item.label.to_lowercase().contains(&query)
                || item.detail.to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or_else(|| value.len())
}

pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn normalized_char(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Char(ch) => Some(ch.to_ascii_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::buffer::Position;

    use super::*;

    fn startup_for(
        workspace_root: PathBuf,
        file: PathBuf,
        recent_workspace: Option<PathBuf>,
    ) -> StartupContext {
        StartupContext {
            config: AppConfig::default(),
            workspace_root,
            initial_files: vec![file.clone()],
            active_file: Some(file),
            recent_workspace,
        }
    }

    #[test]
    fn cut_without_selection_is_noop() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("main.txt");
        fs::write(&file, "abc").unwrap();

        let mut app = App::new(startup_for(workspace.clone(), file.clone(), None), None).unwrap();
        let buffer = app.state.active_buffer_mut().unwrap();
        buffer.set_cursor(Position { line: 0, column: 2 }, false);

        app.cut_selection();

        assert_eq!(app.state.active_buffer().unwrap().text(), "abc");
    }

    #[test]
    fn open_recent_workspace_clears_stale_buffers() {
        let temp = TempDir::new().unwrap();
        let workspace_a = temp.path().join("workspace-a");
        let workspace_b = temp.path().join("workspace-b");
        fs::create_dir_all(&workspace_a).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();
        let file_a = workspace_a.join("a.txt");
        fs::write(&file_a, "alpha").unwrap();

        let mut app = App::new(
            startup_for(
                workspace_a.clone(),
                file_a.clone(),
                Some(workspace_b.clone()),
            ),
            None,
        )
        .unwrap();

        app.open_recent_workspace().unwrap();

        assert_eq!(
            app.state.workspace.root,
            fs::canonicalize(workspace_b).unwrap()
        );
        assert!(app.state.buffers.is_empty());
        assert!(app.state.open_order.is_empty());
        assert!(app.state.active.is_none());
    }

    #[test]
    fn toggle_hidden_files_reloads_workspace_and_keeps_active_visible() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let hidden = workspace.join(".env");
        fs::write(&hidden, "SECRET=1\n").unwrap();

        let mut app = App::new(startup_for(workspace.clone(), hidden.clone(), None), None).unwrap();
        app.last_ui.tree_inner.height = 6;

        assert_eq!(
            app.state.active.as_ref(),
            Some(&fs::canonicalize(&hidden).unwrap())
        );
        assert!(
            !app.state
                .workspace
                .visible_nodes()
                .iter()
                .any(|node| node.name == ".env")
        );

        app.toggle_hidden_files().unwrap();

        assert!(app.state.config.show_hidden);
        assert!(
            app.state
                .workspace
                .visible_nodes()
                .iter()
                .any(|node| node.name == ".env")
        );
        assert_eq!(
            app.state.workspace.selected_path(),
            app.state.active.as_ref()
        );
    }

    #[test]
    fn open_parent_directory_changes_browser_root_and_selects_previous_folder() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let workspace = parent.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("main.txt");
        fs::write(&file, "hello").unwrap();

        let mut app = App::new(startup_for(workspace.clone(), file, None), None).unwrap();
        app.last_ui.tree_inner.height = 8;

        app.open_parent_directory().unwrap();

        assert_eq!(app.state.workspace.root, fs::canonicalize(&parent).unwrap());
        let selected = app.state.workspace.selected_node().unwrap();
        assert_eq!(selected.name, "workspace");
        assert_eq!(selected.path, fs::canonicalize(&workspace).unwrap());
    }

    #[test]
    fn activating_directory_enters_that_directory() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let nested = workspace.join("src");
        fs::create_dir_all(&nested).unwrap();
        let file = workspace.join("notes.txt");
        fs::write(&file, "notes").unwrap();

        let mut app = App::new(startup_for(workspace.clone(), file, None), None).unwrap();
        app.last_ui.tree_inner.height = 8;
        app.state.focus = FocusArea::Tree;

        let src_index = app
            .state
            .workspace
            .visible_nodes()
            .iter()
            .position(|node| node.name == "src")
            .unwrap();
        app.state
            .workspace
            .set_selected_index(src_index, app.tree_viewport_height());

        app.activate_focused_item().unwrap();

        assert_eq!(app.state.workspace.root, fs::canonicalize(&nested).unwrap());
    }

    #[test]
    fn opening_binary_file_sets_error_instead_of_crashing() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let text_file = workspace.join("notes.txt");
        let binary_file = workspace.join("tool.exe");
        fs::write(&text_file, "notes").unwrap();
        fs::write(&binary_file, [0xFF, 0xFE, 0x00, 0x01]).unwrap();

        let mut app = App::new(startup_for(workspace, text_file, None), None).unwrap();
        app.open_buffer(binary_file).unwrap();

        assert_eq!(app.state.status.kind, StatusKind::Error);
        assert!(app.state.status.text.contains("unsupported or binary file"));
    }

    #[test]
    fn help_overlay_toggles_with_command() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("main.txt");
        fs::write(&file, "hello").unwrap();

        let mut app = App::new(startup_for(workspace, file, None), None).unwrap();

        app.dispatch(AppCommand::OpenHelp).unwrap();
        assert!(matches!(app.state.overlay, Some(OverlayState::Help)));

        app.dispatch(AppCommand::OpenHelp).unwrap();
        assert!(app.state.overlay.is_none());
    }

    #[test]
    fn close_buffer_command_closes_active_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("main.txt");
        fs::write(&file, "hello").unwrap();

        let mut app = App::new(startup_for(workspace, file.clone(), None), None).unwrap();

        app.dispatch(AppCommand::CloseBuffer).unwrap();

        assert!(app.state.active.is_none());
        assert!(app.state.buffers.is_empty());
        assert!(app.state.status.text.contains("Closed"));
    }

    #[test]
    fn cycle_tab_switches_active_buffer() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let first = workspace.join("one.txt");
        let second = workspace.join("two.txt");
        fs::write(&first, "one").unwrap();
        fs::write(&second, "two").unwrap();

        let mut app = App::new(startup_for(workspace, first.clone(), None), None).unwrap();
        app.open_buffer(second.clone()).unwrap();
        let canonical_first = fs::canonicalize(&first).unwrap();
        let canonical_second = fs::canonicalize(&second).unwrap();

        app.dispatch(AppCommand::CycleTab(-1)).unwrap();
        assert_eq!(app.state.active.as_ref(), Some(&canonical_first));

        app.dispatch(AppCommand::CycleTab(1)).unwrap();
        assert_eq!(app.state.active.as_ref(), Some(&canonical_second));
    }

    #[test]
    fn reopening_existing_buffer_preserves_tab_order() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let first = workspace.join("one.txt");
        let second = workspace.join("two.txt");
        let third = workspace.join("three.txt");
        fs::write(&first, "one").unwrap();
        fs::write(&second, "two").unwrap();
        fs::write(&third, "three").unwrap();

        let mut app = App::new(startup_for(workspace, first.clone(), None), None).unwrap();
        app.open_buffer(second.clone()).unwrap();
        app.open_buffer(third.clone()).unwrap();
        app.open_buffer(first.clone()).unwrap();

        assert_eq!(
            app.state.open_order,
            vec![
                fs::canonicalize(first).unwrap(),
                fs::canonicalize(second).unwrap(),
                fs::canonicalize(third).unwrap(),
            ]
        );
    }
}
