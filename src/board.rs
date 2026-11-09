//! Kanban boards stored as Markdown, in the Obsidian Kanban plugin's format,
//! so the same file works in Leaf, in Obsidian and for coding agents:
//!
//! ```text
//! ---
//!
//! kanban-plugin: board
//!
//! ---
//!
//! ## Todo
//!
//! - [ ] Card title #tag @{2026-11-20}
//!     more lines of the card, indented
//!
//! ## Done
//!
//! **Complete**
//! - [x] Finished card
//!
//! ***
//!
//! ## Archive
//! ...
//! %% kanban:settings
//! ...
//! %%
//! ```
//!
//! Edits rewrite only the lines of the cards they touch; everything else
//! (the archive, settings, text Leaf doesn't understand) stays as it was.

use std::ops::Range;

const FRONTMATTER_KEY: &str = "kanban-plugin:";
const COMPLETE_MARK: &str = "**Complete**";
const ARCHIVE_MARK: &str = "***";
const SETTINGS_MARK: &str = "%% kanban:settings";
/// Obsidian indents a card's extra lines with four spaces (or a tab).
const INDENT: &str = "    ";

/// What a new board file starts with.
pub const TEMPLATE: &str = "---\n\nkanban-plugin: board\n\n---\n\n## Todo\n\n\n\n## Doing\n\n\n\n## Done\n\n**Complete**\n\n\n\n%% kanban:settings\n```\n{\"kanban-plugin\":\"board\"}\n```\n%%\n";

#[derive(Clone, Debug, PartialEq)]
pub struct Board {
    pub lanes: Vec<Lane>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub title: String,
    /// Line of the `## ` heading.
    pub heading: usize,
    /// Lines up to (not including) the next lane or the archive.
    pub end: usize,
    /// Cards dropped here are ticked (`**Complete**` under the heading).
    pub complete: bool,
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub lines: Range<usize>,
    pub done: bool,
    /// The first line, after the checkbox.
    pub text: String,
    /// The other lines, unindented.
    pub body: Vec<String>,
}

/// What a card shows: its title without tags and dates, which are listed separately.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardParts {
    pub title: String,
    pub tags: Vec<String>,
    pub date: Option<String>,
    /// Checklist items in the body: (done, total).
    pub checklist: Option<(usize, usize)>,
    /// First body line that isn't a checklist item.
    pub note: Option<String>,
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

/// End of the front matter block (index of the closing `---`) if it marks a board.
fn board_frontmatter(lines: &[String]) -> Option<usize> {
    if lines.first().map(|l| l.trim()) != Some("---") {
        return None;
    }
    let close = lines.iter().skip(1).position(|l| l.trim() == "---")? + 1;
    lines[1..close].iter().any(|l| l.trim_start().starts_with(FRONTMATTER_KEY)).then_some(close)
}

pub fn is_board(text: &str) -> bool {
    // Only the head is needed; don't split a big file just to look.
    let head = &text[..text.len().min(4096)];
    let head = &head[..head.char_indices().last().map_or(0, |(i, c)| i + c.len_utf8())];
    board_frontmatter(&split_lines(head)).is_some()
}

/// `- [ ] text` → (check char, text).
fn card_line(line: &str) -> Option<(char, &str)> {
    let rest = line.strip_prefix("- [")?;
    let mut chars = rest.chars();
    let check = chars.next()?;
    let rest = chars.as_str().strip_prefix(']')?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    Some((check, rest.strip_prefix(' ').unwrap_or(rest)))
}

fn continuation(line: &str) -> Option<&str> {
    line.strip_prefix('\t').or_else(|| line.strip_prefix(INDENT)).or_else(|| (line.trim().is_empty() && !line.is_empty()).then_some(""))
}

