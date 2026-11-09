//! The board view of a Kanban Markdown file (see `board.rs`): columns of
//! cards, moved by dragging or with the keyboard. Every change rewrites the
//! file right away (through the workspace's safe save), so other apps and
//! coding agents see it, and their edits show up here when the file reloads.

use super::*;
use crate::board::{self, Board};

actions!(
    board,
    [
        SelectUp, SelectDown, SelectLeft, SelectRight, MoveCardUp, MoveCardDown, MoveCardLeft, MoveCardRight,
        EditCard, ToggleCard, DeleteCard, NewCard, UndoBoard, CancelEdit
    ]
);

/// Boards bigger than this open as Markdown.
const MAX_BOARD_BYTES: usize = 1024 * 1024;
const UNDO_STEPS: usize = 100;
const LANE_WIDTH: f32 = 264.;

pub fn init(cx: &mut App) {
    let board = Some("Board");
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, board),
        KeyBinding::new("down", SelectDown, board),
        KeyBinding::new("left", SelectLeft, board),
        KeyBinding::new("right", SelectRight, board),
        KeyBinding::new("alt-up", MoveCardUp, board),
        KeyBinding::new("alt-down", MoveCardDown, board),
        KeyBinding::new("alt-left", MoveCardLeft, board),
        KeyBinding::new("alt-right", MoveCardRight, board),
        KeyBinding::new("enter", EditCard, board),
        KeyBinding::new("space", ToggleCard, board),
        KeyBinding::new("cmd-backspace", DeleteCard, board),
        KeyBinding::new("n", NewCard, board),
        KeyBinding::new("cmd-z", UndoBoard, board),
        KeyBinding::new("escape", CancelEdit, Some("BoardEditing")),
    ]);
}

/// The workspace saves the file when this arrives.
pub struct BoardEdited;

pub(super) struct BoardState {
    board: Board,
    pub(super) visible: bool,
    selected: Option<(usize, usize)>,
    editing: Option<CardEdit>,
    /// Earlier versions of the file, for ⌘Z.
    undo: Vec<String>,
    /// False for files Leaf can't write back exactly (odd encodings).
    editable: bool,
}

struct CardEdit {
    target: EditTarget,
    input: Entity<InputState>,
    _events: Subscription,
}

#[derive(Clone, Copy, PartialEq)]
enum EditTarget {
    Card(usize, usize),
    New(usize),
}

/// A card being dragged, and the picture under the pointer.
#[derive(Clone)]
struct DraggedCard {
    lane: usize,
    card: usize,
    title: SharedString,
}

impl Render for DraggedCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .w(px(LANE_WIDTH - 16.))
            .px_3()
            .py_2p5()
            .rounded(px(10.))
            .bg(theme.muted)
            .border_1()
            .border_color(theme.caret)
            .shadow_lg()
            .text_sm()
            .text_color(theme.foreground)
            .child(self.title.clone())
    }
}

impl BoardState {
    /// The board in `doc`, if it is one Leaf can show.
    pub(super) fn detect(doc: &Document) -> Option<Self> {
        if doc.len() > MAX_BOARD_BYTES || doc.as_slice().is_none() {
            return None;
        }
        let text = doc.text(0..doc.len());
        if !board::is_board(&text) {
            return None;
        }
        Some(Self {
            board: board::parse(&text)?,
            visible: true,
            selected: None,
            editing: None,
            undo: Vec::new(),
            editable: !doc.lossy,
        })
    }

    pub(super) fn summary(&self) -> String {
        let cards: usize = self.board.lanes.iter().map(|l| l.cards.len()).sum();
        format!("{} columns · {cards} cards", self.board.lanes.len())
    }

    fn card_count(&self, lane: usize) -> usize {
        self.board.lanes.get(lane).map_or(0, |l| l.cards.len())
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.and_then(|(lane, card)| {
            let lane = lane.min(self.board.lanes.len().checked_sub(1)?);
            let count = self.card_count(lane);
            (count > 0).then(|| (lane, card.min(count - 1)))
        });
    }
}

impl Viewer {
    fn board_text(&self) -> String {
        self.doc.text(0..self.doc.len()).into_owned()
    }

    /// Shows `new` as the file, remembers the old text for undo, and asks the
    /// workspace to save. `None` (an edit that didn't apply) does nothing.
    fn apply_board(&mut self, new: Option<String>, select: Option<(usize, usize)>, undoable: bool, cx: &mut Context<Self>) {
        let Some(new) = new else { return };
        let old = self.board_text();
        if new == old {
            return;
        }
        let Some(parsed) = board::parse(&new) else { return };
        self.swap_doc(Arc::new(Document::new(Bytes::Owned(new.into_bytes()), false)));
        let Some(state) = self.board.as_mut() else { return };
        if undoable {
            state.undo.push(old);
            if state.undo.len() > UNDO_STEPS {
                state.undo.remove(0);
            }
        }
        state.board = parsed;
        if select.is_some() {
            state.selected = select;
        }
        state.clamp_selection();
        cx.emit(BoardEdited);
        cx.notify();
    }

