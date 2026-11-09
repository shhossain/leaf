//! Syntax highlighting for the viewer, done only for lines on screen.
//!
//! syntect is line-at-a-time and stateful, so the parser state is recorded
//! every `CHUNK` lines. Highlighting a visible chunk then starts from its
//! checkpoint and parses at most `CHUNK` lines. All parsing happens on one
//! [`Worker`] thread per open file, never on the UI thread.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::text_index::Document;

pub const CHUNK: usize = 128;
/// Bigger files are shown without highlighting.
pub const MAX_HIGHLIGHT_BYTES: usize = 16 * 1024 * 1024;
/// Only the start of a very long line is parsed.
const MAX_PARSE_LINE: usize = 4_000;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// `(byte range in the source line, theme highlight name)`.
pub type Spans = Vec<(Range<usize>, &'static str)>;

pub fn find_syntax(path: &Path, first_line: &str) -> Option<&'static SyntaxReference> {
    let set = &*SYNTAXES;
    let by_token = |s: &std::ffi::OsStr| set.find_syntax_by_extension(&s.to_string_lossy());
    path.extension()
        .and_then(by_token)
        .or_else(|| path.file_name().and_then(by_token)) // Makefile, Dockerfile
        .or_else(|| set.find_syntax_by_first_line(first_line)) // #!/bin/bash
        .filter(|s| s.name != "Plain Text")
}

#[derive(Clone)]
pub struct LineState {
    parse: ParseState,
    scopes: ScopeStack,
}

impl LineState {
    pub fn new(syntax: &SyntaxReference) -> Self {
        Self { parse: ParseState::new(syntax), scopes: ScopeStack::new() }
    }

    /// Advances over one line. With `out`, records its highlight spans.
    fn parse(&mut self, raw: &[u8], namer: &mut Namer, out: Option<&mut Spans>) {
        let raw = &raw[..raw.len().min(MAX_PARSE_LINE)];
        let mut line = String::from_utf8_lossy(raw).into_owned();
        line.push('\n');
        let Ok(ops) = self.parse.parse_line(&line, &SYNTAXES) else { return };
        let Some(out) = out else {
            for (_, op) in ops {
                let _ = self.scopes.apply(&op);
            }
            return;
        };
        let content_len = line.len() - 1;
        let mut cursor = 0;
        for (ix, op) in ops {
            let ix = ix.min(content_len);
            if ix > cursor {
                namer.push(out, cursor..ix, &self.scopes);
            }
            cursor = cursor.max(ix);
            let _ = self.scopes.apply(&op);
        }
        if content_len > cursor {
            namer.push(out, cursor..content_len, &self.scopes);
        }
    }
}

/// Parses `lines` from `state`, returning the state at the end. Used by the
/// background pass to record checkpoints.
pub fn advance(doc: &Document, mut state: LineState, lines: Range<usize>) -> LineState {
    let mut namer = Namer::default();
    for range in doc.line_ranges(lines) {
        state.parse(&doc.slice(range), &mut namer, None);
    }
    state
}

/// Highlights `lines` (one chunk) starting from that chunk's checkpoint.
pub fn highlight(doc: &Document, mut state: LineState, lines: Range<usize>) -> Vec<Spans> {
    let mut namer = Namer::default();
    doc.line_ranges(lines)
        .into_iter()
        .map(|range| {
            let mut spans = Spans::new();
            state.parse(&doc.slice(range), &mut namer, Some(&mut spans));
            spans
        })
        .collect()
}

/// Chunks the exact pass walks past its last checkpoint for one request.
/// Farther jumps (⌘↓ in a big file) keep approximate colors rather than
/// parsing everything in between.
const MAX_EXACT_GAP_CHUNKS: usize = 50;
/// Lines parsed before a chunk when highlighting it without exact state.
const WARMUP_LINES: usize = 64;

/// Colors for one chunk, sent from the highlighter thread.
pub struct Highlighted {
    pub chunk: usize,
    pub generation: u64,
    /// No better version of this chunk is coming.
    pub last: bool,
    pub spans: Vec<Spans>,
}

enum Message {
    Chunk(usize),
    Document { doc: Arc<Document>, generation: u64, keep_chunks: usize },
}

