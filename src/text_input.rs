//!
//! Text input.
//!
//! * Can do the usual insert/delete/movement operations.
//! * Text selection via keyboard and mouse.
//! * Scrolls with the cursor.
//! * Invalid flag.
//!
//! The visual cursor must be set separately after rendering.
//! It is accessible as [TextInputState::screen_cursor()] after rendering.
//!
//! Event handling by calling the freestanding fn [handle_events].
//! There's [handle_mouse_events] if you want to override the default key bindings but keep
//! the mouse behaviour.
//!
use crate::_private::NonExhaustive;
use crate::clipboard::{Clipboard, LocalClipboard};
use crate::core::{TextCore, TextString};
use crate::event::{ReadOnly, TextOutcome};
use crate::undo_buffer::{UndoBuffer, UndoEntry, UndoVec};
use crate::{ipos_type, upos_type, Cursor, Glyph, Grapheme, TextError, TextPosition, TextRange};
use rat_event::util::MouseFlags;
use rat_event::{ct_event, HandleEvent, MouseOnly, Regular};
use rat_focus::{FocusFlag, HasFocusFlag};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::{BlockExt, StatefulWidget, Style, Stylize, Widget};
use ratatui::widgets::{Block, StatefulWidgetRef};
use std::borrow::Cow;
use std::cmp::min;
use std::ops::Range;

/// Text input widget.
#[derive(Debug, Default, Clone)]
pub struct TextInput<'a> {
    block: Option<Block<'a>>,
    style: Style,
    focus_style: Option<Style>,
    select_style: Option<Style>,
    invalid_style: Option<Style>,
    text_style: Vec<Style>,
}

/// Combined style for the widget.
#[derive(Debug, Clone)]
pub struct TextInputStyle {
    pub style: Style,
    pub focus: Option<Style>,
    pub select: Option<Style>,
    pub invalid: Option<Style>,
    pub non_exhaustive: NonExhaustive,
}

/// State for TextInput.
#[derive(Debug, Clone)]
pub struct TextInputState {
    /// Current focus state.
    pub focus: FocusFlag,
    /// The whole area with block.
    pub area: Rect,
    /// Area inside a possible block.
    pub inner: Rect,

    /// Editing core
    pub value: TextCore<TextString>,

    /// Display as invalid.
    pub invalid: bool,
    /// Display offset
    pub offset: upos_type,

    /// Mouse selection in progress.
    pub mouse: MouseFlags,

    /// Construct with `..Default::default()`
    pub non_exhaustive: NonExhaustive,
}

impl Default for TextInputStyle {
    fn default() -> Self {
        Self {
            style: Default::default(),
            focus: Default::default(),
            select: Default::default(),
            invalid: Default::default(),
            non_exhaustive: NonExhaustive,
        }
    }
}

impl<'a> TextInput<'a> {
    /// New widget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the combined style.
    #[inline]
    pub fn styles(mut self, style: TextInputStyle) -> Self {
        self.style = style.style;
        self.focus_style = style.focus;
        self.select_style = style.select;
        self.invalid_style = style.invalid;
        self
    }

    /// Base text style.
    #[inline]
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// Style when focused.
    #[inline]
    pub fn focus_style(mut self, style: impl Into<Style>) -> Self {
        self.focus_style = Some(style.into());
        self
    }

    /// Style for selection
    #[inline]
    pub fn select_style(mut self, style: impl Into<Style>) -> Self {
        self.select_style = Some(style.into());
        self
    }

    /// Style for the invalid indicator.
    /// This is patched onto either base_style or focus_style
    #[inline]
    pub fn invalid_style(mut self, style: impl Into<Style>) -> Self {
        self.invalid_style = Some(style.into());
        self
    }

    /// List of text-styles.
    ///
    /// Use [TextAreaState::add_style()] to refer a text range to
    /// one of these styles.
    pub fn text_style<T: IntoIterator<Item = Style>>(mut self, styles: T) -> Self {
        self.text_style = styles.into_iter().collect();
        self
    }

    /// Block.
    #[inline]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl<'a> StatefulWidgetRef for TextInput<'a> {
    type State = TextInputState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        render_ref(self, area, buf, state);
    }
}

impl<'a> StatefulWidget for TextInput<'a> {
    type State = TextInputState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        render_ref(&self, area, buf, state);
    }
}

