#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    WordLeft,
    WordRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseActionKind {
    Down,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseAction {
    pub kind: MouseActionKind,
    pub column: u16,
    pub row: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    None,
    Quit,
    Save,
    PromptSaveAs,
    OpenQuickOpen,
    OpenCommandPalette,
    OpenHelp,
    OpenFind,
    OpenReplace,
    PromptGotoLine,
    ToggleSidebar,
    OpenParentDirectory,
    ReloadWorkspace,
    OpenRecentWorkspace,
    CloseBuffer,
    CycleTab(i32),
    RevertBuffer,
    FocusNext,
    FocusPrev,
    TreeMove(i32),
    TreeExpand,
    TreeCollapse,
    TreeScroll(i32),
    TreePage(i32),
    PickerMove(i32),
    ActivateFocusedItem,
    CancelOverlay,
    OverlayBackspace,
    OverlayDelete,
    OverlayMoveLeft,
    OverlayMoveRight,
    OverlaySubmit,
    OverlayTab,
    OverlayToggleCase,
    ReplaceCurrent,
    ReplaceAll,
    Search(SearchDirection),
    ToggleHiddenFiles,
    EditMotion {
        motion: Motion,
        extend_selection: bool,
    },
    InsertChar(char),
    InsertNewline,
    InsertTab,
    Backspace,
    DeleteForward,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    Mouse(MouseAction),
}
