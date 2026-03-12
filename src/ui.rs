use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::path::PathBuf;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, Theme};
use syntect::parsing::SyntaxSet;

use crate::app::{
    AppState, FocusArea, OverlayState, PickerState, SearchField, SearchMode, SearchState,
    StatusKind, filtered_items, relative_path,
};
use crate::buffer::{Position, TextBuffer};
use crate::syntax::resolve_syntax;

#[derive(Debug, Clone, Default)]
pub struct UiMetadata {
    pub tree_area: Rect,
    pub tree_inner: Rect,
    pub editor_area: Rect,
    pub editor_inner: Rect,
    pub tab_bar_area: Rect,
    pub top_bar_area: Rect,
    pub status_area: Rect,
    pub gutter_width: u16,
    pub tree_scroll: usize,
    pub tab_hits: Vec<TabHitbox>,
}

#[derive(Debug, Clone)]
pub struct TabHitbox {
    pub area: Rect,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct Palette {
    background: Color,
    top_bar: Color,
    sidebar: Color,
    editor: Color,
    overlay: Color,
    border: Color,
    text: Color,
    muted: Color,
    accent: Color,
    accent_soft: Color,
    warning: Color,
    error: Color,
    line_number: Color,
    current_line: Color,
    selection: Color,
}

pub fn render(
    frame: &mut Frame,
    state: &AppState,
    syntax_set: &SyntaxSet,
    theme: &Theme,
) -> UiMetadata {
    let palette = palette();
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );

    let [top_bar_area, body, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(area);
    let [tree_area, editor_area] = if state.show_sidebar {
        Layout::horizontal([Constraint::Length(30), Constraint::Min(30)]).areas(body)
    } else {
        [Rect::default(), body]
    };

    let tree_inner = if state.show_sidebar {
        Rect {
            x: tree_area.x,
            y: tree_area.y + 1,
            width: tree_area.width,
            height: tree_area.height.saturating_sub(1),
        }
    } else {
        Rect::default()
    };

    let editor_inner = Rect {
        x: editor_area.x,
        y: editor_area.y + 2,
        width: editor_area.width,
        height: editor_area.height.saturating_sub(2),
    };

    let mut metadata = UiMetadata {
        tree_area,
        tree_inner,
        editor_area,
        editor_inner,
        tab_bar_area: Rect {
            x: editor_area.x,
            y: editor_area.y,
            width: editor_area.width,
            height: 1,
        },
        top_bar_area,
        status_area,
        gutter_width: 0,
        tree_scroll: 0,
        tab_hits: Vec::new(),
    };

    render_top_bar(frame, top_bar_area, state, palette);
    if state.show_sidebar {
        metadata.tree_scroll = render_tree(frame, tree_area, tree_inner, state, palette);
    }
    let (gutter_width, tab_hits) = render_editor(
        frame,
        editor_area,
        editor_inner,
        state,
        syntax_set,
        theme,
        palette,
    );
    metadata.gutter_width = gutter_width;
    metadata.tab_hits = tab_hits;
    render_status(frame, status_area, state, palette);
    render_overlay(frame, state, &metadata, palette);
    metadata
}

fn render_top_bar(frame: &mut Frame, area: Rect, state: &AppState, palette: Palette) {
    let workspace = state.workspace.root.display().to_string();
    let file = state
        .active_buffer()
        .and_then(|buffer| buffer.path.as_ref())
        .map(|path| relative_path(&state.workspace.root, path))
        .unwrap_or_else(|| "No file".to_string());
    let dirty = state.active_buffer().is_some_and(|buffer| buffer.dirty);
    let line = Line::from(vec![
        Span::styled(
            " workspace ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(
            workspace,
            Style::default().fg(palette.text).bg(palette.top_bar),
        ),
        Span::styled(
            "   file ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(file, Style::default().fg(palette.text).bg(palette.top_bar)),
        Span::styled(
            if dirty { "   modified" } else { "" },
            Style::default().fg(palette.accent).bg(palette.top_bar),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(palette.top_bar)),
        area,
    );
}

fn render_tree(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    state: &AppState,
    palette: Palette,
) -> usize {
    frame.render_widget(
        Block::default()
            .style(Style::default().bg(palette.sidebar))
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(palette.border)),
        area,
    );
    if inner.height == 0 {
        return 0;
    }

    let header_style = Style::default().bg(palette.sidebar).fg(palette.muted);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", header_style),
            Span::styled(
                &state.workspace.root_name,
                Style::default().bg(palette.sidebar).fg(palette.text),
            ),
            Span::styled("  ", header_style),
            Span::styled(
                if state.config.show_hidden {
                    "all files"
                } else {
                    "filtered"
                },
                header_style,
            ),
        ])),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    let nodes = state.workspace.visible_nodes();
    let selected = state.workspace.selected_index();
    let scroll = state.workspace.scroll_offset();
    let active_path = state.active.clone();
    let lines = nodes
        .iter()
        .skip(scroll)
        .take(inner.height as usize)
        .enumerate()
        .map(|(offset, node)| {
            let selected_row = scroll + offset == selected;
            let is_active =
                !node.is_parent_link && active_path.as_ref().is_some_and(|path| path == &node.path);
            let is_open =
                !node.is_parent_link && state.open_order.iter().any(|path| path == &node.path);
            let is_dirty = state
                .buffers
                .get(&node.path)
                .is_some_and(|buffer| buffer.dirty)
                && !node.is_parent_link;
            let marker = if selected_row { ">" } else { " " };
            let indent = "  ".repeat(node.depth);
            let prefix = if node.is_parent_link {
                "< "
            } else if node.is_dir {
                "/ "
            } else {
                "  "
            };
            let status = match (is_dirty, is_open, is_active) {
                (true, _, _) => "*",
                (false, true, true) => "@",
                (false, true, false) => "o",
                _ => " ",
            };
            let available = inner.width.saturating_sub(2) as usize;
            let content = truncate_text(
                &format!("{indent}{prefix}{status} {}", node.name),
                available,
            );
            let style = if selected_row {
                Style::default()
                    .bg(palette.selection)
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD)
            } else if node.is_parent_link {
                Style::default().bg(palette.sidebar).fg(palette.muted)
            } else if is_dirty {
                Style::default().bg(palette.sidebar).fg(palette.warning)
            } else if is_active {
                Style::default().bg(palette.sidebar).fg(palette.accent)
            } else if is_open {
                Style::default().bg(palette.sidebar).fg(palette.accent_soft)
            } else {
                Style::default().bg(palette.sidebar).fg(palette.text)
            };
            let marker_style = if selected_row {
                Style::default().bg(palette.selection).fg(palette.accent)
            } else {
                Style::default().bg(palette.sidebar).fg(palette.sidebar)
            };
            Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(content, style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(palette.sidebar))
            .wrap(Wrap { trim: false }),
        inner,
    );
    scroll
}

fn render_editor(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    state: &AppState,
    syntax_set: &SyntaxSet,
    theme: &Theme,
    palette: Palette,
) -> (u16, Vec<TabHitbox>) {
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.editor)),
        area,
    );

    let tab_hits = render_editor_header(frame, area, state, palette);

    let Some(buffer) = state.active_buffer() else {
        let placeholder = Paragraph::new("Open a file from the sidebar or use Ctrl+P.")
            .style(Style::default().fg(palette.muted).bg(palette.editor))
            .wrap(Wrap { trim: false });
        frame.render_widget(placeholder, inner);
        return (0, tab_hits);
    };

    let gutter_width = if state.config.line_numbers {
        buffer.line_count().to_string().len() as u16 + 2
    } else {
        0
    };
    let lines = render_buffer_lines(
        buffer,
        inner,
        state,
        syntax_set,
        theme,
        gutter_width,
        palette,
    );
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette.editor)),
        inner,
    );

    if state.overlay.is_none() && state.focus == FocusArea::Editor {
        set_editor_cursor(frame, buffer, inner, gutter_width);
    }

    (gutter_width, tab_hits)
}