fn render_ref(widget: &TextInput<'_>, area: Rect, buf: &mut Buffer, state: &mut TextInputState) {
    state.area = area;
    state.inner = widget.block.inner_if_some(area);

    widget.block.render(area, buf);

    let inner = state.inner;

    if inner.width == 0 || inner.height == 0 {
        // noop
        return;
    }

    let focus_style = if let Some(focus_style) = widget.focus_style {
        focus_style
    } else {
        widget.style
    };
    let select_style = if let Some(select_style) = widget.select_style {
        select_style
    } else {
        Style::default().on_yellow()
    };
    let invalid_style = if let Some(invalid_style) = widget.invalid_style {
        invalid_style
    } else {
        Style::default().red()
    };

    let (style, select_style) = if state.focus.get() {
        if state.invalid {
            (
                focus_style.patch(invalid_style),
                select_style.patch(invalid_style),
            )
        } else {
            (focus_style, select_style)
        }
    } else {
        if state.invalid {
            (
                widget.style.patch(invalid_style),
                widget.style.patch(invalid_style),
            )
        } else {
            (widget.style, widget.style)
        }
    };

    // set base style
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.right() {
            let cell = buf.get_mut(x, y);
            cell.reset();
            cell.set_style(style);
        }
    }

    let ox = state.offset() as u16;
    // this is just a guess at the display-width
    let show_range = {
        let start = ox as upos_type;
        let end = min(start + inner.width as upos_type, state.len());
        state.bytes_at_range(start..end).expect("valid_range")
    };
    let selection = state.selection();
    let mut styles = Vec::new();

    let glyph_iter = state
        .value
        .glyphs(0..1, ox, inner.width)
        .expect("valid_offset");
    for g in glyph_iter {
        if g.screen_width() > 0 {
            let mut style = style;
            styles.clear();
            state
                .value
                .styles_at_page(show_range.clone(), g.text_bytes().start, &mut styles);
            for style_nr in &styles {
                if let Some(s) = widget.text_style.get(*style_nr) {
                    style = style.patch(*s);
                }
            }
            // selection
            if selection.contains(&g.pos().x) {
                style = style.patch(select_style);
            };

            // relative screen-pos of the glyph
            let screen_pos = g.screen_pos();

            // render glyph
            let cell = buf.get_mut(inner.x + screen_pos.0, inner.y + screen_pos.1);
            cell.set_symbol(g.glyph());
            cell.set_style(style);
            // clear the reset of the cells to avoid interferences.
            for d in 1..g.screen_width() {
                let cell = buf.get_mut(inner.x + screen_pos.0 + d, inner.y + screen_pos.1);
                cell.reset();
                cell.set_style(style);
            }
        }
    }
}

impl Default for TextInputState {
    fn default() -> Self {
        let mut value = TextCore::new(
            Some(Box::new(UndoVec::new(99))),
            Some(Box::new(LocalClipboard::new())),
        );
        value.set_glyph_line_break(false);

        Self {
            focus: Default::default(),
            invalid: false,
            area: Default::default(),
            inner: Default::default(),
            mouse: Default::default(),
            value,
            non_exhaustive: NonExhaustive,
            offset: 0,
        }
    }
}

impl HasFocusFlag for TextInputState {
    fn focus(&self) -> FocusFlag {
        self.focus.clone()
    }

    fn area(&self) -> Rect {
        self.area
    }
}

impl TextInputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn named(name: &str) -> Self {
        Self {
            focus: FocusFlag::named(name),
            ..TextInputState::default()
        }
    }

    /// Renders the widget in invalid style.
    #[inline]
    pub fn set_invalid(&mut self, invalid: bool) {
        self.invalid = invalid;
    }

    /// Renders the widget in invalid style.
    #[inline]
    pub fn get_invalid(&self) -> bool {
        self.invalid
    }
}

impl TextInputState {
    /// Clipboard
    pub fn set_clipboard(&mut self, clip: Option<impl Clipboard + 'static>) {
        match clip {
            None => self.value.set_clipboard(None),
            Some(v) => self.value.set_clipboard(Some(Box::new(v))),
        }
    }

    /// Clipboard
    pub fn clipboard(&self) -> Option<&dyn Clipboard> {
        self.value.clipboard()
    }

