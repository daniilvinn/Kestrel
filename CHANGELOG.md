# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ks --set-default` makes Kestrel the default file manager on Windows (folders, drives and
  Win+E open Kestrel in a PowerShell window, where `Q` leaves you in the directory you quit in);
  `ks --unset-default` gives the job back to File Explorer and restores the
  registry values it replaced.
- `X` extracts ZIP and RAR archives (RAR 5 included) as a background job with progress,
  conflict handling and undo. An archive with one top-level item is extracted in place; anything
  else gets a folder named after the archive. Entries that would escape the destination, links
  and password-protected entries are skipped and reported.
- The preview lists the contents of RAR archives, not only ZIP.

## [0.1.0] - 2026-10-11

First release.

### Added

#### Navigation

- Vim-style navigation: `j` `k`, `gg` `G`, half-page scrolling, enter and leave directories, back
  and forward history, jumps to home, Downloads, Desktop, the config directory and This PC.
- This PC view above a drive root, with label, file system and a free-space bar; it updates
  within a second when a drive is plugged in or removed, and empty drives never raise a system
  dialog.
- Streaming directory listings that draw only the visible rows, so directories with 100,000
  entries open instantly; the list refreshes by itself when files change on disk.
- Sorting by name, size, modified time or extension, hidden-file toggle, marks, visual range
  selection and select-all.
- Git integration: branch and ahead/behind/staged/modified/untracked/conflict counts in the header
  and a status glyph on every row. OneDrive placeholders are marked and never downloaded.

#### Preview

- Background preview pane that never blocks keys and is cancelled when the cursor moves.
- Syntax-highlighted text with line numbers (UTF-8 and UTF-16 with BOM), Markdown, directory
  listings, zip archive contents, images and hex dumps for binary files.
- Image previews over Sixel, Kitty or iTerm2 graphics with a half-block fallback and automatic
  protocol detection.
- Metadata view for any entry; files that must not be read (online-only placeholders, very large
  or locked files) show the reason instead.
- File content is sanitised so it cannot inject terminal escape sequences.

#### Find and search

- Filter the current directory as you type.
- Find-file overlay: recursive fuzzy match with results streaming in while walking, scopes
  (directory, git root, home, drive), hidden and gitignored toggles, multi-select, a cached index
  kept fresh by the file watcher, and a live preview.
- Content-search overlay: live grep with smart case, literal or regex, whole-word and glob
  filtering, results grouped by file, open at the matching line in your editor, or list the
  matching files in a new tab.

#### Command palette, help, keymap and themes

- Command palette (`:`) listing every command with its keys, and a filterable help overlay (`?`)
  with a which-key popup for prefix keys.
- Fully rebindable key map per context in `config.toml`, with `docs/keymap.md` generated from the
  program; `ctrl-` and `alt-` chords keep working on non-Latin keyboard layouts.
- Commented `config.toml` written on first run, validated with clear errors that never stop
  Kestrel from starting, and reloadable without a restart.
- `KESTREL_CONFIG_DIR` and `KESTREL_STATE_DIR` move the configuration and state directories
  (portable installs).
- Two built-in themes (`ember` dark, `paper` light) and user themes as small TOML files, with
  256-colour fallback; `nerd`, `unicode` and `ascii` icon sets.

#### File operations

- Copy, cut, paste, move to the Recycle Bin, permanent delete with typed confirmation, rename, bulk
  rename in your editor with a preview, and create files and directories (nested paths work).
- Copy, move and delete run as cancellable background jobs with progress, throughput and ETA, a
  jobs overlay, and a conflict prompt (overwrite, skip, rename, keep both, apply to all).
- Undo for rename, move, create and trash.
- Copy absolute, relative or name paths to the clipboard; open with the default app, choose an
  app, or open in your editor; run a shell command in the current directory with streaming output.

#### Tabs, bookmarks, frecency and shell integration

- Up to nine tabs, bookmarks (`m` / `'`) and a frecency jump (`z`) over the directories you visit
  most, persisted under `%LOCALAPPDATA%\kestrel\state`.
- Shell integration for PowerShell 5.1, PowerShell 7, bash and zsh (`--print-shell-init`,
  `--cwd-file`) so that `Q` quits and changes your shell's directory.
- A Windows Terminal profile snippet in the README.

#### Claude Code integration

- Optional embedded pane running your own installed Claude Code in a ConPTY terminal, or a
  Windows Terminal split; resume the last session of a project.
- Subscription only: API-billing environment variables are removed from every `claude` process,
  a preflight (`claude auth status`) refuses API-key and cloud-provider logins unless you confirm,
  `--bare` is never passed, and credentials are never read or stored.
- Send marked files, or selected lines of a preview, to the pane as `@` mentions; send grep hits
  as `@path#Lnn` mentions.
- A per-spawn MCP server (`ks mcp`) that lets Claude read the selection, search files and
  contents, list directories and reveal a path in the explorer, and a `PostToolUse` hook
  (`ks hook`) that marks files Claude changed, with a **Claude changes** view and diff.
- Nothing is installed behind your back: MCP and hook configuration are injected per spawn and no
  `.mcp.json` is written into your projects.

#### Performance

- Key press to frame in well under a millisecond (budget 16 ms), a 100,000-entry directory listed
  completely in about 60 ms, fuzzy finder keystrokes on 500,000 paths in about 12 ms, and content
  search faster than `ripgrep` wall time on the measured corpora. See
  [docs/performance.md](docs/performance.md).
- A single static `ks.exe` (static C runtime, no Visual C++ redistributable needed).

[Unreleased]: https://github.com/daniilvinn/Kestrel/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/daniilvinn/Kestrel/releases/tag/v0.1.0