fn render_buffer_lines(
    buffer: &TextBuffer,
    area: Rect,
    state: &AppState,
    syntax_set: &SyntaxSet,
    theme: &Theme,
    gutter_width: u16,
    palette: Palette,
) -> Vec<Line<'static>> {
    if area.height == 0 {
        return Vec::new();
    }
    let end_line = (buffer.scroll_y + area.height as usize).min(buffer.line_count());
    let syntax = resolve_syntax(syntax_set, buffer);
    let mut highlighter = HighlightLines::new(syntax, theme);
    let selection = buffer.selection_bounds();
    let mut output = Vec::new();

    for index in 0..end_line {
        let line_text = buffer.line_text(index);
        let syntax_spans = syntax_line_spans(
            &mut highlighter,
            syntax_set,
            &line_text,
            buffer.scroll_x,
            area.width,
            gutter_width,
            palette,
        );
        let spans =
            if let Some(selected) = selection.and_then(|bounds| line_selection(bounds, index)) {
                selected_line_spans(
                    &line_text,
                    selected,
                    buffer.scroll_x,
                    area.width,
                    gutter_width,
                    palette,
                )
            } else {
                apply_line_background(
                    syntax_spans,
                    if buffer.cursor.line == index {
                        palette.current_line
                    } else {
                        palette.editor
                    },
                )
            };
        if index >= buffer.scroll_y {
            let mut full_spans = Vec::new();
            if state.config.line_numbers {
                let line_no = format!(
                    "{:>width$} ",
                    index + 1,
                    width = gutter_width.saturating_sub(1) as usize
                );
                let line_style = if buffer.cursor.line == index {
                    Style::default()
                        .fg(palette.accent)
                        .bg(palette.current_line)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.line_number).bg(palette.editor)
                };
                full_spans.push(Span::styled(line_no, line_style));
            }
            full_spans.extend(spans);
            output.push(Line::from(full_spans));
        }
    }

    while output.len() < area.height as usize {
        output.push(Line::from(""));
    }
    output
}

fn syntax_line_spans(
    highlighter: &mut HighlightLines<'_>,
    syntax_set: &SyntaxSet,
    text: &str,
    scroll_x: usize,
    width: u16,
    gutter_width: u16,
    palette: Palette,
) -> Vec<Span<'static>> {
    let available = width.saturating_sub(gutter_width) as usize;
    let cropped = crop_text(text, scroll_x, available);
    let Ok(regions) = highlighter.highlight_line(text, syntax_set) else {
        return vec![Span::styled(
            cropped,
            Style::default().fg(palette.text).bg(palette.editor),
        )];
    };

    let mut char_pos = 0usize;
    let mut spans = Vec::new();
    let viewport_end = scroll_x.saturating_add(available);
    for (style, part) in regions {
        let part_len = part.chars().count();
        let part_start = char_pos;
        let part_end = char_pos + part_len;
        char_pos = part_end;
        if part_end <= scroll_x || part_start >= viewport_end {
            continue;
        }
        let start = scroll_x.saturating_sub(part_start);
        let end = part_len.min(viewport_end.saturating_sub(part_start));
        if start >= end {
            continue;
        }
        let text = part
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>();
        spans.push(Span::styled(text, convert_style(style, palette.editor)));
    }

    if spans.is_empty() {
        spans.push(Span::styled(
            cropped,
            Style::default().fg(palette.text).bg(palette.editor),
        ));
    }
    spans
}