    /// Copy to internal buffer
    pub fn copy_to_clip(&mut self) -> bool {
        let Some(clip) = self.value.clipboard() else {
            return false;
        };

        _ = clip.set_string(self.selected_text().as_ref());

        true
    }

    /// Cut to internal buffer
    pub fn cut_to_clip(&mut self) -> bool {
        let Some(clip) = self.value.clipboard() else {
            return false;
        };

        match clip.set_string(self.selected_text().as_ref()) {
            Ok(_) => self
                .delete_range(self.selection())
                .expect("valid_selection"),
            Err(_) => true,
        }
    }

    /// Paste from internal buffer.
    pub fn paste_from_clip(&mut self) -> bool {
        let Some(clip) = self.value.clipboard() else {
            return false;
        };

        if let Ok(text) = clip.get_string() {
            self.insert_str(text)
        } else {
            true
        }
    }
}

impl TextInputState {
    /// Set undo buffer.
    pub fn set_undo_buffer(&mut self, undo: Option<impl UndoBuffer + 'static>) {
        match undo {
            None => self.value.set_undo_buffer(None),
            Some(v) => self.value.set_undo_buffer(Some(Box::new(v))),
        }
    }

    /// Undo
    #[inline]
    pub fn undo_buffer(&self) -> Option<&dyn UndoBuffer> {
        self.value.undo_buffer()
    }

    /// Undo
    #[inline]
    pub fn undo_buffer_mut(&mut self) -> Option<&mut dyn UndoBuffer> {
        self.value.undo_buffer_mut()
    }

    /// Get all recent replay recordings.
    pub fn recent_replay_log(&mut self) -> Vec<UndoEntry> {
        self.value.recent_replay_log()
    }

    /// Apply the replay recording.
    pub fn replay_log(&mut self, replay: &[UndoEntry]) {
        self.value.replay_log(replay)
    }

    /// Undo operation
    pub fn undo(&mut self) -> bool {
        self.value.undo()
    }

    /// Redo operation
    pub fn redo(&mut self) -> bool {
        self.value.redo()
    }
}

impl TextInputState {
    /// Set and replace all styles.
    #[inline]
    pub fn set_styles(&mut self, styles: Vec<(Range<usize>, usize)>) {
        self.value.set_styles(styles);
    }

    /// Add a style for a [TextRange]. The style-nr refers to one
    /// of the styles set with the widget.
    #[inline]
    pub fn add_style(&mut self, range: Range<usize>, style: usize) {
        self.value.add_style(range.into(), style);
    }

    /// Add a style for a Range<upos_type> to denote the cells.
    /// The style-nr refers to one of the styles set with the widget.
    #[inline]
    pub fn add_range_style(
        &mut self,
        range: Range<upos_type>,
        style: usize,
    ) -> Result<(), TextError> {
        let r = self
            .value
            .bytes_at_range(TextRange::new((range.start, 0), (range.end, 0)))?;
        self.value.add_style(r, style);
        Ok(())
    }

    /// Remove the exact TextRange and style.
    #[inline]
    pub fn remove_style(&mut self, range: Range<usize>, style: usize) {
        self.value.remove_style(range.into(), style);
    }

    /// Remove the exact Range<upos_type> and style.
    #[inline]
    pub fn remove_range_style(
        &mut self,
        range: Range<upos_type>,
        style: usize,
    ) -> Result<(), TextError> {
        let r = self
            .value
            .bytes_at_range(TextRange::new((range.start, 0), (range.end, 0)))?;
        self.value.remove_style(r, style);
        Ok(())
    }

    /// All styles active at the given position.
    #[inline]
    pub fn styles_at(&self, byte_pos: usize, buf: &mut Vec<usize>) {
        self.value.styles_at(byte_pos, buf)
    }

    /// Check if the given style applies at the position and
    /// return the complete range for the style.
    #[inline]
    pub fn style_match(&self, byte_pos: usize, style: usize) -> Option<Range<usize>> {
        self.value.style_match(byte_pos, style.into())
    }

    /// List of all styles.
    #[inline]
    pub fn styles(&self) -> Option<impl Iterator<Item = (Range<usize>, usize)> + '_> {
        self.value.styles()
    }
}

impl TextInputState {
    /// Offset shown.
    #[inline]
    pub fn offset(&self) -> upos_type {
        self.offset
    }

