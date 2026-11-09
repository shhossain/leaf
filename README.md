# Leaf

A small macOS text viewer and editor. Files open read-only, and editing is one keystroke away. Open files sit in a Notes-style sidebar and are only loaded when you look at them, so keeping hundreds open costs little.

![Leaf showing a note](docs/screenshots/notes.png)

Leaf exists because CotEditor slows down with many files open, and TextEdit is not a good viewer.

## Features

- **Viewer first.** Files open read-only. Press ⌘E or double-click to edit, and ⌘E again (or Done) to go back.
- **Lazy files.** A file is read when you open it, and only the 12 most recently viewed stay in memory. The sidebar reads just the first 2 KB of each file for its date and preview line.
- **Large files.** Files over 64 MB stay on disk and are read in 256 KB blocks as you scroll, while the line index is built in the background. A 200 MB log opens in about 0.2 s.
- **Log tailing.** When a file grows, only the new part is read. If you are at the bottom, the view follows new lines.
- **Filter.** ⇧⌘L shows only the lines that contain some text or match a `/regex/`. On a 200 MB, 2.5 million line log this takes a few seconds.
- **Highlighting** for about 200 languages, on a background thread, only for the lines you scroll to.
- **Pages.** Markdown renders as a page and `.txt` reads as wrapped prose. Minified JSON opens formatted, and CSV/TSV files open as a table. ⇧⌘V switches to the raw lines.
- **Notes.** ⌘N (or ⌘T) creates a Markdown note in `~/Documents/Leaf Notes`. Notes save as you type and take their name from the first line.
- **Encodings.** Shift_JIS, Windows-1252, UTF-16 and others are detected, and saving keeps the original encoding, BOM and line endings.
- **Boards.** A Markdown file in the [Obsidian Kanban](https://github.com/mgmeyers/obsidian-kanban) format opens as columns of cards. Drag cards between columns, or use the keyboard. Every change is saved to the file right away, rewriting only the lines of the card that changed, so the same board works in Obsidian. ⇧⌘N makes a new board. See [Boards](#boards) for the format.
- **Watched folders.** Add a folder to the sidebar (⇧⌘O, the **+ Folder** button, or drop a folder on the window) and its text files are listed like a Notes folder, newest first, with subfolders up to four levels deep. Hidden files, build folders such as `node_modules` and `target`, and binary files are left out. The folder is re-listed every 3 seconds while it is shown, and nothing is read until you open a file. Browsing a folder doesn't add its files to Open Files.
- **Search.** The sidebar search (⇧⌘F) matches file names and the text inside every open file, or every file in the folder being shown, and shows the matching line. Opening a result jumps to it.
- **Compare.** ⇧⌘D compares the current file with the one you viewed before it, side by side. ⌥-click a file in the sidebar to compare with that one.
- **Safe saving.** Saves go to a temporary file that is renamed over the original, so a crash never leaves half a file. Leaf checks the file on disk before every save and will not overwrite changes made by another program without asking.
- **Recovery.** Unsaved edits are copied to `~/Library/Application Support/Leaf/Recovery` while you type. After a crash or a forced quit they come back the next time you open the file.
- **External changes.** Files changed by other programs reload when you come back to them. If you have unsaved edits, Leaf warns you instead.
- **Session.** Open files come back on the next launch, still unloaded until you click them.
- **Quick Open (⌘P)** jumps to any open file, file in a watched folder, or recently closed file. **Go to Heading (⇧⌘J)** jumps within a Markdown file.
- **Links.** ⌘-click a URL or a file reference such as `src/main.rs:42` in a log or in agent output to open it (in the browser, or in Leaf at that line). Links in rendered Markdown open the same way.
- **The `leaf` command.** `leaf notes.md`, `leaf src/main.rs:42` or `leaf ~/Projects/site` (a folder goes to the sidebar), from a terminal or a coding agent. `./scripts/install.sh` installs it; with a downloaded app, run `ln -s /Applications/Leaf.app/Contents/Resources/leaf /usr/local/bin/leaf`.
- **Logs.** In plain files, ERROR lines are red, WARN amber and DEBUG dimmed. Selecting a word marks it everywhere else on screen.
- **Wrap and zoom.** ⌥Z wraps code and logs to the window (files up to 1 MB). ⌘+ and ⌘− change the text size; Leaf remembers it.
- **Writing.** The toolbar shows the word count and reading time of notes and Markdown (or of the selection). Checklist boxes in a rendered page can be clicked to tick them, and double-clicking a paragraph starts editing there. ⌥⌘N opens today's note.
- **Pinned files (⇧⌘P)** stay in a Pinned section at the top of the sidebar.
- **Compare with Last Commit** (View menu) shows what changed in a file since the last git commit.
- **Reveal in Finder (⌥⌘R)**, **Copy Path (⌥⌘C)**, and a solid (not translucent) window option in the View menu.

| Code | Markdown | Editing a note |
|---|---|---|
| ![Code view](docs/screenshots/code.png) | ![Markdown view](docs/screenshots/markdown.png) | ![Editing](docs/screenshots/editing.png) |

## Boards

A board is an ordinary Markdown file. Columns are `##` headings and cards are task-list items:

```md
---

kanban-plugin: board

---

## Todo

- [ ] Write the parser #feature @{2026-11-20}
    Extra lines of a card are indented four spaces.
    - [x] checklist items show as progress on the card

## Done

**Complete**
- [x] Finished card
```

- `#tag` shows as a label and `@{date}` as a due date, in red once it has passed. Click a label to show only cards with it, or press ⌘F to filter by words, `#tag` or `overdue`.
- Double-click a column's name to rename it, drag it by its name to reorder, or use **+ Add column**. Empty columns can be removed with their ✕.
- Cards moved into a column marked `**Complete**` are ticked.
- Anything after `***` (Obsidian's archive) and the `%% kanban:settings %%` block are kept but not shown.

Coding agents such as Claude Code can manage a board by editing the file with their normal tools: add a `- [ ] ` line under a heading to create a card, move its lines to another heading to change its column, or change `[ ]` to `[x]` to tick it. Leaf reloads the file within a second or two and shows the change. If the board has unsaved changes in Leaf when that happens, Leaf flags the conflict instead of overwriting either side.

## Memory use

Measured with `footprint` on an Apple Silicon Mac, release build, after the window settled:

| Situation | Memory |
|---|---|
| Empty window | 57 MB |
| One small text file | 61 MB |
| One small source file, highlighted | 76 MB |
| 300 files open, one shown | 75 MB |
| 7 MB source file (200,000 lines) | 83 MB |
| 200 MB log file | 62 MB |

Images in a rendered Markdown page add their decoded size (about 7 MB for each 1600×1100 screenshot). The app is about 15 MB on disk.

## Install

Download `Leaf-<version>-macos-arm64.zip` from [Releases](https://github.com/shhossain/leaf/releases), unzip it, and move `Leaf.app` to `/Applications`. Builds are for Apple Silicon.

The app is signed ad hoc, not notarized, so macOS blocks it on first launch. Remove the quarantine flag once:

```sh
xattr -dr com.apple.quarantine /Applications/Leaf.app
```

### Build from source

You need Rust (the version is pinned in `rust-toolchain.toml`; rustup installs it automatically) and the Xcode command line tools.

```sh
git clone https://github.com/shhossain/leaf
cd leaf
./scripts/install.sh   # builds Leaf.app and copies it to /Applications
```

The first build takes a few minutes and about 3 GB of disk space.

### Make Leaf the default app

In Finder, choose Get Info on a file, pick Leaf under "Open with", then click Change All.

Or set several types at once from the repo:

```sh
swift scripts/set-default.swift --show txt md json                              # current defaults
swift scripts/set-default.swift /Applications/Leaf.app txt md log json yaml toml
```

The script prints the previous app for each type, so you can switch back the same way.

## Keyboard

| Keys | Action |
|---|---|
| ⌘O | Open files |
| ⌘P | Quick Open |
| ⇧⌘J | Go to heading |
| ⇧⌘O | Add a folder to the sidebar |
| ⌘N, ⌘T | New note |
| ⇧⌘N | New board |
| ⌥⌘N | Today's note |
| ⇧⌘P | Pin / unpin |
| ⌥⌘R, ⌥⌘C | Reveal in Finder, copy path |
| ⌥Z | Wrap long lines |
| ⌘+, ⌘−, ⌘0 | Text size |
| ⌘E, double-click | Edit / Done |
| ⌘S | Save |
| ⌘W | Close file |
| ⇧⌘T | Reopen closed file |
| ⌃Tab, ⇧⌃Tab, ⌥⌘→, ⌥⌘← | Next / previous file |
| ⌘1 … ⌘8, ⌘9 | Go to that row in the sidebar, last row |
| ⌘F, ⌘G, ⇧⌘G | Find, next, previous match |
| ⌘L | Go to line |
| ⇧⌘L | Filter lines |
| ⇧⌘V | Page, table or formatted view / raw lines |
| ⇧⌘D, ⌥-click | Compare with the previous file / with that file |
| ⌘B, ⌥⌘S | Show or hide the sidebar |
| ⇧⌘F, ⌥⌘F | Search the sidebar |
| Space, ⇧Space | Page down / up |
| ⌘↑, ⌘↓ | Top / bottom |
| Esc | Close the find or filter bar |

On a board:

| Keys | Action |
|---|---|
| Arrows | Select a card |
| ⌥ + arrows | Move the card |
| Return, double-click | Edit the card's title |
| N | New card in the column |
| Space | Tick or untick |
| ⌘⌫ | Delete the card |
| ⌘Z | Undo |
| ⌘F | Filter cards |
| ⇧⌘V | Board / Markdown source |

## How it works

Leaf is written in Rust on [GPUI](https://github.com/zed-industries/zed), the GPU-rendered UI framework from Zed, and [gpui-component](https://github.com/longbridge/gpui-component).

The viewer does not use a text editor widget. It keeps the file's bytes plus the start offset of every 64th line, and draws only the rows on screen. Syntax highlighting uses syntect on a worker thread per file: it saves the parser state every 128 lines, but only as far as you have scrolled, so a large file that you only glance at is never fully parsed. When you jump far ahead, lines are colored from an approximate state first and corrected once the exact pass catches up. The full editor (from gpui-component) is created only when you start editing a file, because it needs about twelve times the file's size in memory.

| File | Purpose |
|---|---|
| `src/workspace.rs` | Window, sidebar, open-file lifecycle, saving, session |
| `src/viewer.rs` | Read-only line view, find, filter, go to line, page, JSON and table views |
| `src/text_index.rs` | File bytes (in memory, or read from disk in blocks) and the sparse line index |
| `src/doc.rs` | Loading, appended-data detection, atomic writes |
| `src/highlight.rs` | Highlighting worker thread with checkpoints |
| `src/encoding.rs` | Encoding detection and round-trip on save |
| `src/file_list.rs` | Sidebar dates, preview lines, content search |
| `src/folders.rs` | Listing watched folders |
| `src/board.rs`, `src/board_ui.rs` | Kanban boards: parsing and edits, and the board view |
| `src/page.rs` | Rendered pages: jumping, clickable checklists, word count |
| `src/palette.rs` | Quick Open and Go to Heading |
| `scripts/leaf` | The `leaf` command |
| `src/recovery.rs` | Copies of unsaved edits |
| `src/notes.rs` | Notes folder, auto-naming |
| `src/style.rs` | Colors |

Run the tests with `cargo test`.

## Limitations

- Apple Silicon only, and not notarized.
- Files over 16 MB are view-only. Files over 64 MB are assumed to be UTF-8.
- The viewer cuts lines longer than 4,000 characters.
- Double-clicking a rendered Markdown page opens the editor at the top of the file, not at the paragraph you clicked.
- UTF-16 files without a byte-order mark are treated as binary.
- A file in an encoding that does not convert back exactly opens view-only.
- Search skips files over 16 MB (it still matches their names).
- A watched folder lists at most 5,000 files.
- Quitting from the Dock skips the unsaved-changes prompt; edits are kept as recovery copies instead.

## Releases

Pushing a tag such as `v0.2.0` runs `.github/workflows/release.yml`, which tests, builds `Leaf.app`, and attaches a zip to a GitHub release.

## License

Apache-2.0. The scope-to-color mapping in `src/highlight.rs` is adapted from gpui-component, which is also Apache-2.0.