fn selected_line_spans(
    text: &str,
    selected: (usize, usize),
    scroll_x: usize,
    width: u16,
    gutter_width: u16,
    palette: Palette,
) -> Vec<Span<'static>> {
    let available = width.saturating_sub(gutter_width) as usize;
    let visible = text
        .chars()
        .skip(scroll_x)
        .take(available)
        .collect::<Vec<_>>();
    let mut spans = Vec::new();
    for (offset, ch) in visible.iter().enumerate() {
        let actual = scroll_x + offset;
        let style = if actual >= selected.0 && actual < selected.1 {
            Style::default().bg(palette.selection).fg(palette.text)
        } else {
            Style::default().bg(palette.editor).fg(palette.text)
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            "",
            Style::default().bg(palette.editor).fg(palette.text),
        ));
    }
    spans
}

fn render_editor_header(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    palette: Palette,
) -> Vec<TabHitbox> {
    let tabs_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let header = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: 1,
    };
    let file = state
        .active_buffer()
        .map(|buffer| {
            let name = buffer.display_name();
            let rel = buffer
                .path
                .as_ref()
                .map(|path| relative_path(&state.workspace.root, path))
                .unwrap_or_else(|| name.clone());
            (name, rel, buffer.dirty)
        })
        .unwrap_or_else(|| ("No file".to_string(), "".to_string(), false));
    let focus_marker = if state.focus == FocusArea::Editor {
        "editor"
    } else {
        "browser"
    };
    let (tab_line, tab_hits) = render_tab_line(state, tabs_area, palette);
    frame.render_widget(Paragraph::new(tab_line), tabs_area);
    let line = Line::from(vec![
        Span::styled("  ", Style::default().bg(palette.editor)),
        Span::styled(
            file.0,
            Style::default()
                .fg(palette.text)
                .bg(palette.editor)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if file.2 { "  modified" } else { "" },
            Style::default().fg(palette.warning).bg(palette.editor),
        ),
        Span::styled("  ", Style::default().bg(palette.editor)),
        Span::styled(
            file.1,
            Style::default().fg(palette.muted).bg(palette.editor),
        ),
        Span::styled("  ", Style::default().bg(palette.editor)),
        Span::styled(
            focus_marker,
            Style::default().fg(palette.accent_soft).bg(palette.editor),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), header);
    tab_hits
}

fn render_tab_line(
    state: &AppState,
    area: Rect,
    palette: Palette,
) -> (Line<'static>, Vec<TabHitbox>) {
    let mut spans = Vec::new();
    let mut tab_hits = Vec::new();
    let mut cursor_x = area.x;
    let right = area.right();

    if state.open_order.is_empty() {
        spans.push(Span::styled(
            "  no open files",
            Style::default().fg(palette.muted).bg(palette.editor),
        ));
        return (Line::from(spans), tab_hits);
    }

    for path in &state.open_order {
        if cursor_x >= right {
            break;
        }
        let is_active = state.active.as_ref().is_some_and(|active| active == path);
        let is_dirty = state.buffers.get(path).is_some_and(|buffer| buffer.dirty);
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let label = if is_dirty {
            format!(" {} * ", truncate_text(&title, 20))
        } else {
            format!(" {} ", truncate_text(&title, 22))
        };
        let available = right.saturating_sub(cursor_x) as usize;
        if available == 0 {
            break;
        }
        let rendered = truncate_text(&label, available);
        let width = rendered.chars().count() as u16;
        let style = if is_active {
            Style::default()
                .fg(palette.text)
                .bg(palette.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.muted).bg(palette.top_bar)
        };
        spans.push(Span::styled(rendered.clone(), style));
        if width > 0 {
            tab_hits.push(TabHitbox {
                area: Rect {
                    x: cursor_x,
                    y: area.y,
                    width,
                    height: 1,
                },
                path: path.clone(),
            });
        }
        cursor_x = cursor_x.saturating_add(width);
        if cursor_x < right {
            spans.push(Span::styled(
                " ",
                Style::default().bg(palette.editor).fg(palette.border),
            ));
            cursor_x = cursor_x.saturating_add(1);
        }
    }

    (Line::from(spans), tab_hits)
}