    /// Offset shown. This is corrected if the cursor wouldn't be visible.
    #[inline]
    pub fn set_offset(&mut self, offset: upos_type) {
        self.offset = offset;
    }

    /// Cursor position.
    #[inline]
    pub fn cursor(&self) -> upos_type {
        self.value.cursor().x
    }

    /// Selection anchor.
    #[inline]
    pub fn anchor(&self) -> upos_type {
        self.value.anchor().x
    }

    /// Set the cursor position, reset selection.
    #[inline]
    pub fn set_cursor(&mut self, cursor: upos_type, extend_selection: bool) -> bool {
        self.value
            .set_cursor(TextPosition::new(cursor, 0), extend_selection)
    }

    /// Selection.
    #[inline]
    pub fn has_selection(&self) -> bool {
        self.value.has_selection()
    }

    /// Selection.
    #[inline]
    pub fn selection(&self) -> Range<upos_type> {
        let v = self.value.selection();
        v.start.x..v.end.x
    }

    /// Selection.
    #[inline]
    pub fn set_selection(&mut self, anchor: upos_type, cursor: upos_type) -> bool {
        self.value
            .set_selection(TextPosition::new(anchor, 0), TextPosition::new(cursor, 0))
    }

    /// Selection.
    #[inline]
    pub fn select_all(&mut self) -> bool {
        self.value.select_all()
    }

    /// Selection.
    #[inline]
    pub fn selected_text(&self) -> &str {
        match self
            .value
            .str_slice(self.value.selection())
            .expect("valid_range")
        {
            Cow::Borrowed(v) => v,
            Cow::Owned(_) => {
                unreachable!()
            }
        }
    }
}

impl TextInputState {
    /// Empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Text value.
    #[inline]
    pub fn text(&self) -> &str {
        self.value.text().as_str()
    }

    /// Text slice as Cow<str>
    #[inline]
    pub fn str_slice(&self, range: Range<upos_type>) -> Result<Cow<'_, str>, TextError> {
        self.value
            .str_slice(TextRange::new((range.start, 0), (range.end, 0)))
    }

    /// Length as grapheme count.
    #[inline]
    pub fn len(&self) -> upos_type {
        self.value.line_width(0).expect("valid_row")
    }

    /// Length as grapheme count.
    #[inline]
    pub fn line_width(&self) -> upos_type {
        self.value.line_width(0).expect("valid_row")
    }

    /// Iterator for the glyphs of the lines in range.
    /// Glyphs here a grapheme + display length.
    #[inline]
    pub fn glyphs(
        &self,
        screen_offset: u16,
        screen_width: u16,
    ) -> Result<impl Iterator<Item = Glyph<'_>>, TextError> {
        self.value.glyphs(0..1, screen_offset, screen_width)
    }

    /// Get a cursor over all the text with the current position set at pos.
    #[inline]
    pub fn text_graphemes(
        &self,
        pos: upos_type,
    ) -> Result<impl Iterator<Item = Grapheme<'_>> + Cursor, TextError> {
        self.value.text_graphemes(TextPosition::new(pos, 0))
    }

    /// Get a cursor over the text-range the current position set at pos.
    #[inline]
    pub fn graphemes(
        &self,
        range: Range<upos_type>,
        pos: upos_type,
    ) -> Result<impl Iterator<Item = Grapheme<'_>> + Cursor, TextError> {
        self.value.graphemes(
            TextRange::new((range.start, 0), (range.end, 0)),
            TextPosition::new(pos, 0),
        )
    }

    /// Grapheme position to byte position.
    /// This is the (start,end) position of the single grapheme after pos.
    #[inline]
    pub fn byte_at(&self, pos: upos_type) -> Result<Range<usize>, TextError> {
        self.value.byte_at(TextPosition::new(pos, 0))
    }

    /// Grapheme range to byte range.
    #[inline]
    pub fn bytes_at_range(&self, range: Range<upos_type>) -> Result<Range<usize>, TextError> {
        self.value
            .bytes_at_range(TextRange::new((range.start, 0), (range.end, 0)))
    }

    /// Byte position to grapheme position.
    /// Returns the position that contains the given byte index.
    #[inline]
    pub fn byte_pos(&self, byte: usize) -> Result<upos_type, TextError> {
        self.value.byte_pos(byte).map(|v| v.x)
    }

    /// Byte range to grapheme range.
    #[inline]
    pub fn byte_range(&self, bytes: Range<usize>) -> Result<Range<upos_type>, TextError> {
        self.value.byte_range(bytes).map(|v| v.start.x..v.end.x)
    }
}