fn parse_lines(lines: &[String]) -> Option<Board> {
    let start = board_frontmatter(lines)? + 1;
    let stop = lines[start..]
        .iter()
        .position(|l| l.trim() == ARCHIVE_MARK || l.starts_with(SETTINGS_MARK))
        .map_or(lines.len(), |p| start + p);
    let mut lanes: Vec<Lane> = Vec::new();
    let mut ix = start;
    while ix < stop {
        let line = &lines[ix];
        if let Some(title) = line.strip_prefix("## ") {
            if let Some(last) = lanes.last_mut() {
                last.end = ix;
            }
            lanes.push(Lane { title: title.trim().to_string(), heading: ix, end: stop, complete: false, cards: Vec::new() });
            ix += 1;
            continue;
        }
        let Some(lane) = lanes.last_mut() else {
            ix += 1;
            continue;
        };
        if line.trim() == COMPLETE_MARK {
            lane.complete = true;
            ix += 1;
            continue;
        }
        if let Some((check, text)) = card_line(line) {
            let first = ix;
            let mut body = Vec::new();
            ix += 1;
            while ix < stop {
                match continuation(&lines[ix]) {
                    Some(rest) => body.push(rest.to_string()),
                    None => break,
                }
                ix += 1;
            }
            // Indented blank lines at the end belong to nobody in particular; keep them out.
            while body.last().is_some_and(|l| l.trim().is_empty()) && ix > first + 1 && lines[ix - 1].trim().is_empty() {
                body.pop();
                ix -= 1;
            }
            lane.cards.push(Card { lines: first..ix, done: check != ' ', text: text.to_string(), body });
            continue;
        }
        ix += 1;
    }
    Some(Board { lanes })
}

pub fn parse(text: &str) -> Option<Board> {
    parse_lines(&split_lines(text))
}

/// Where a new last card of `lane` goes.
fn lane_insert_point(lines: &[String], lane: &Lane) -> usize {
    if let Some(card) = lane.cards.last() {
        return card.lines.end;
    }
    let mut at = lane.heading + 1;
    if at < lane.end && lines[at].trim().is_empty() {
        at += 1;
    }
    if at < lane.end && lines[at].trim() == COMPLETE_MARK {
        at += 1;
    }
    at
}

fn card_first_line(check: char, text: &str) -> String {
    format!("- [{check}] {}", text.trim())
}

/// Inserts `block` at `at`, keeping a blank line before a following heading.
fn insert_block(lines: &mut Vec<String>, at: usize, mut block: Vec<String>) {
    let next = lines.get(at).map(|l| l.trim().to_string());
    if next.is_some_and(|n| n.starts_with("## ") || n == ARCHIVE_MARK || n.starts_with("%%")) {
        block.push(String::new());
    }
    lines.splice(at..at, block);
}

fn edit(text: &str, f: impl FnOnce(&mut Vec<String>, &Board) -> Option<()>) -> Option<String> {
    let mut lines = split_lines(text);
    let board = parse_lines(&lines)?;
    f(&mut lines, &board)?;
    Some(lines.join("\n"))
}

/// Moves card `card` of lane `from` to lane `to`, before card `before` (as
/// numbered before the move), or to the end. Cards dropped in a Complete lane
/// are ticked; cards taken out of one are unticked.
pub fn move_card(text: &str, from: usize, card: usize, to: usize, before: Option<usize>) -> Option<String> {
    edit(text, |lines, board| {
        let source = board.lanes.get(from)?;
        let target = board.lanes.get(to)?;
        let moving = source.cards.get(card)?;
        let at = match before.and_then(|b| target.cards.get(b)) {
            Some(next) => next.lines.start,
            None => lane_insert_point(lines, target),
        };
        if from == to && (at == moving.lines.start || at == moving.lines.end) {
            return Some(()); // dropped where it already is
        }
        let mut block: Vec<String> = lines[moving.lines.clone()].to_vec();
        if from != to && (target.complete || source.complete) {
            let check = if target.complete { 'x' } else { ' ' };
            let (_, rest) = card_line(&block[0])?;
            block[0] = card_first_line(check, rest);
        }
        let len = moving.lines.len();
        lines.drain(moving.lines.clone());
        let at = if at > moving.lines.start { at - len } else { at };
        insert_block(lines, at, block);
        Some(())
    })
}