fn render_status(frame: &mut Frame, area: Rect, state: &AppState, palette: Palette) {
    let focus = match state.focus {
        FocusArea::Tree => "tree",
        FocusArea::Editor => "editor",
    };
    let path = state
        .active_buffer()
        .and_then(|buffer| buffer.path.as_ref())
        .map(|path| relative_path(&state.workspace.root, path))
        .unwrap_or_else(|| "no file".to_string());
    let dirty = state.active_buffer().is_some_and(|buffer| buffer.dirty);
    let cursor = state
        .active_buffer()
        .map(|buffer| {
            format!(
                "ln {}  col {}",
                buffer.cursor.line + 1,
                buffer.cursor.column + 1
            )
        })
        .unwrap_or_else(|| "ln -  col -".to_string());
    let style = match state.status.kind {
        StatusKind::Info => Style::default().fg(palette.text).bg(palette.top_bar),
        StatusKind::Warning => Style::default().fg(palette.warning).bg(palette.top_bar),
        StatusKind::Error => Style::default().fg(palette.error).bg(palette.top_bar),
    };
    let [status_line, hint_line] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let separator = Span::styled(
        " | ",
        Style::default().fg(palette.border).bg(palette.top_bar),
    );
    let first = Line::from(vec![
        Span::styled(
            " mode ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(focus, Style::default().fg(palette.text).bg(palette.top_bar)),
        separator.clone(),
        Span::styled(
            " state ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(
            if dirty { "modified" } else { "saved" },
            if dirty {
                Style::default().fg(palette.warning).bg(palette.top_bar)
            } else {
                Style::default().fg(palette.accent).bg(palette.top_bar)
            },
        ),
        separator.clone(),
        Span::styled(
            " cursor ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(
            cursor.replace("ln ", "").replace("  col ", ":"),
            Style::default().fg(palette.text).bg(palette.top_bar),
        ),
        separator,
        Span::styled(
            " help ",
            Style::default().fg(palette.muted).bg(palette.top_bar),
        ),
        Span::styled(
            "F1",
            Style::default().fg(palette.accent).bg(palette.top_bar),
        ),
    ]);

    let second_width = hint_line.width.max(1) as usize;
    let file_width = second_width.saturating_mul(2) / 5;
    let status_width = second_width.saturating_sub(file_width + 14);
    let second = Line::from(vec![
        Span::styled(
            " file ",
            Style::default().fg(palette.muted).bg(palette.background),
        ),
        Span::styled(
            truncate_text(&path, file_width.max(8)),
            Style::default().fg(palette.text).bg(palette.background),
        ),
        Span::styled(
            " | ",
            Style::default().fg(palette.border).bg(palette.background),
        ),
        Span::styled(
            " status ",
            Style::default().fg(palette.muted).bg(palette.background),
        ),
        Span::styled(
            truncate_text(&state.status.text, status_width.max(10)),
            match state.status.kind {
                StatusKind::Info => Style::default().fg(palette.text).bg(palette.background),
                StatusKind::Warning => Style::default().fg(palette.warning).bg(palette.background),
                StatusKind::Error => Style::default().fg(palette.error).bg(palette.background),
            },
        ),
    ]);

    frame.render_widget(Paragraph::new(first).style(style), status_line);
    frame.render_widget(
        Paragraph::new(second).style(Style::default().bg(palette.background)),
        hint_line,
    );
}

fn render_overlay(frame: &mut Frame, state: &AppState, metadata: &UiMetadata, palette: Palette) {
    match state.overlay.as_ref() {
        Some(OverlayState::QuickOpen(picker)) | Some(OverlayState::CommandPalette(picker)) => {
            render_picker_overlay(frame, picker, metadata, palette)
        }
        Some(OverlayState::Search(search)) => {
            render_search_overlay(frame, search, metadata, palette)
        }
        Some(OverlayState::Help) => render_help_overlay(frame, palette),
        Some(OverlayState::Prompt(prompt)) => {
            let area = centered_rect(60, 5, frame.area());
            frame.render_widget(Clear, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border))
                .style(Style::default().bg(palette.overlay))
                .title(format!(" {} ", prompt.title));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(prompt.input.value.clone())
                    .style(Style::default().fg(palette.text).bg(palette.overlay)),
                inner,
            );
            let cursor_x = inner.x + prompt.input.cursor as u16;
            frame.set_cursor_position((cursor_x.min(inner.right().saturating_sub(1)), inner.y));
        }
        None => {}
    }
}