/// A viewer's own highlighting thread. It owns every bit of parser state
/// (syntect's Oniguruma state can't cross threads), so the UI thread only
/// sends chunk numbers and receives finished spans — it never parses.
/// Dropping the worker ends the thread.
pub struct Worker {
    tx: std::sync::mpsc::Sender<Message>,
}

impl Worker {
    pub fn spawn(
        doc: Arc<Document>,
        syntax: &'static SyntaxReference,
        out: futures::channel::mpsc::UnboundedSender<Highlighted>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("leaf-highlight".into())
            .spawn(move || run(rx, doc, syntax, out));
        Self { tx }
    }

    pub fn request(&self, chunk: usize) {
        let _ = self.tx.send(Message::Chunk(chunk));
    }

    /// The file grew: chunks before `keep_chunks` are still valid.
    pub fn set_document(&self, doc: Arc<Document>, generation: u64, keep_chunks: usize) {
        let _ = self.tx.send(Message::Document { doc, generation, keep_chunks });
    }
}

fn run(
    rx: std::sync::mpsc::Receiver<Message>,
    mut doc: Arc<Document>,
    syntax: &'static SyntaxReference,
    out: futures::channel::mpsc::UnboundedSender<Highlighted>,
) {
    let mut checkpoints = vec![LineState::new(syntax)];
    let mut generation = 0;
    while let Ok(first) = rx.recv() {
        let mut chunks: Vec<usize> = Vec::new();
        for message in std::iter::once(first).chain(rx.try_iter()) {
            match message {
                Message::Chunk(chunk) if !chunks.contains(&chunk) => chunks.push(chunk),
                Message::Chunk(_) => {}
                Message::Document { doc: new_doc, generation: new_generation, keep_chunks } => {
                    (doc, generation) = (new_doc, new_generation);
                    checkpoints.truncate(keep_chunks.max(1));
                    chunks.clear();
                }
            }
        }
        // Newest requests first: they're what's on screen now.
        for chunk in chunks.into_iter().rev() {
            let start = chunk * CHUNK;
            if start >= doc.line_count() {
                continue;
            }
            let send = |last: bool, spans: Vec<Spans>| out.unbounded_send(Highlighted { chunk, generation, last, spans });
            if chunk < checkpoints.len() {
                if send(true, highlight(&doc, checkpoints[chunk].clone(), start..start + CHUNK)).is_err() {
                    return;
                }
                continue;
            }
            // Best guess right away: parse from a fresh state a little earlier.
            let warm = start.saturating_sub(WARMUP_LINES);
            let mut guess = highlight(&doc, LineState::new(syntax), warm..start + CHUNK);
            guess.drain(..(start - warm).min(guess.len()));
            let reachable = chunk < checkpoints.len() + MAX_EXACT_GAP_CHUNKS;
            if send(!reachable, guess).is_err() {
                return;
            }
            if reachable {
                while checkpoints.len() <= chunk {
                    let k = checkpoints.len() - 1;
                    let next = advance(&doc, checkpoints[k].clone(), k * CHUNK..(k + 1) * CHUNK);
                    checkpoints.push(next);
                }
                if send(true, highlight(&doc, checkpoints[chunk].clone(), start..start + CHUNK)).is_err() {
                    return;
                }
            }
        }
    }
}

#[derive(Default)]
struct Namer {
    cache: HashMap<Scope, Option<&'static str>>,
}

impl Namer {
    fn push(&mut self, out: &mut Spans, range: Range<usize>, scopes: &ScopeStack) {
        // The innermost scope that names something wins.
        let name = scopes
            .scopes
            .iter()
            .rev()
            .find_map(|scope| *self.cache.entry(*scope).or_insert_with(|| semantic_name(*scope)));
        let Some(name) = name else { return };
        match out.last_mut() {
            Some((last, last_name)) if last.end == range.start && *last_name == name => {
                last.end = range.end
            }
            _ => out.push((range, name)),
        }
    }
}