impl TextInputState {
    /// Reset to empty.
    #[inline]
    pub fn clear(&mut self) -> bool {
        if self.is_empty() {
            false
        } else {
            self.offset = 0;
            self.value.clear();
            true
        }
    }

    /// Set text.
    ///
    /// Returns an error if the text contains line-breaks.
    #[inline]
    pub fn set_text<S: Into<String>>(&mut self, s: S) {
        self.offset = 0;
        self.value.set_text(TextString::new_string(s.into()));
    }

    /// Insert a char at the current position.
    #[inline]
    pub fn insert_char(&mut self, c: char) -> bool {
        if self.value.has_selection() {
            self.value
                .remove_str_range(self.value.selection())
                .expect("valid_selection");
        }
        if c == '\n' {
            return false;
        } else if c == '\t' {
            self.value
                .insert_tab(self.value.cursor())
                .expect("valid_cursor");
        } else {
            self.value
                .insert_char(self.value.cursor(), c)
                .expect("valid_cursor");
        }
        self.scroll_cursor_to_visible();
        true
    }

    /// Insert a tab character at the cursor position.
    /// Removes the selection and inserts the tab.
    pub fn insert_tab(&mut self) -> bool {
        if self.value.has_selection() {
            self.value
                .remove_str_range(self.value.selection())
                .expect("valid_selection");
        }
        self.value
            .insert_tab(self.value.cursor())
            .expect("valid_cursor");
        self.scroll_cursor_to_visible();
        true
    }

    /// Insert a str at the current position.
    #[inline]
    pub fn insert_str(&mut self, t: impl AsRef<str>) -> bool {
        let t = t.as_ref();
        if self.value.has_selection() {
            self.value
                .remove_str_range(self.value.selection())
                .expect("valid_selection");
        }
        self.value
            .insert_str(self.value.cursor(), t)
            .expect("valid_cursor");
        self.scroll_cursor_to_visible();
        true
    }