fn render_picker_overlay(
    frame: &mut Frame,
    picker: &PickerState,
    metadata: &UiMetadata,
    palette: Palette,
) {
    let area = centered_rect(70, 12, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border))
        .style(Style::default().bg(palette.overlay))
        .title(format!(" {} ", picker.title));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [query_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    frame.render_widget(
        Paragraph::new(format!("> {}", picker.query.value))
            .style(Style::default().fg(palette.accent).bg(palette.overlay)),
        query_area,
    );

    let filtered = filtered_items(picker);
    let scroll = picker
        .selected
        .saturating_sub(list_area.height.saturating_sub(1) as usize);
    let lines = filtered
        .iter()
        .skip(scroll)
        .take(list_area.height as usize)
        .enumerate()
        .map(|(offset, item)| {
            let is_selected = scroll + offset == picker.selected;
            let style = if is_selected {
                Style::default()
                    .bg(palette.selection)
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(palette.overlay).fg(palette.text)
            };
            Line::from(vec![
                Span::styled(
                    if is_selected { "> " } else { "  " },
                    if is_selected {
                        Style::default().bg(palette.selection).fg(palette.accent)
                    } else {
                        Style::default().bg(palette.overlay).fg(palette.overlay)
                    },
                ),
                Span::styled(item.label.clone(), style),
                Span::styled(
                    format!("  {}", item.detail),
                    Style::default()
                        .bg(style.bg.unwrap_or(palette.overlay))
                        .fg(palette.muted),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(palette.overlay))
            .wrap(Wrap { trim: false }),
        list_area,
    );
    let cursor_x = query_area.x + 2 + picker.query.cursor as u16;
    let _ = metadata;
    frame.set_cursor_position((
        cursor_x.min(query_area.right().saturating_sub(1)),
        query_area.y,
    ));
}

fn render_search_overlay(
    frame: &mut Frame,
    search: &SearchState,
    _metadata: &UiMetadata,
    palette: Palette,
) {
    let height = if search.mode == SearchMode::Replace {
        7
    } else {
        5
    };
    let area = centered_rect(60, height, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border))
        .style(Style::default().bg(palette.overlay))
        .title(if search.mode == SearchMode::Replace {
            " Replace "
        } else {
            " Find "
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let find_style = if search.active_field == SearchField::Query {
        Style::default().fg(palette.accent).bg(palette.overlay)
    } else {
        Style::default().fg(palette.text).bg(palette.overlay)
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!("Find: {}", search.query.value)).style(find_style),
        chunks[0],
    );

    let mut cursor_y = chunks[0].y;
    let mut cursor_x = chunks[0].x + 6 + search.query.cursor as u16;

    if search.mode == SearchMode::Replace {
        let replace_style = if search.active_field == SearchField::Replacement {
            Style::default().fg(palette.accent).bg(palette.overlay)
        } else {
            Style::default().fg(palette.text).bg(palette.overlay)
        };
        frame.render_widget(
            Paragraph::new(format!("Replace: {}", search.replacement.value)).style(replace_style),
            chunks[1],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Case sensitive: {} | Enter next | Ctrl+R replace | Ctrl+A all | F6 toggle case",
                if search.case_sensitive { "on" } else { "off" }
            ))
            .style(Style::default().fg(palette.muted).bg(palette.overlay)),
            chunks[2],
        );
        if search.active_field == SearchField::Replacement {
            cursor_y = chunks[1].y;
            cursor_x = chunks[1].x + 9 + search.replacement.cursor as u16;
        }
    } else {
        frame.render_widget(
            Paragraph::new(format!(
                "Case sensitive: {} | Enter next | F6 toggle case",
                if search.case_sensitive { "on" } else { "off" }
            ))
            .style(Style::default().fg(palette.muted).bg(palette.overlay)),
            chunks[1],
        );
    }

    frame.set_cursor_position((cursor_x.min(inner.right().saturating_sub(1)), cursor_y));
}

fn render_help_overlay(frame: &mut Frame, palette: Palette) {
    let area = centered_rect(68, 12, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border))
        .style(Style::default().bg(palette.overlay))
        .title(" Shortcuts ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(vec![
            Span::styled("F1", Style::default().fg(palette.accent)),
            Span::styled(
                "  show or hide this overview",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+P", Style::default().fg(palette.accent)),
            Span::styled(
                "  quick open by file name",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+W", Style::default().fg(palette.accent)),
            Span::styled(
                "  close the current file",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl+Tab / Ctrl+Left / Ctrl+Right",
                Style::default().fg(palette.accent),
            ),
            Span::styled(
                "  move between open file tabs",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+Shift+P", Style::default().fg(palette.accent)),
            Span::styled("  command palette", Style::default().fg(palette.text)),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+F / Ctrl+H", Style::default().fg(palette.accent)),
            Span::styled(
                "  find and replace in the current file",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+G", Style::default().fg(palette.accent)),
            Span::styled("  go to line", Style::default().fg(palette.text)),
        ]),
        Line::from(vec![
            Span::styled("F8 or Ctrl+.", Style::default().fg(palette.accent)),
            Span::styled(
                "  show or hide filtered files",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Enter / Right", Style::default().fg(palette.accent)),
            Span::styled(
                "  open file or enter folder in the browser",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Left / Backspace", Style::default().fg(palette.accent)),
            Span::styled(
                "  go up to the parent folder",
                Style::default().fg(palette.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("Esc", Style::default().fg(palette.accent)),
            Span::styled(
                "  close this panel or any active surface",
                Style::default().fg(palette.text),
            ),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(palette.text).bg(palette.overlay))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn set_editor_cursor(frame: &mut Frame, buffer: &TextBuffer, inner: Rect, gutter_width: u16) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let visible_line = buffer.cursor.line.saturating_sub(buffer.scroll_y);
    let visible_col = buffer.cursor.column.saturating_sub(buffer.scroll_x);
    if visible_line >= inner.height as usize {
        return;
    }
    let x = inner.x + gutter_width + visible_col as u16;
    let y = inner.y + visible_line as u16;
    if x < inner.right() && y < inner.bottom() {
        frame.set_cursor_position((x, y));
    }
}

fn line_selection(selection: (Position, Position), line: usize) -> Option<(usize, usize)> {
    let (start, end) = selection;
    if line < start.line || line > end.line {
        return None;
    }
    let line_start = if line == start.line { start.column } else { 0 };
    let line_end = if line == end.line {
        end.column
    } else {
        usize::MAX
    };
    Some((line_start, line_end))
}

fn crop_text(text: &str, scroll_x: usize, width: usize) -> String {
    text.chars().skip(scroll_x).take(width).collect::<String>()
}

fn truncate_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return text.to_string();
    }
    if width == 1 {
        return ".".to_string();
    }
    let mut output = chars.into_iter().take(width - 1).collect::<String>();
    output.push('.');
    output
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50u16.saturating_sub(height * 100 / area.height.max(1)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn apply_line_background(spans: Vec<Span<'static>>, background: Color) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let style = span.style.patch(Style::default().bg(background));
            Span::styled(span.content.to_string(), style)
        })
        .collect()
}

fn convert_style(style: SyntectStyle, background: Color) -> Style {
    let (red, green, blue) =
        readable_rgb(style.foreground.r, style.foreground.g, style.foreground.b);
    Style::default()
        .fg(Color::Rgb(red, green, blue))
        .bg(background)
        .add_modifier(
            if style
                .font_style
                .contains(syntect::highlighting::FontStyle::BOLD)
            {
                Modifier::BOLD
            } else {
                Modifier::empty()
            },
        )
}

fn readable_rgb(red: u8, green: u8, blue: u8) -> (u8, u8, u8) {
    let luminance = (0.2126 * red as f32) + (0.7152 * green as f32) + (0.0722 * blue as f32);
    if luminance >= 108.0 {
        return (red, green, blue);
    }
    if luminance <= 1.0 {
        return (190, 196, 204);
    }

    let scale = 108.0 / luminance;
    (
        ((red as f32 * scale).min(235.0)) as u8,
        ((green as f32 * scale).min(235.0)) as u8,
        ((blue as f32 * scale).min(235.0)) as u8,
    )
}

fn palette() -> Palette {
    Palette {
        background: Color::Rgb(17, 19, 22),
        top_bar: Color::Rgb(26, 29, 34),
        sidebar: Color::Rgb(22, 24, 29),
        editor: Color::Rgb(15, 17, 20),
        overlay: Color::Rgb(29, 32, 37),
        border: Color::Rgb(60, 64, 72),
        text: Color::Rgb(228, 231, 236),
        muted: Color::Rgb(172, 177, 185),
        accent: Color::Rgb(144, 194, 124),
        accent_soft: Color::Rgb(112, 157, 212),
        warning: Color::Rgb(214, 180, 104),
        error: Color::Rgb(219, 113, 113),
        line_number: Color::Rgb(118, 125, 136),
        current_line: Color::Rgb(26, 30, 36),
        selection: Color::Rgb(49, 57, 66),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    use crate::app::StatusMessage;
    use crate::buffer::TextBuffer;
    use crate::config::AppConfig;
    use crate::workspace::WorkspaceState;

    use super::*;

    #[test]
    fn renders_default_layout() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let workspace = WorkspaceState::load(std::env::current_dir().unwrap(), true).unwrap();
        let mut buffers = BTreeMap::new();
        let path = std::env::current_dir().unwrap().join("Cargo.toml");
        buffers.insert(
            path.clone(),
            TextBuffer::with_text(Some(path.clone()), "fn main() {}\n"),
        );
        let state = AppState {
            config: AppConfig::default(),
            workspace,
            focus: FocusArea::Editor,
            overlay: None,
            show_sidebar: true,
            status: StatusMessage::default(),
            buffers,
            open_order: vec![path.clone()],
            active: Some(path),
            recent_workspace: None,
        };
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme = ThemeSet::load_defaults().themes["InspiredGitHub"].clone();

        let completed = terminal
            .draw(|frame| {
                let _ = render(frame, &state, &syntax_set, &theme);
            })
            .unwrap();

        assert_eq!(completed.area.width, 100);
    }

    #[test]
    fn narrow_editor_render_does_not_panic() {
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let workspace = WorkspaceState::load(std::env::current_dir().unwrap(), true).unwrap();
        let mut buffers = BTreeMap::new();
        let path = std::env::current_dir().unwrap().join("main.rs");
        buffers.insert(
            path.clone(),
            TextBuffer::with_text(
                Some(path.clone()),
                "let highlighted_identifier = 1234567890;\n",
            ),
        );
        let state = AppState {
            config: AppConfig::default(),
            workspace,
            focus: FocusArea::Editor,
            overlay: None,
            show_sidebar: false,
            status: StatusMessage::default(),
            buffers,
            open_order: vec![path.clone()],
            active: Some(path),
            recent_workspace: None,
        };
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme = ThemeSet::load_defaults().themes["InspiredGitHub"].clone();

        let completed = terminal
            .draw(|frame| {
                let _ = render(frame, &state, &syntax_set, &theme);
            })
            .unwrap();

        assert_eq!(completed.area.width, 20);
    }
}