    fn board_editable(&self) -> bool {
        self.board.as_ref().is_some_and(|b| b.editable)
    }

    /// Moves a card; `before` counts cards as they were before the move.
    fn board_move(&mut self, from: usize, card: usize, to: usize, before: Option<usize>, cx: &mut Context<Self>) {
        if !self.board_editable() {
            return;
        }
        let text = self.board_text();
        let new = board::move_card(&text, from, card, to, before);
        let Some(state) = self.board.as_ref() else { return };
        let target_len = state.card_count(to) - usize::from(from == to);
        let index = match before {
            Some(b) if from == to && b > card => b - 1,
            Some(b) => b,
            None => target_len,
        };
        self.apply_board(new, Some((to, index.min(target_len))), true, cx);
    }

    fn select_card(&mut self, lane: usize, card: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.board.as_mut() {
            state.selected = Some((lane, card));
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn step_selection(&mut self, lanes: isize, cards: isize, cx: &mut Context<Self>) {
        let Some(state) = self.board.as_mut() else { return };
        let lane_count = state.board.lanes.len();
        if lane_count == 0 {
            return;
        }
        let (lane, card) = match state.selected {
            None => {
                let first = (0..lane_count).find(|&l| state.card_count(l) > 0).unwrap_or(0);
                state.selected = (state.card_count(first) > 0).then_some((first, 0));
                cx.notify();
                return;
            }
            Some(selected) => selected,
        };
        let mut new_lane = lane;
        if lanes != 0 {
            // Skip empty columns.
            let mut l = lane as isize + lanes;
            while l >= 0 && (l as usize) < lane_count {
                if state.card_count(l as usize) > 0 {
                    new_lane = l as usize;
                    break;
                }
                l += lanes;
            }
        }
        let count = state.card_count(new_lane);
        let new_card = if new_lane != lane { card.min(count - 1) } else { (card as isize + cards).clamp(0, count as isize - 1) as usize };
        state.selected = Some((new_lane, new_card));
        cx.notify();
    }

    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(0, -1, cx);
    }
    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(0, 1, cx);
    }
    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-1, 0, cx);
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(1, 0, cx);
    }

    fn move_selected(&mut self, lanes: isize, cards: isize, cx: &mut Context<Self>) {
        let Some((lane, card)) = self.board.as_ref().and_then(|b| b.selected) else { return };
        let lane_count = self.board.as_ref().map_or(0, |b| b.board.lanes.len());
        if lanes != 0 {
            let to = lane as isize + lanes;
            if to < 0 || to as usize >= lane_count {
                return;
            }
            let to = to as usize;
            let before = (card < self.board.as_ref().map_or(0, |b| b.card_count(to))).then_some(card);
            self.board_move(lane, card, to, before, cx);
        } else if cards < 0 && card > 0 {
            self.board_move(lane, card, lane, Some(card - 1), cx);
        } else if cards > 0 {
            let count = self.board.as_ref().map_or(0, |b| b.card_count(lane));
            if card + 1 < count {
                self.board_move(lane, card, lane, (card + 2 < count).then_some(card + 2), cx);
            }
        }
    }

    fn on_move_up(&mut self, _: &MoveCardUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(0, -1, cx);
    }
    fn on_move_down(&mut self, _: &MoveCardDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(0, 1, cx);
    }
    fn on_move_left(&mut self, _: &MoveCardLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(-1, 0, cx);
    }
    fn on_move_right(&mut self, _: &MoveCardRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(1, 0, cx);
    }

    fn toggle_card(&mut self, lane: usize, card: usize, cx: &mut Context<Self>) {
        if self.board_editable() {
            let new = board::toggle_done(&self.board_text(), lane, card);
            self.apply_board(new, Some((lane, card)), true, cx);
        }
    }

    fn on_toggle_card(&mut self, _: &ToggleCard, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((lane, card)) = self.board.as_ref().and_then(|b| b.selected) {
            self.toggle_card(lane, card, cx);
        }
    }

    fn on_delete_card(&mut self, _: &DeleteCard, _: &mut Window, cx: &mut Context<Self>) {
        let Some((lane, card)) = self.board.as_ref().and_then(|b| b.selected) else { return };
        if self.board_editable() {
            let new = board::delete_card(&self.board_text(), lane, card);
            self.apply_board(new, Some((lane, card)), true, cx);
        }
    }

    fn on_undo_board(&mut self, _: &UndoBoard, _: &mut Window, cx: &mut Context<Self>) {
        let previous = self.board.as_mut().and_then(|b| b.undo.pop());
        self.apply_board(previous, None, false, cx);
    }

    fn on_edit_card(&mut self, _: &EditCard, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((lane, card)) = self.board.as_ref().and_then(|b| b.selected) {
            self.start_card_edit(EditTarget::Card(lane, card), window, cx);
        }
    }

    fn on_new_card(&mut self, _: &NewCard, window: &mut Window, cx: &mut Context<Self>) {
        let lane = self.board.as_ref().and_then(|b| b.selected).map_or(0, |(lane, _)| lane);
        self.start_card_edit(EditTarget::New(lane), window, cx);
    }

    fn start_card_edit(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.board.as_ref().filter(|b| b.editable) else { return };
        let value = match target {
            EditTarget::Card(lane, card) => {
                let Some(card) = state.board.lanes.get(lane).and_then(|l| l.cards.get(card)) else { return };
                card.text.clone()
            }
            EditTarget::New(lane) if lane < state.board.lanes.len() => String::new(),
            EditTarget::New(_) => return,
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Card title  #tag  @{2026-12-31}").default_value(value));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_card_edit(true, window, cx),
            InputEvent::Blur => this.commit_card_edit(false, window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        if let Some(state) = self.board.as_mut() {
            state.editing = Some(CardEdit { target, input, _events: events });
        }
        cx.notify();
    }

    /// Saves the card being typed. Return on a new card starts the next one.
    fn commit_card_edit(&mut self, keep_adding: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.board.as_mut().and_then(|b| b.editing.take()) else { return };
        let value = edit.input.read(cx).value().to_string();
        let target = edit.target;
        drop(edit); // its blur must not commit a second time
        let text = self.board_text();
        match target {
            EditTarget::Card(lane, card) => {
                self.apply_board(board::set_card_text(&text, lane, card, &value), Some((lane, card)), true, cx);
            }
            EditTarget::New(lane) => {
                let index = self.board.as_ref().map_or(0, |b| b.card_count(lane));
                let added = board::add_card(&text, lane, &value);
                let was_added = added.is_some();
                self.apply_board(added, Some((lane, index)), true, cx);
                if was_added && keep_adding {
                    self.start_card_edit(EditTarget::New(lane), window, cx);
                    return;
                }
            }
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn on_cancel_edit(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.board.as_mut() {
            state.editing = None; // dropping the input skips its blur commit
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The parsed board was replaced on disk (or reloaded): re-read it.
    pub(super) fn reparse_board(&mut self) {
        let text = self.board_text();
        if let (Some(state), Some(parsed)) = (self.board.as_mut(), board::parse(&text)) {
            state.board = parsed;
            state.clamp_selection();
        }
    }

    pub(super) fn render_board(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.board.as_ref() else { return div().into_any_element() };
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        let (muted, card_bg, border, ring, fg) = (theme.muted_foreground, theme.muted, theme.border, theme.caret, theme.foreground);
        let drop_bg = theme.secondary;
        let (tag_bg, tag_fg) = if dark { (rgb(0x2e3a52), rgb(0xa9c1ee)) } else { (rgb(0xdde8fb), rgb(0x2b5aa8)) };
        let (date_bg, date_fg) = if dark { (rgb(0x4a3b1c), rgb(0xf0cf7a)) } else { (rgb(0xfbefd0), rgb(0x8a6212)) };
        let editing_target = state.editing.as_ref().map(|e| e.target);
        let editable = state.editable;

        let chip = move |text: String, bg: Rgba, fg: Rgba| div().px(px(7.)).rounded(px(9.)).bg(bg).text_color(fg).child(text);

        let lanes = state.board.lanes.iter().enumerate().map(|(l, lane)| {
            let cards = lane.cards.iter().enumerate().map(|(c, card)| {
                if editing_target == Some(EditTarget::Card(l, c)) {
                    let edit = state.editing.as_ref().expect("editing");
                    return div()
                        .key_context("BoardEditing")
                        .p_1()
                        .rounded(px(10.))
                        .bg(card_bg)
                        .border_2()
                        .border_color(ring)
                        .child(Input::new(&edit.input).small().appearance(false))
                        .into_any_element();
                }
                let parts = board::card_parts(card);
                let selected = state.selected == Some((l, c));
                let title: SharedString = if parts.title.is_empty() { card.text.clone().into() } else { parts.title.clone().into() };
                let done = card.done;
                let has_chips = !parts.tags.is_empty() || parts.date.is_some();
                div()
                    .id(("card", l * 100_000 + c))
                    .flex_none()
                    .px_3()
                    .py_2()
                    .rounded(px(10.))
                    .bg(card_bg)
                    .border_2()
                    .border_color(if selected { ring.into() } else { border.opacity(0.0) })
                    .when(editable, |el| {
                        el.on_drag(DraggedCard { lane: l, card: c, title: title.clone() }, |dragged, _, _, cx| {
                            cx.new(|_| dragged.clone())
                        })
                        .drag_over::<DraggedCard>(move |style, _, _, _| style.bg(drop_bg))
                        .on_drop(cx.listener(move |this, dragged: &DraggedCard, _, cx| {
                            this.board_move(dragged.lane, dragged.card, l, Some(c), cx);
                        }))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.select_card(l, c, window, cx);
                            if event.click_count == 2 {
                                this.start_card_edit(EditTarget::Card(l, c), window, cx);
                            }
                        }),
                    )
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .id(("check", l * 100_000 + c))
                                    .flex_none()
                                    .mt(px(3.))
                                    .size(px(14.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(if done { ring } else { muted })
                                    .when(done, |el| el.bg(ring))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_card(l, c, cx))),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .line_height(px(19.))
                                    .when(done, |el| el.line_through().text_color(muted))
                                    .child(title),
                            ),
                    )
                    .when(has_chips, |el| {
                        el.child(
                            h_flex()
                                .flex_wrap()
                                .gap_1p5()
                                .mt_1p5()
                                .pl(px(22.))
                                .text_xs()
                                .children(parts.tags.iter().map(|t| chip(format!("#{t}"), tag_bg, tag_fg)))
                                .when_some(parts.date.clone(), |el, date| el.child(chip(date, date_bg, date_fg))),
                        )
                    })
                    .when_some(parts.note.clone(), |el, note| {
                        el.child(div().mt_1().pl(px(22.)).text_xs().text_color(muted).truncate().child(note))
                    })
                    .when_some(parts.checklist, |el, (done, total)| {
                        el.child(div().mt_1().pl(px(22.)).text_xs().text_color(muted).child(format!("☑ {done} / {total}")))
                    })
                    .into_any_element()
            });
            let adding = editing_target == Some(EditTarget::New(l));
            let footer = if adding {
                let edit = state.editing.as_ref().expect("editing");
                div()
                    .key_context("BoardEditing")
                    .p_1()
                    .rounded(px(10.))
                    .bg(card_bg)
                    .border_2()
                    .border_color(ring)
                    .child(Input::new(&edit.input).small().appearance(false))
                    .into_any_element()
            } else {
                div()
                    .id(("add", l))
                    .px_2()
                    .py_1p5()
                    .rounded(px(8.))
                    .text_xs()
                    .text_color(muted)
                    .when(editable, |el| {
                        el.hover(move |style| style.bg(drop_bg))
                            .on_click(cx.listener(move |this, _, window, cx| this.start_card_edit(EditTarget::New(l), window, cx)))
                            .child("+ Add a card")
                    })
                    .into_any_element()
            };
            v_flex()
                .id(("lane", l))
                .w(px(LANE_WIDTH))
                .flex_none()
                .h_full()
                .gap_2()
                .p_1()
                .rounded(px(12.))
                .when(editable, |el| {
                    el.drag_over::<DraggedCard>(move |style, _, _, _| style.bg(drop_bg.opacity(0.5)))
                        .on_drop(cx.listener(move |this, dragged: &DraggedCard, _, cx| {
                            this.board_move(dragged.lane, dragged.card, l, None, cx);
                        }))
                })
                .child(
                    h_flex()
                        .gap_2()
                        .px_1()
                        .pt_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(lane.title.clone())
                        .child(div().font_weight(FontWeight::NORMAL).text_color(muted).child(lane.cards.len().to_string())),
                )
                .child(
                    v_flex()
                        .id(("lane-cards", l))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .gap_2()
                        .children(cards)
                        .child(footer),
                )
        });

        let hints = if editable {
            "↑↓←→ select   ⌥ + arrows move   ⏎ edit   Space done   N new card   ⌘⌫ delete   ⌘Z undo   ⇧⌘V source"
        } else {
            "View only: this file's encoding can't be written back exactly."
        };
        v_flex()
            .id("board")
            .key_context(if state.editing.is_some() { "BoardRoot" } else { "Board" })
            .track_focus(&self.focus)
            .size_full()
            .text_color(fg)
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_move_left))
            .on_action(cx.listener(Self::on_move_right))
            .on_action(cx.listener(Self::on_edit_card))
            .on_action(cx.listener(Self::on_toggle_card))
            .on_action(cx.listener(Self::on_delete_card))
            .on_action(cx.listener(Self::on_new_card))
            .on_action(cx.listener(Self::on_undo_board))
            .on_action(cx.listener(Self::on_cancel_edit))
            .child(
                h_flex()
                    .id("board-lanes")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .gap(px(14.))
                    .px(px(20.))
                    .pt_2()
                    .pb_3()
                    .children(lanes),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(20.))
                    .py_1p5()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(hints),
            )
            .into_any_element()
    }
}