pub fn add_card(text: &str, lane: usize, title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    edit(text, |lines, board| {
        let lane = board.lanes.get(lane)?;
        let check = if lane.complete { 'x' } else { ' ' };
        let at = lane_insert_point(lines, lane);
        insert_block(lines, at, vec![card_first_line(check, title)]);
        Some(())
    })
}

/// Replaces a card's first line (its title, tags and date); the rest stays.
pub fn set_card_text(text: &str, lane: usize, card: usize, title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    edit(text, |lines, board| {
        let card = board.lanes.get(lane)?.cards.get(card)?;
        let (check, _) = card_line(&lines[card.lines.start])?;
        lines[card.lines.start] = card_first_line(check, title);
        Some(())
    })
}

pub fn toggle_done(text: &str, lane: usize, card: usize) -> Option<String> {
    edit(text, |lines, board| {
        let card = board.lanes.get(lane)?.cards.get(card)?;
        let (check, rest) = card_line(&lines[card.lines.start])?;
        lines[card.lines.start] = card_first_line(if check == ' ' { 'x' } else { ' ' }, rest);
        Some(())
    })
}

pub fn delete_card(text: &str, lane: usize, card: usize) -> Option<String> {
    edit(text, |lines, board| {
        let card = board.lanes.get(lane)?.cards.get(card)?;
        lines.drain(card.lines.clone());
        Some(())
    })
}

/// Where the columns end: the archive (`***`), the settings block, or the
/// end of the file.
fn lanes_end(lines: &[String], board: &Board) -> usize {
    match board.lanes.last() {
        Some(lane) => lane.end,
        None => board_frontmatter(lines).map_or(lines.len(), |f| f + 1),
    }
}

pub fn add_lane(text: &str, title: &str) -> Option<String> {
    let title = title.trim().replace('\n', " ");
    if title.is_empty() {
        return None;
    }
    edit(text, |lines, board| {
        let at = lanes_end(lines, board);
        let mut block = Vec::new();
        if at > 0 && !lines[at - 1].trim().is_empty() {
            block.push(String::new());
        }
        block.extend([format!("## {title}"), String::new(), String::new()]);
        lines.splice(at..at, block);
        Some(())
    })
}

pub fn rename_lane(text: &str, lane: usize, title: &str) -> Option<String> {
    let title = title.trim().replace('\n', " ");
    if title.is_empty() {
        return None;
    }
    edit(text, |lines, board| {
        lines[board.lanes.get(lane)?.heading] = format!("## {title}");
        Some(())
    })
}

/// Moves column `from` before column `before`, or to the end.
pub fn move_lane(text: &str, from: usize, before: Option<usize>) -> Option<String> {
    edit(text, |lines, board| {
        let lane = board.lanes.get(from)?;
        let at = match before.and_then(|b| board.lanes.get(b)) {
            Some(next) => next.heading,
            None => lanes_end(lines, board),
        };
        if at == lane.heading || at == lane.end {
            return Some(());
        }
        let mut block: Vec<String> = lines[lane.heading..lane.end].to_vec();
        if block.last().is_some_and(|l| !l.trim().is_empty()) {
            block.push(String::new()); // the last column may end without a blank line
        }
        let len = lane.end - lane.heading;
        lines.drain(lane.heading..lane.end);
        let at = if at > lane.heading { at - len } else { at };
        lines.splice(at..at, block);
        Some(())
    })
}

/// Removes an empty column.
pub fn delete_lane(text: &str, lane: usize) -> Option<String> {
    edit(text, |lines, board| {
        let lane = board.lanes.get(lane).filter(|l| l.cards.is_empty())?;
        lines.drain(lane.heading..lane.end);
        Some(())
    })
}

