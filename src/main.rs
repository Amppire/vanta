mod document;

use std::{ops::Range, path::PathBuf};

use document::Document;
use gpui::{
    App, Application, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, Style, TextRun, UTF16Selection, UnderlineStyle, Window, WindowBounds,
    WindowOptions, actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

const BACKGROUND: u32 = 0x00fb_faf7;
const BORDER: u32 = 0x00dd_dbd5;
const MUTED: u32 = 0x0073_746f;
const SELECTION: u32 = 0x334f_73c8;
const TEXT: u32 = 0x0028_2927;
const LINE_HEIGHT: f32 = 22.0;

actions!(
    editor,
    [
        Backspace,
        Copy,
        Cut,
        Delete,
        Down,
        End,
        Home,
        Left,
        Newline,
        Paste,
        Right,
        Save,
        SelectAll,
        SelectDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        Up,
    ]
);

struct Editor {
    document: Document,
    focus: FocusHandle,
    selection: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layouts: Vec<LineLayout>,
    bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    status: String,
}

struct LineLayout {
    start: usize,
    end: usize,
    shaped: ShapedLine,
}

impl Editor {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self {
            document,
            focus: cx.focus_handle(),
            selection: 0..0,
            selection_reversed: false,
            marked_range: None,
            layouts: Vec::new(),
            bounds: None,
            selecting: false,
            status: "Ready".into(),
        }
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selection = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.selection_reversed = !self.selection_reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.document
            .text()
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.document
            .text()
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.document.text().len())
    }

    fn line_and_column(&self, offset: usize) -> (usize, usize) {
        let ranges = line_ranges(self.document.text());
        let line = ranges
            .iter()
            .position(|range| offset <= range.end)
            .unwrap_or(ranges.len() - 1);
        let column = self.document.text()[ranges[line].start..offset]
            .graphemes(true)
            .count();
        (line, column)
    }

    fn offset_on_line(&self, line: usize, column: usize) -> usize {
        let ranges = line_ranges(self.document.text());
        let range = &ranges[line.min(ranges.len() - 1)];
        self.document.text()[range.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map_or(range.end, |(offset, _)| range.start + offset)
    }

    fn vertical_offset(&self, delta: isize) -> usize {
        let (line, column) = self.line_and_column(self.cursor());
        self.offset_on_line(line.saturating_add_signed(delta), column)
    }

    fn line_start(&self) -> usize {
        let (line, _) = self.line_and_column(self.cursor());
        line_ranges(self.document.text())[line].start
    }

    fn line_end(&self) -> usize {
        let (line, _) = self.line_and_column(self.cursor());
        line_ranges(self.document.text())[line].end
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let cursor = range.start + text.len();
        self.document.replace(range, text);
        self.selection = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.status = "Modified".into();
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selection.is_empty() {
            self.previous_boundary(self.cursor())
        } else {
            self.selection.start
        };
        self.move_to(offset, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selection.is_empty() {
            self.next_boundary(self.cursor())
        } else {
            self.selection.end
        };
        self.move_to(offset, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_offset(-1), cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_offset(1), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_offset(-1), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_offset(1), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.document.text().len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            let previous = self.previous_boundary(self.cursor());
            if previous == self.cursor() {
                return;
            }
            self.selection.start = previous;
        }
        self.replace(self.selection.clone(), "", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                return;
            }
            self.selection.end = next;
        }
        self.replace(self.selection.clone(), "", cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.replace(self.selection.clone(), "\n", cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.document.text()[self.selection.clone()].to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.document.text()[self.selection.clone()].to_owned(),
            ));
            self.replace(self.selection.clone(), "", cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace(self.selection.clone(), &text, cx);
        }
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        self.status = match self.document.save() {
            Ok(()) => "Saved".into(),
            Err(error) => format!("Save failed: {error}"),
        };
        cx.notify();
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.bounds else {
            return 0;
        };
        let line = (((position.y - bounds.top()) / px(LINE_HEIGHT)) as usize)
            .min(self.layouts.len().saturating_sub(1));
        self.layouts.get(line).map_or(0, |layout| {
            layout.start
                + layout
                    .shaped
                    .closest_index_for_x(position.x - bounds.left())
        })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        let offset = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for character in self.document.text().chars() {
            if utf16 >= offset {
                break;
            }
            utf8 += character.len_utf8();
            utf16 += character.len_utf16();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.document.text()[..offset].encode_utf16().count()
    }

    fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(range);
        actual_range.replace(self.range_to_utf16(range.clone()));
        Some(self.document.text()[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(self.selection.clone()),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .clone()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.replace(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = range.start;
        self.document.replace(range, text);
        self.marked_range = (!text.is_empty()).then_some(start..start + text.len());
        self.selection = selected.map_or_else(
            || start + text.len()..start + text.len(),
            |range| {
                start + utf16_to_utf8(text, range.start)..start + utf16_to_utf8(text, range.end)
            },
        );
        self.status = "Modified".into();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.bounds?;
        let range = self.range_from_utf16(range);
        let line = self
            .layouts
            .iter()
            .position(|line| range.start <= line.end)?;
        let layout = &self.layouts[line];
        Some(Bounds::from_corners(
            point(
                bounds.left() + layout.shaped.x_for_index(range.start - layout.start),
                bounds.top() + line_y(line),
            ),
            point(
                bounds.left()
                    + layout
                        .shaped
                        .x_for_index(range.end.min(layout.end) - layout.start),
                bounds.top() + line_y(line + 1),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_position(point)))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .document
            .path()
            .and_then(|path| path.file_name())
            .map_or_else(|| "Untitled".into(), |name| name.to_string_lossy());
        let title = if self.document.is_dirty() {
            format!("{name} •")
        } else {
            name.into_owned()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .key_context("Editor")
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::save))
            .child(
                div()
                    .h(px(38.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(title)
                    .child(div().text_sm().text_color(rgb(MUTED)).child("Vanta")),
            )
            .child(
                div()
                    .id("editor-scroll")
                    .flex_1()
                    .overflow_scroll()
                    .cursor(CursorStyle::IBeam)
                    .p_3()
                    .font_family("monospace")
                    .text_size(px(14.0))
                    .line_height(px(LINE_HEIGHT))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .child(EditorElement {
                        editor: cx.entity(),
                    }),
            )
            .child(
                div()
                    .h(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(self.status.clone())
                    .child("UTF-8"),
            )
    }
}

struct EditorElement {
    editor: Entity<Editor>,
}

struct PrepaintState {
    lines: Vec<LineLayout>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let lines = line_ranges(self.editor.read(cx).document.text()).len();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = line_y(lines).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let text = editor.document.text();
        let selection = editor.selection.clone();
        let cursor = editor.cursor();
        let marked = editor.marked_range.clone();
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let mut lines = Vec::new();
        let mut selections = Vec::new();
        let mut cursor_quad = None;

        for (line_number, range) in line_ranges(text).into_iter().enumerate() {
            let line_text = &text[range.clone()];
            let run = TextRun {
                len: line_text.len(),
                font: text_style.font(),
                color: text_style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = text_runs(run, marked.as_ref(), &range);
            let shaped = window.text_system().shape_line(
                line_text.to_owned().into(),
                font_size,
                &runs,
                None,
            );
            let top = bounds.top() + line_y(line_number);

            let selected_start = selection.start.max(range.start);
            let selected_end = selection.end.min(range.end);
            if selected_start < selected_end {
                selections.push(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + shaped.x_for_index(selected_start - range.start),
                            top,
                        ),
                        point(
                            bounds.left() + shaped.x_for_index(selected_end - range.start),
                            top + px(LINE_HEIGHT),
                        ),
                    ),
                    rgba(SELECTION),
                ));
            }

            if selection.is_empty() && cursor >= range.start && cursor <= range.end {
                cursor_quad = Some(fill(
                    Bounds::new(
                        point(
                            bounds.left() + shaped.x_for_index(cursor - range.start),
                            top,
                        ),
                        size(px(2.0), px(LINE_HEIGHT)),
                    ),
                    rgb(TEXT),
                ));
            }

            lines.push(LineLayout {
                start: range.start,
                end: range.end,
                shaped,
            });
        }

        PrepaintState {
            lines,
            selections,
            cursor: cursor_quad,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.editor.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        for selection in state.selections.drain(..) {
            window.paint_quad(selection);
        }
        for (line_number, line) in state.lines.iter().enumerate() {
            line.shaped
                .paint(
                    point(bounds.left(), bounds.top() + line_y(line_number)),
                    px(LINE_HEIGHT),
                    window,
                    cx,
                )
                .expect("text shaping must paint");
        }
        if focus.is_focused(window)
            && let Some(cursor) = state.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.editor.update(cx, |editor, _| {
            editor.layouts = std::mem::take(&mut state.lines);
            editor.bounds = Some(bounds);
        });
    }
}

fn text_runs(base: TextRun, marked: Option<&Range<usize>>, line: &Range<usize>) -> Vec<TextRun> {
    let Some(marked) = marked else {
        return vec![base];
    };
    let start = marked.start.max(line.start).min(line.end) - line.start;
    let end = marked.end.max(line.start).min(line.end) - line.start;
    if start >= end {
        return vec![base];
    }
    [
        TextRun {
            len: start,
            ..base.clone()
        },
        TextRun {
            len: end - start,
            underline: Some(UnderlineStyle {
                color: Some(base.color),
                thickness: px(1.0),
                wavy: false,
            }),
            ..base.clone()
        },
        TextRun {
            len: base.len - end,
            ..base
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, _) in text.match_indices('\n') {
        ranges.push(start..index);
        start = index + 1;
    }
    ranges.push(start..text.len());
    ranges
}

#[allow(clippy::cast_precision_loss)]
fn line_y(line: usize) -> Pixels {
    px(LINE_HEIGHT * line as f32)
}

fn utf16_to_utf8(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf8 += character.len_utf8();
        utf16 += character.len_utf16();
    }
    utf8
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("left", Left, Some("Editor")),
        KeyBinding::new("right", Right, Some("Editor")),
        KeyBinding::new("up", Up, Some("Editor")),
        KeyBinding::new("down", Down, Some("Editor")),
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new("shift-up", SelectUp, Some("Editor")),
        KeyBinding::new("shift-down", SelectDown, Some("Editor")),
        KeyBinding::new("home", Home, Some("Editor")),
        KeyBinding::new("end", End, Some("Editor")),
        KeyBinding::new("enter", Newline, Some("Editor")),
        KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
        KeyBinding::new("cmd-c", Copy, Some("Editor")),
        KeyBinding::new("cmd-x", Cut, Some("Editor")),
        KeyBinding::new("cmd-v", Paste, Some("Editor")),
        KeyBinding::new("cmd-s", Save, Some("Editor")),
        KeyBinding::new("ctrl-a", SelectAll, Some("Editor")),
        KeyBinding::new("ctrl-c", Copy, Some("Editor")),
        KeyBinding::new("ctrl-x", Cut, Some("Editor")),
        KeyBinding::new("ctrl-v", Paste, Some("Editor")),
        KeyBinding::new("ctrl-s", Save, Some("Editor")),
    ]);
}

fn main() {
    let document = match std::env::args_os().nth(1).map(PathBuf::from) {
        Some(path) => Document::open(path),
        None => Ok(Document::untitled()),
    }
    .unwrap_or_else(|error| {
        eprintln!("vanta: {error}");
        std::process::exit(1);
    });

    Application::new().run(move |cx: &mut App| {
        bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| Editor::new(document, cx)),
            )
            .expect("Vanta must open its main window");
        window
            .update(cx, |editor, window, _| window.focus(&editor.focus))
            .expect("Vanta must focus its editor");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_without_losing_the_trailing_empty_line() {
        assert_eq!(line_ranges("a\nb\n"), vec![0..1, 2..3, 4..4]);
        assert_eq!(line_ranges(""), vec![0..0]);
        assert_eq!(utf16_to_utf8("a🦀b", 3), 5);
    }
}