    /// Deletes the given range.
    #[inline]
    pub fn delete_range(&mut self, range: Range<upos_type>) -> Result<bool, TextError> {
        if !range.is_empty() {
            self.value
                .remove_str_range(TextRange::new((range.start, 0), (range.end, 0)))?;
            self.scroll_cursor_to_visible();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl TextInputState {
    /// Delete the char after the cursor.
    #[inline]
    pub fn delete_next_char(&mut self) -> bool {
        if self.has_selection() {
            self.delete_range(self.selection())
                .expect("valid_selection")
        } else {
            let r = self
                .value
                .remove_next_char(self.value.cursor())
                .expect("valid_cursor");
            let s = self.scroll_cursor_to_visible();

            r || s
        }
    }

    /// Delete the char before the cursor.
    #[inline]
    pub fn delete_prev_char(&mut self) -> bool {
        if self.value.has_selection() {
            self.delete_range(self.selection())
                .expect("valid_selection")
        } else {
            let r = self
                .value
                .remove_prev_char(self.value.cursor())
                .expect("valid_cursor");
            let s = self.scroll_cursor_to_visible();

            r || s
        }
    }

    /// Find the start of the next word. Word is everything that is not whitespace.
    pub fn next_word_start(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value
            .next_word_start(TextPosition::new(pos, 0))
            .map(|v| v.x)
    }

    /// Find the end of the next word.  Skips whitespace first, then goes on
    /// until it finds the next whitespace.
    pub fn next_word_end(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value
            .next_word_end(TextPosition::new(pos, 0))
            .map(|v| v.x)
    }

    /// Find prev word. Skips whitespace first.
    /// Attention: start/end are mirrored here compared to next_word_start/next_word_end,
    /// both return start<=end!
    pub fn prev_word_start(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value
            .prev_word_start(TextPosition::new(pos, 0))
            .map(|v| v.x)
    }

    /// Find the end of the previous word. Word is everything that is not whitespace.
    /// Attention: start/end are mirrored here compared to next_word_start/next_word_end,
    /// both return start<=end!
    pub fn prev_word_end(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value
            .prev_word_end(TextPosition::new(pos, 0))
            .map(|v| v.x)
    }

    /// Is the position at a word boundary?
    pub fn is_word_boundary(&self, pos: upos_type) -> Result<bool, TextError> {
        self.value.is_word_boundary(TextPosition::new(pos, 0))
    }

    /// Find the start of the word at pos.
    pub fn word_start(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value
            .word_start(TextPosition::new(pos, 0))
            .map(|v| v.x)
    }

    /// Find the end of the word at pos.
    pub fn word_end(&self, pos: upos_type) -> Result<upos_type, TextError> {
        self.value.word_end(TextPosition::new(pos, 0)).map(|v| v.x)
    }

    /// Deletes the next word.
    #[inline]
    pub fn delete_next_word(&mut self) -> bool {
        if self.has_selection() {
            self.delete_range(self.selection())
                .expect("valid_selection")
        } else {
            let cursor = self.cursor();

            let start = self.next_word_start(cursor).expect("valid_cursor");
            if start != cursor {
                self.delete_range(cursor..start).expect("valid_range")
            } else {
                let end = self.next_word_end(cursor).expect("valid_cursor");
                self.delete_range(cursor..end).expect("valid_range")
            }
        }
    }

    /// Deletes the given range.
    #[inline]
    pub fn delete_prev_word(&mut self) -> bool {
        if self.has_selection() {
            self.delete_range(self.selection())
                .expect("valid_selection")
        } else {
            let cursor = self.cursor();

            let end = self.prev_word_end(cursor).expect("valid_cursor");
            if end != cursor {
                self.delete_range(end..cursor).expect("valid_cursor")
            } else {
                let start = self.prev_word_start(cursor).expect("valid_cursor");
                self.delete_range(start..cursor).expect("valid_cursor")
            }
        }
    }

    /// Move to the next char.
    #[inline]
    pub fn move_right(&mut self, extend_selection: bool) -> bool {
        let c = min(self.cursor() + 1, self.len());
        let c = self.set_cursor(c, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    /// Move to the previous char.
    #[inline]
    pub fn move_left(&mut self, extend_selection: bool) -> bool {
        let c = self.cursor().saturating_sub(1);
        let c = self.set_cursor(c, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    /// Start of line
    #[inline]
    pub fn move_to_line_start(&mut self, extend_selection: bool) -> bool {
        let c = self.set_cursor(0, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    /// End of line
    #[inline]
    pub fn move_to_line_end(&mut self, extend_selection: bool) -> bool {
        let c = self.len();
        let c = self.set_cursor(c, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    #[inline]
    pub fn move_to_next_word(&mut self, extend_selection: bool) -> bool {
        let cursor = self.cursor();
        let end = self.next_word_end(cursor).expect("valid_cursor");
        let c = self.set_cursor(end, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    #[inline]
    pub fn move_to_prev_word(&mut self, extend_selection: bool) -> bool {
        let cursor = self.cursor();
        let start = self.prev_word_start(cursor).expect("valid_cursor");
        let c = self.set_cursor(start, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }
}

impl TextInputState {
    /// Converts from a widget relative screen coordinate to a grapheme index.
    /// x is the relative screen position.
    pub fn screen_to_col(&self, scx: i16) -> upos_type {
        let ox = self.offset();

        if scx < 0 {
            ox.saturating_sub((scx as ipos_type).abs() as upos_type)
        } else if scx as u16 >= self.inner.width {
            min(ox + scx as upos_type, self.len())
        } else {
            let scx = scx as u16;

            let line = self.glyphs(ox as u16, self.inner.width).expect("valid_row");

            let mut col = ox;
            for g in line {
                col = g.pos().x;
                if scx < g.screen_pos().0 + g.screen_width() {
                    break;
                }
            }
            col
        }
    }

    /// Converts a grapheme based position to a screen position
    /// relative to the widget area.
    pub fn col_to_screen(&self, pos: upos_type) -> Result<u16, TextError> {
        let ox = self.offset();

        if pos < ox {
            return Ok(0);
        }

        let line = self.glyphs(ox as u16, self.inner.width)?;
        let mut screen_x = 0;
        for g in line {
            if g.pos().x == pos {
                break;
            }
            screen_x = g.screen_pos().0 + g.screen_width();
        }
        Ok(screen_x)
    }

    /// Set the cursor position from a screen position relative to the origin
    /// of the widget. This value can be negative, which selects a currently
    /// not visible position and scrolls to it.
    #[inline]
    pub fn set_screen_cursor(&mut self, cursor: i16, extend_selection: bool) -> bool {
        let scx = cursor;

        let cx = self.screen_to_col(scx);

        let c = self.set_cursor(cx, extend_selection);
        let s = self.scroll_cursor_to_visible();
        c || s
    }

    /// The current text cursor as an absolute screen position.
    #[inline]
    pub fn screen_cursor(&self) -> Option<(u16, u16)> {
        if self.is_focused() {
            let cx = self.cursor();
            let ox = self.offset();

            if cx < ox {
                None
            } else if cx > ox + self.inner.width as upos_type {
                None
            } else {
                let sc = self.col_to_screen(cx).expect("valid_cursor");
                Some((self.inner.x + sc, self.inner.y))
            }
        } else {
            None
        }
    }

    /// Scrolling
    pub fn scroll_left(&mut self, delta: upos_type) -> bool {
        self.set_offset(self.offset.saturating_sub(delta));
        true
    }

    /// Scrolling
    pub fn scroll_right(&mut self, delta: upos_type) -> bool {
        self.set_offset(self.offset + delta);
        true
    }

    /// Change the offset in a way that the cursor is visible.
    pub fn scroll_cursor_to_visible(&mut self) -> bool {
        let old_offset = self.offset();

        let c = self.cursor();
        let o = self.offset();

        let no = if c < o {
            c
        } else if c >= o + self.inner.width as upos_type {
            c.saturating_sub(self.inner.width as upos_type)
        } else {
            o
        };

        self.set_offset(no);

        self.offset() != old_offset
    }
}

impl HandleEvent<crossterm::event::Event, Regular, TextOutcome> for TextInputState {
    fn handle(&mut self, event: &crossterm::event::Event, _keymap: Regular) -> TextOutcome {
        // small helper ...
        fn tc(r: bool) -> TextOutcome {
            if r {
                TextOutcome::TextChanged
            } else {
                TextOutcome::Unchanged
            }
        }

        let mut r = if self.is_focused() {
            match event {
                ct_event!(key press c)
                | ct_event!(key press SHIFT-c)
                | ct_event!(key press CONTROL_ALT-c) => tc(self.insert_char(*c)),
                ct_event!(keycode press Tab) => {
                    // ignore tab from focus
                    tc(if !self.focus.gained() {
                        self.insert_tab()
                    } else {
                        false
                    })
                }
                ct_event!(keycode press Backspace) => tc(self.delete_prev_char()),
                ct_event!(keycode press Delete) => tc(self.delete_next_char()),
                ct_event!(keycode press CONTROL-Backspace)
                | ct_event!(keycode press ALT-Backspace) => tc(self.delete_prev_word()),
                ct_event!(keycode press CONTROL-Delete) => tc(self.delete_next_word()),
                ct_event!(key press CONTROL-'c') => tc(self.copy_to_clip()),
                ct_event!(key press CONTROL-'x') => tc(self.cut_to_clip()),
                ct_event!(key press CONTROL-'v') => tc(self.paste_from_clip()),
                ct_event!(key press CONTROL-'d') => tc(self.clear()),
                ct_event!(key press CONTROL-'z') => tc(self.value.undo()),
                ct_event!(key press CONTROL_SHIFT-'Z') => tc(self.value.redo()),

                ct_event!(key release _)
                | ct_event!(key release SHIFT-_)
                | ct_event!(key release CONTROL_ALT-_)
                | ct_event!(keycode release Tab)
                | ct_event!(keycode release Backspace)
                | ct_event!(keycode release Delete)
                | ct_event!(keycode release CONTROL-Backspace)
                | ct_event!(keycode release ALT-Backspace)
                | ct_event!(keycode release CONTROL-Delete)
                | ct_event!(key release CONTROL-'c')
                | ct_event!(key release CONTROL-'x')
                | ct_event!(key release CONTROL-'v')
                | ct_event!(key release CONTROL-'d')
                | ct_event!(key release CONTROL-'y')
                | ct_event!(key release CONTROL-'z')
                | ct_event!(key release CONTROL_SHIFT-'Z') => TextOutcome::Unchanged,

                _ => TextOutcome::Continue,
            }
        } else {
            TextOutcome::Continue
        };
        if r == TextOutcome::Continue {
            r = self.handle(event, ReadOnly);
        }
        r
    }
}

impl HandleEvent<crossterm::event::Event, ReadOnly, TextOutcome> for TextInputState {
    fn handle(&mut self, event: &crossterm::event::Event, _keymap: ReadOnly) -> TextOutcome {
        let mut r = if self.is_focused() {
            match event {
                ct_event!(keycode press Left) => self.move_left(false).into(),
                ct_event!(keycode press Right) => self.move_right(false).into(),
                ct_event!(keycode press CONTROL-Left) => self.move_to_prev_word(false).into(),
                ct_event!(keycode press CONTROL-Right) => self.move_to_next_word(false).into(),
                ct_event!(keycode press Home) => self.move_to_line_start(false).into(),
                ct_event!(keycode press End) => self.move_to_line_end(false).into(),
                ct_event!(keycode press SHIFT-Left) => self.move_left(true).into(),
                ct_event!(keycode press SHIFT-Right) => self.move_right(true).into(),
                ct_event!(keycode press CONTROL_SHIFT-Left) => self.move_to_prev_word(true).into(),
                ct_event!(keycode press CONTROL_SHIFT-Right) => self.move_to_next_word(true).into(),
                ct_event!(keycode press SHIFT-Home) => self.move_to_line_start(true).into(),
                ct_event!(keycode press SHIFT-End) => self.move_to_line_end(true).into(),
                ct_event!(keycode press ALT-Left) => self.scroll_left(1).into(),
                ct_event!(keycode press ALT-Right) => self.scroll_right(1).into(),
                ct_event!(key press CONTROL-'a') => self.select_all().into(),

                ct_event!(keycode release Left)
                | ct_event!(keycode release Right)
                | ct_event!(keycode release CONTROL-Left)
                | ct_event!(keycode release CONTROL-Right)
                | ct_event!(keycode release Home)
                | ct_event!(keycode release End)
                | ct_event!(keycode release SHIFT-Left)
                | ct_event!(keycode release SHIFT-Right)
                | ct_event!(keycode release CONTROL_SHIFT-Left)
                | ct_event!(keycode release CONTROL_SHIFT-Right)
                | ct_event!(keycode release SHIFT-Home)
                | ct_event!(keycode release SHIFT-End)
                | ct_event!(key release CONTROL-'a') => TextOutcome::Unchanged,

                _ => TextOutcome::Continue,
            }
        } else {
            TextOutcome::Continue
        };

        if r == TextOutcome::Continue {
            r = self.handle(event, MouseOnly);
        }
        r
    }
}

impl HandleEvent<crossterm::event::Event, MouseOnly, TextOutcome> for TextInputState {
    fn handle(&mut self, event: &crossterm::event::Event, _keymap: MouseOnly) -> TextOutcome {
        match event {
            ct_event!(mouse any for m) if self.mouse.drag(self.area, m) => {
                let c = (m.column as i16) - (self.inner.x as i16);
                self.set_screen_cursor(c, true).into()
            }
            ct_event!(mouse down Left for column,row) => {
                if self.gained_focus() {
                    // don't react to the first click that's for
                    // focus. this one shouldn't demolish the selection.
                    TextOutcome::Unchanged
                } else if self.inner.contains((*column, *row).into()) {
                    let c = (column - self.inner.x) as i16;
                    self.set_screen_cursor(c, false).into()
                } else {
                    TextOutcome::Continue
                }
            }
            _ => TextOutcome::Continue,
        }
    }
}

/// Handle all events.
/// Text events are only processed if focus is true.
/// Mouse events are processed if they are in range.
pub fn handle_events(
    state: &mut TextInputState,
    focus: bool,
    event: &crossterm::event::Event,
) -> TextOutcome {
    state.focus.set(focus);
    state.handle(event, Regular)
}

/// Handle only navigation events.
/// Text events are only processed if focus is true.
/// Mouse events are processed if they are in range.
pub fn handle_readonly_events(
    state: &mut TextInputState,
    focus: bool,
    event: &crossterm::event::Event,
) -> TextOutcome {
    state.focus.set(focus);
    state.handle(event, ReadOnly)
}

/// Handle only mouse-events.
pub fn handle_mouse_events(
    state: &mut TextInputState,
    event: &crossterm::event::Event,
) -> TextOutcome {
    state.handle(event, MouseOnly)
}