/// A due date (`2026-11-20`) before `today`.
pub fn is_overdue(date: &str, today: &str) -> bool {
    date.len() >= 10 && date.is_char_boundary(10) && date[..10].bytes().filter(|b| b.is_ascii_digit()).count() == 8 && &date[..10] < today
}

/// Splits a card into what the board shows.
pub fn card_parts(card: &Card) -> CardParts {
    let mut parts = CardParts::default();
    let mut title = Vec::new();
    let mut rest = card.text.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("@{") {
            if let Some(end) = after.find('}') {
                parts.date = Some(after[..end].to_string());
                rest = &after[end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix("@@{") {
            if let Some(end) = after.find('}') {
                rest = &after[end + 1..]; // a time; the date chip is enough
                continue;
            }
        }
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let at_word_start = title.last().is_none_or(|c: &char| c.is_whitespace());
        if at_word_start && word.len() > 1 && word.starts_with('#') && !word[1..].starts_with('#') {
            parts.tags.push(word[1..].to_string());
            rest = &rest[word_end..];
            continue;
        }
        if at_word_start && word.starts_with('^') && word.len() > 1 && word_end == rest.trim_end().len() {
            break; // an Obsidian block id at the end
        }
        let ch = rest.chars().next().expect("not empty");
        title.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    let title: String = title.into_iter().collect();
    parts.title = clean_markup(title.split_whitespace().collect::<Vec<_>>().join(" ").as_str());
    let (mut done, mut total) = (0, 0);
    for line in &card.body {
        match card_line(line.trim_start()) {
            Some((check, _)) => {
                total += 1;
                done += usize::from(check != ' ');
            }
            None if parts.note.is_none() && !line.trim().is_empty() => parts.note = Some(clean_markup(line.trim())),
            None => {}
        }
    }
    parts.checklist = (total > 0).then_some((done, total));
    parts
}

/// `[[note|alias]]` → `alias`, `**bold**` → `bold`, `` `code` `` → `code`.
fn clean_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let link = &after[..end];
                out.push_str(link.rsplit('|').next().unwrap_or(link));
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\n\nkanban-plugin: board\n\n---\n\n## Todo\n\n- [ ] Write parser #feature @{2026-11-20}\n    - [x] lines\n    - [ ] cards\n- [ ] Second\n\n\n## Doing\n\n\n\n## Done\n\n**Complete**\n- [x] Old one\n\n***\n\n## Archive\n\n- [x] Archived\n\n%% kanban:settings\n```\n{\"kanban-plugin\":\"board\"}\n```\n%%\n";

    fn titles(text: &str) -> Vec<Vec<String>> {
        parse(text).unwrap().lanes.iter().map(|l| l.cards.iter().map(|c| c.text.clone()).collect()).collect()
    }

    #[test]
    fn parses_obsidian_boards() {
        assert!(is_board(SAMPLE));
        assert!(!is_board("# Just notes\n- [ ] task\n"));
        let board = parse(SAMPLE).unwrap();
        assert_eq!(board.lanes.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["Todo", "Doing", "Done"]);
        assert!(board.lanes[2].complete && !board.lanes[0].complete);
        assert_eq!(board.lanes[0].cards[0].body, ["- [x] lines", "- [ ] cards"]);
        assert!(board.lanes[2].cards[0].done);
        let parts = card_parts(&board.lanes[0].cards[0]);
        assert_eq!(parts.title, "Write parser");
        assert_eq!(parts.tags, ["feature"]);
        assert_eq!(parts.date.as_deref(), Some("2026-11-20"));
        assert_eq!(parts.checklist, Some((1, 2)));
    }

    #[test]
    fn moves_cards_and_keeps_everything_else() {
        let moved = move_card(SAMPLE, 0, 0, 1, None).unwrap();
        assert_eq!(titles(&moved)[0], ["Second"]);
        assert_eq!(titles(&moved)[1], ["Write parser #feature @{2026-11-20}"]);
        assert_eq!(parse(&moved).unwrap().lanes[1].cards[0].body.len(), 2, "body moves with the card");
        assert!(moved.ends_with("%% kanban:settings\n```\n{\"kanban-plugin\":\"board\"}\n```\n%%\n"));
        assert!(moved.contains("## Archive\n\n- [x] Archived"));
        // Into the Complete lane: ticked. Back out: unticked.
        let done = move_card(&moved, 1, 0, 2, Some(0)).unwrap();
        assert!(done.contains("**Complete**\n- [x] Write parser"));
        let back = move_card(&done, 2, 0, 0, Some(0)).unwrap();
        assert!(back.contains("## Todo\n\n- [ ] Write parser"));
        // Reorder within a lane, both directions.
        let reordered = move_card(SAMPLE, 0, 1, 0, Some(0)).unwrap();
        assert_eq!(titles(&reordered)[0][0], "Second");
        assert_eq!(move_card(&reordered, 0, 0, 0, None).unwrap(), SAMPLE.to_string());
    }

    #[test]
    fn adds_edits_toggles_and_deletes() {
        let added = add_card(SAMPLE, 1, "New idea").unwrap();
        assert_eq!(titles(&added)[1], ["New idea"]);
        let done_lane = add_card(SAMPLE, 2, "Shipped").unwrap();
        assert!(parse(&done_lane).unwrap().lanes[2].cards[1].done);
        let renamed = set_card_text(SAMPLE, 0, 1, "Renamed").unwrap();
        assert_eq!(titles(&renamed)[0][1], "Renamed");
        let toggled = toggle_done(SAMPLE, 0, 1).unwrap();
        assert!(parse(&toggled).unwrap().lanes[0].cards[1].done);
        let deleted = delete_card(SAMPLE, 0, 0).unwrap();
        assert_eq!(titles(&deleted)[0], ["Second"]);
        assert!(add_card(SAMPLE, 9, "x").is_none() && add_card(SAMPLE, 0, "  ").is_none());
    }

    #[test]
    fn edits_columns() {
        let lanes = |t: &str| parse(t).unwrap().lanes.iter().map(|l| l.title.clone()).collect::<Vec<_>>();
        let added = add_lane(SAMPLE, "Review").unwrap();
        assert_eq!(lanes(&added), ["Todo", "Doing", "Done", "Review"]);
        assert!(added.contains("## Review\n\n\n***"), "before the archive");
        let renamed = rename_lane(SAMPLE, 1, "In Progress").unwrap();
        assert_eq!(lanes(&renamed), ["Todo", "In Progress", "Done"]);
        let moved = move_lane(SAMPLE, 2, Some(0)).unwrap();
        assert_eq!(lanes(&moved), ["Done", "Todo", "Doing"]);
        assert_eq!(titles(&moved)[0], ["Old one"]);
        let back = move_lane(&moved, 0, None).unwrap();
        assert_eq!(lanes(&back), ["Todo", "Doing", "Done"]);
        assert_eq!(lanes(&delete_lane(SAMPLE, 1).unwrap()), ["Todo", "Done"]);
        assert!(delete_lane(SAMPLE, 0).is_none(), "only empty columns");
        assert!(is_overdue("2026-11-01", "2026-11-10") && !is_overdue("2026-12-01", "2026-11-10") && !is_overdue("soon", "2026-11-10"));
    }

    #[test]
    fn template_is_a_board_and_crlf_files_work() {
        let board = parse(TEMPLATE).unwrap();
        assert_eq!(board.lanes.len(), 3);
        let first = add_card(TEMPLATE, 0, "First").unwrap();
        assert!(first.contains("## Todo\n\n- [ ] First\n"));
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(titles(&crlf), titles(SAMPLE));
    }
}