/// Maps a TextMate scope onto the theme's highlight names.
/// Adapted from gpui-component's syntect backend (Apache-2.0).
fn semantic_name(scope: Scope) -> Option<&'static str> {
    let scope = scope.build_string();
    let is = |prefix: &str| {
        scope == prefix || scope.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'))
    };
    let name = if is("punctuation.definition.string")
        || is("punctuation.definition.comment")
        || is("invalid")
        || is("meta")
        || is("source")
        || is("text")
    {
        return None;
    } else if is("comment.block.documentation") || is("comment.line.documentation") {
        "comment.doc"
    } else if is("comment") {
        "comment"
    } else if is("constant.character.escape") {
        "string.escape"
    } else if is("string.regexp") {
        "string.regex"
    } else if is("string") {
        "string"
    } else if is("constant.numeric") {
        "number"
    } else if is("constant.language.boolean") {
        "boolean"
    } else if is("constant") || is("support.constant") || is("entity.name.constant") {
        "constant"
    } else if is("keyword.operator") {
        "operator"
    } else if is("keyword.control.import") || is("meta.preprocessor") {
        "preproc"
    } else if is("keyword") || is("storage.modifier") {
        "keyword"
    } else if is("storage.type") {
        let declarations = [
            "class", "enum", "function", "impl", "interface", "module", "namespace", "struct",
            "trait", "type", "let", "var", "const",
        ];
        if declarations.iter().any(|word| scope.contains(word)) { "keyword" } else { "type" }
    } else if is("entity.name.function")
        || is("support.function")
        || is("variable.function")
        || is("meta.function-call")
    {
        "function"
    } else if is("entity.name.type")
        || is("entity.name.class")
        || is("entity.name.struct")
        || is("entity.name.enum")
        || is("entity.name.trait")
        || is("entity.name.interface")
        || is("entity.other.inherited-class")
        || is("support.type")
        || is("support.class")
    {
        "type"
    } else if is("entity.name.tag") {
        "tag"
    } else if is("entity.other.attribute-name") {
        "attribute"
    } else if is("entity.name.label") {
        "label"
    } else if is("entity.name.section") || is("markup.heading") {
        "title"
    } else if is("variable.language") {
        "variable.special"
    } else if is("variable.other.member")
        || is("variable.other.property")
        || is("support.variable.property")
        || is("meta.mapping.key")
    {
        "property"
    } else if is("variable") {
        "variable"
    } else if is("markup.bold") {
        "emphasis.strong"
    } else if is("markup.italic") {
        "emphasis"
    } else if is("markup.underline.link") {
        "link_uri"
    } else if is("markup.raw") {
        "text.literal"
    } else if is("markup.list") {
        "punctuation.list_marker"
    } else if is("punctuation.section") || is("punctuation.definition.block") {
        "punctuation.bracket"
    } else if is("punctuation.separator") || is("punctuation.terminator") {
        "punctuation.delimiter"
    } else if is("punctuation") {
        "punctuation"
    } else {
        return None;
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_index::Bytes;

    #[test]
    fn highlights_from_any_checkpoint_consistently() {
        let text: String = (0..300).map(|i| format!("fn f{i}() {{ let x = \"s\"; }} // c\n")).collect();
        let doc = Document::new(Bytes::Owned(text.into_bytes()), true);
        let syntax = find_syntax(Path::new("a.rs"), "").expect("rust syntax");
        let start = LineState::new(syntax);
        let at_128 = advance(&doc, start.clone(), 0..CHUNK);
        let direct = highlight(&doc, at_128, CHUNK..CHUNK + 2);
        let from_top = highlight(&doc, start, 0..CHUNK + 2);
        assert_eq!(direct[0], from_top[CHUNK]);
        let names: Vec<_> = direct[0].iter().map(|s| s.1).collect();
        assert!(names.contains(&"keyword") && names.contains(&"string") && names.contains(&"comment"), "{names:?}");
    }

    #[test]
    fn detects_by_extension_name_and_shebang() {
        assert!(find_syntax(Path::new("x.py"), "").is_some());
        assert!(find_syntax(Path::new("Makefile"), "").is_some());
        assert!(find_syntax(Path::new("script"), "#!/bin/bash").is_some());
        assert!(find_syntax(Path::new("notes.unknownext"), "hello").is_none());
    }
}
