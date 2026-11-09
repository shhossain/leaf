# Leaf

A small macOS text viewer and editor. Files open read-only, and editing is one keystroke away. Open files sit in a Notes-style sidebar and are only loaded when you look at them, so keeping hundreds open costs little.

![Leaf showing a note](docs/screenshots/notes.png)

Leaf exists because CotEditor slows down with many files open, and TextEdit is not a good viewer.

## Features

- **Viewer first.** Files open read-only. Press ⌘E or double-click to edit, and ⌘E again (or Done) to go back.
- **Lazy files.** A file is read when you open it, and only the 12 most recently viewed stay in memory. The sidebar reads just the first 2 KB of each file for its date and preview line.
- **Large files.** Files over 64 MB are memory-mapped and indexed in the background. A 200 MB log opens in about 0.2 s.
- **Highlighting** for about 200 languages, computed only for the lines you scroll to.
- **Pages.** Markdown renders as a page and `.txt` reads as wrapped prose. ⇧⌘V switches to numbered source lines.
- **Notes.** ⌘N creates a Markdown note in `~/Documents/Leaf Notes`. Notes save as you type and take their name from the first line.
- **Encodings.** Shift_JIS, Windows-1252, UTF-16 and others are detected, and saving keeps the original encoding, BOM and line endings.
- **External changes.** Files changed by other programs reload when you come back to them. If you have unsaved edits, Leaf warns you instead and asks before overwriting.
- **Session.** Open files come back on the next launch, still unloaded until you click them.

| Code | Markdown | Editing a note |
|---|---|---|
| ![Code view](docs/screenshots/code.png) | ![Markdown view](docs/screenshots/markdown.png) | ![Editing](docs/screenshots/editing.png) |

## Memory use

Measured with `footprint` on an Apple Silicon Mac, release build, after the window settled:

| Situation | Memory |
|---|---|
| Empty window | 55 MB |
| One small file | 59–73 MB |
| 300 files open, one shown | 73 MB |
| 7 MB source file (200,000 lines) | 91 MB |
| 200 MB log file | 61 MB |

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
| ⌘N | New note |
| ⌘E, double-click | Edit / Done |
| ⌘S | Save |
| ⌘W | Close file |
| ⇧⌘T | Reopen closed file |
| ⌃Tab, ⇧⌃Tab | Next / previous file |
| ⌘F, ⌘G, ⇧⌘G | Find, next, previous match |
| ⌘L | Go to line |
| ⇧⌘V | Page view / source lines |
| ⌥⌘S | Show or hide the sidebar |
| ⌥⌘F | Search open files |
| Space, ⇧Space | Page down / up |
| ⌘↑, ⌘↓ | Top / bottom |

## How it works

Leaf is written in Rust on [GPUI](https://github.com/zed-industries/zed), the GPU-rendered UI framework from Zed, and [gpui-component](https://github.com/longbridge/gpui-component).

The viewer does not use a text editor widget. It keeps the file's bytes plus the start offset of every 64th line, and draws only the rows on screen. Syntax highlighting uses syntect: a background pass saves the parser state every 128 lines, but only as far as you have scrolled, so a large file that you only glance at is never fully parsed. The full editor (from gpui-component) is created only when you start editing a file, because it needs about twelve times the file's size in memory.

| File | Purpose |
|---|---|
| `src/workspace.rs` | Window, sidebar, open-file lifecycle, saving, session |
| `src/viewer.rs` | Read-only line view, find, go to line, page view |
| `src/text_index.rs` | File bytes (owned or memory-mapped) and the sparse line index |
| `src/highlight.rs` | Visible-line syntax highlighting with checkpoints |
| `src/encoding.rs` | Encoding detection and round-trip on save |
| `src/notes.rs` | Notes folder, auto-naming |
| `src/style.rs` | Colors |

Run the tests with `cargo test`.

## Limitations

- Apple Silicon only, and not notarized.
- Files over 16 MB are view-only. Files over 64 MB are assumed to be UTF-8.
- The viewer cuts lines longer than 4,000 characters.
- Double-clicking a rendered Markdown page opens the editor at the top of the file, not at the paragraph you clicked.
- UTF-16 files without a byte-order mark are treated as binary.
- Quitting from the Dock skips the unsaved-changes prompt.

## Releases

Pushing a tag such as `v0.2.0` runs `.github/workflows/release.yml`, which tests, builds `Leaf.app`, and attaches a zip to a GitHub release.

## License

Apache-2.0. The scope-to-color mapping in `src/highlight.rs` is adapted from gpui-component, which is also Apache-2.0.
