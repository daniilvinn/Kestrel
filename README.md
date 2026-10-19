<div align="center">

# Kestrel

**A fast, keyboard-first terminal file explorer for Windows.**

Vim-style keys · fuzzy find · live grep · rich previews · undoable file operations · tabs and bookmarks · optional Claude Code pane

[![CI](https://github.com/daniilvinn/Kestrel/actions/workflows/ci.yml/badge.svg)](https://github.com/daniilvinn/Kestrel/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/daniilvinn/Kestrel?sort=semver)](https://github.com/daniilvinn/Kestrel/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6)
![Rust](https://img.shields.io/badge/built%20with-Rust-dea584)

[Install](#install) · [Quick start](#quick-start) · [Features](#features) · [Configuration](#configuration) · [Claude Code](#claude-code) · [Wiki](https://github.com/daniilvinn/Kestrel/wiki)

<img src="docs/img/tour.gif" alt="A tour of Kestrel: browsing a project, previewing code, finding a file, searching contents and opening the command palette" width="900">

</div>

Kestrel (`ks`) is a terminal file explorer written in Rust with [ratatui](https://ratatui.rs). It
has calm chrome with a single accent colour and a prompt line at the bottom, and it stays fast on
huge folders: a directory of 100,000 entries shows its first rows in about a millisecond. It can
also run your own installed [Claude Code](https://claude.com/claude-code) next to the file list,
hand it files as `@` mentions and show you what it changed.

> Kestrel is not affiliated with or endorsed by Anthropic. It launches the Claude Code CLI you
> installed yourself, on your own subscription.

## Highlights

<table>
<tr>
<td width="50%" valign="top">

**Navigate at the speed of thought.** `hjkl`, history, bookmarks, a frecency jump (`z`) and a
which-key popup for every prefix. Git status per file, live updates when files change on disk,
and a **This PC** view that notices drives being plugged in.

</td>
<td width="50%" valign="top">

**Find anything.** `ctrl-p` fuzzy-finds files below the current folder while it walks the tree.
`ctrl-f` greps file contents as you type: grouped hits, regex and whole-word toggles, a glob
filter and a live preview of every hit.

</td>
</tr>
<tr>
<td valign="top">

**See before you open.** Syntax-highlighted code, rendered Markdown, images (Sixel or half
blocks), zip listings, hex dumps and full metadata. Previews are rendered off the UI thread, so
holding `j` never stutters.

</td>
<td valign="top">

**Change files safely.** Copy, move, trash and archive extraction run as background jobs with
progress, speed and ETA. Undo with `u`, bulk rename in your editor, a conflict dialog, and
permanent deletes only after you type `yes`.

</td>
</tr>
</table>

## Screenshots

| | |
|:---:|:---:|
| <img src="docs/img/main.png" alt="File list with git glyphs and a rendered Markdown preview"><br>**Browse**: git glyphs, sizes, rendered Markdown | <img src="docs/img/preview.png" alt="Rust source with syntax highlighting in the preview pane"><br>**Preview**: syntax-highlighted code |
| <img src="docs/img/find.png" alt="Find file overlay matching 'sky' with a preview of the highlighted file"><br>**Find file** (`ctrl-p`) | <img src="docs/img/search.png" alt="Search contents overlay with grouped hits and a preview scrolled to the hit"><br>**Search contents** (`ctrl-f`) |
| <img src="docs/img/palette.png" alt="Command palette listing commands and their keys"><br>**Command palette** (`:`) | <img src="docs/img/claude.png" alt="Claude Code running in a pane below the file list with two files sent as mentions"><br>**Claude Code pane** (`c`) |

<details>
<summary>More: image preview, help, This PC</summary>

| | |
|:---:|:---:|
| <img src="docs/img/image.png" alt="Image preview drawn with half blocks"><br>**Image preview** | <img src="docs/img/help.png" alt="Help overlay listing key bindings by group"><br>**Help** (`?`) |
| <img src="docs/img/drives.png" alt="This PC view with drives, file systems and free-space bars"><br>**This PC** (`g /`) | |

</details>

## Install

**Requirements:** Windows 10 1809+ or Windows 11, ideally in
[Windows Terminal](https://aka.ms/terminal) with a [Nerd Font](https://www.nerdfonts.com/) for
the icons (see [Icons](#icons) if you do not have one). `git` on `PATH` lights up the branch and
change glyphs.

**Download:** grab `ks.exe` (or the zip with the docs) from the
[latest release](https://github.com/daniilvinn/Kestrel/releases/latest) and put it in a folder on
your `PATH`.

**From source** (stable Rust, MSVC toolchain; see [BUILD.md](BUILD.md)):

```powershell
cargo install --git https://github.com/daniilvinn/Kestrel ks-app --locked
# or, from a clone:
cargo install --path crates/ks-app --locked
```

Then run it:

```powershell
ks              # open the current directory
ks C:\Users     # open a directory (a file opens its parent)
```

To let `Q` leave your shell in the folder you were browsing, add the shell function once
([Shell integration](#shell-integration)) and start Kestrel with `k`.

## Quick start

| Keys | What |
|---|---|
| `j` `k` / `↓` `↑` | move the cursor |
| `l` `enter` / `h` `backspace` | enter a directory or open a file / go to the parent |
| `/` | filter this directory as you type |
| `ctrl-p` | find a file anywhere below here |
| `ctrl-f` | search file contents |
| `space`, `y` `x` `p`, `d`, `u` | mark, copy / cut / paste, move to the Recycle Bin, undo |
| `:` | command palette: every command, with its keys |
| `?` | help: every key binding, filterable with `/` |
| `q` / `Q` | quit / quit and `cd` your shell there |

After a prefix key (`g`, `S`, `m`, `'`) a which-key popup lists what can follow. Every key can be
rebound, and [docs/keymap.md](docs/keymap.md) has the complete table, generated from the program.
New here? The wiki's [Getting Started](https://github.com/daniilvinn/Kestrel/wiki/Getting-Started)
is a ten-minute guided tour.

## Features

### Navigation

| Keys | Action |
|---|---|
| `j` `k`, `gg` `G`, `ctrl-d` `ctrl-u` | move, top / bottom, half page (also `home` `end` `pagedown` `pageup`) |
| `h` `l`, `backspace` `enter` | parent / enter a directory, or open a file with its default app |
| `ctrl-o` / `tab` | history back / forward |
| `~` `g h` `g d` `g D` `g c` `g /` | home, Downloads, Desktop, Kestrel's config folder, This PC |
| `.` | show or hide hidden files |
| `S` then `n` `s` `m` `e` | sort by name / size / modified time / extension (again: reverse) |
| `space`, `v`, `ctrl-a` | mark and move down, visual range, mark all |

- **This PC.** Above a drive root, `h` shows the drives with label, file system and a free-space
  bar. Plugging in or removing a drive updates the list within a second, and empty drives never
  raise a system dialog.
- **Big folders.** Listings stream in chunks and only visible rows are drawn. The folder refreshes
  by itself when files change on disk, and the cursor stays on its entry.
- **Git.** The header shows the branch and counts (`↑` ahead, `↓` behind, `+` staged, `~` modified,
  `?` untracked, `U` conflicts). Each row carries a glyph: `M` `+` `R` `D` `?` `!` `U`.
- **OneDrive.** Online-only placeholders get a cloud glyph. Preview and search never download them.

### Preview

The preview pane appears on terminals 80 columns wide or more. `w` forces it on or off, `P` toggles
a parent-directory column (100+ columns), `i` switches between content and metadata, and `J` `K`
scroll it. The preview renders:

- text with line numbers and syntax highlighting (UTF-8 and UTF-16 detected)
- Markdown
- directory listings and ZIP / RAR archive contents
- images (Sixel, Kitty or iTerm2 when the terminal speaks them, half blocks otherwise; see
  [Terminal graphics](#terminal-graphics))
- binary files as a hex dump
- metadata: size, times, attributes, link target and owner

It runs in the background, never blocks a key and is cancelled when the cursor moves on.

### Filter, find file, search contents

| Keys | What |
|---|---|
| `/` | filter the current directory; `enter` keeps the filter as a chip, `esc` clears it |
| `ctrl-p` or `f` | **find file**: recursive fuzzy match on the relative path, streaming while it walks |
| `ctrl-f` or `s` | **search contents**: live grep, smart case, literal by default |

<details>
<summary>Find file and search contents in detail</summary>

**Find file.**
- Moving: `↑` `↓` or `ctrl-n` `ctrl-p` move, and `tab` marks a result and moves down.
- Options: `ctrl-r` cycles the scope (current directory, git root, home, drive). `alt-.` toggles
  hidden files and `alt-g` toggles gitignored files; the highlighted file stays highlighted.
- Opening: `enter` goes to the file's folder with the cursor on it. Marks in one folder are
  selected there, and marks in several folders open a results list. `ctrl-enter` or `ctrl-o`
  open the files with their default app.
- Speed: the last walk of each root is cached and kept fresh from watcher events, so reopening is
  instant. Junctions and symlinks are never followed.

**Search contents.**
- Typing and filtering: type the query, then `tab` to the glob field (`*.rs`, `!target/**`).
  `ctrl-r` toggles regex and `ctrl-w` toggles whole words.
- Results: `enter` opens the hit in your editor at that line, and `ctrl-l` turns the matching files
  into a results list in a new tab. Hits are grouped by file and capped at 10,000. Binary files,
  files over 10 MB and cloud placeholders are skipped and counted in the footer.
- Claude: `alt-m` marks hits, and `alt-c` sends them to the Claude pane as `@path#Lnn` mentions.

On terminals at least 100 columns wide, both overlays preview the highlighted result live.

</details>

### File operations

| Keys | What |
|---|---|
| `y` `x` `p` | copy, cut, paste into the current directory |
| `d` / `D` | move to the Recycle Bin / delete permanently after typing `yes` |
| `u` | undo the last rename, move, create, trash or extraction |
| `X` | extract ZIP / RAR archives next to themselves (several top-level items get their own folder) |
| `r` / `R` | rename / bulk rename in your editor (with an `old -> new` preview to confirm) |
| `a` | create a file; a trailing `/` creates a directory (nested paths work) |
| `Y` | copy paths to the clipboard (absolute, relative or name) |
| `o` / `O` / `e` | open with the default app / choose an app / open in your editor |
| `!` | run a shell command here; the output streams into a pager |
| `g j` | jobs: progress, throughput and ETA; `x` cancels, `c` clears finished ones |

Copy, move and delete run as background jobs, and quitting while one runs asks first. On a name
conflict, choose `O` overwrite, `s` skip, `r` rename, `k` keep both or `c` cancel. Press `a` first
to apply the next overwrite, skip or keep-both choice to every remaining conflict. The editor is `[editor] open`, else
`$VISUAL`, `$EDITOR`, `code`, then `notepad`. Terminal editors (vim, nvim, hx, ...) suspend
Kestrel while they run. GUI editors start detached.

### Tabs, bookmarks and frecency

| Keys | What |
|---|---|
| `t` / `1`..`9` / `ctrl-w` | new tab / switch to a tab / close the tab |
| `m` then a key / `'` then a key | set a bookmark / jump to it; `M` lists them |
| `z` | frecency jump: type to filter the folders you visit most and most recently |

Bookmarks and visit history are kept in `%LOCALAPPDATA%\kestrel\state` and survive restarts.

### Shell integration

A program cannot change its parent shell's directory, so `Q` ("quit and cd") goes through a small
shell function. `q` always leaves the shell where it was.

```powershell
# PowerShell 7 and Windows PowerShell 5.1: add to your profile (notepad $PROFILE), then run `k`
ks --print-shell-init pwsh | Out-String | Invoke-Expression
```

```sh
# bash or zsh, including Git Bash
eval "$(ks --print-shell-init bash)"
```

<details>
<summary>A Windows Terminal profile for Kestrel</summary>

Add this to the `profiles.list` array of Windows Terminal's `settings.json`, with your own GUID
from `[guid]::NewGuid()`:

```json
{
    "guid": "{6b2f1d3e-84a0-4c57-9e1b-3d5a7c9f0e21}",
    "name": "Kestrel",
    "commandline": "pwsh.exe -NoLogo -NoExit -Command k",
    "startingDirectory": "%USERPROFILE%"
}
```

Use `"commandline": "ks.exe"` if you do not need `Q` to change the shell's directory, and
`powershell.exe` for Windows PowerShell 5.1.

</details>

#### Default file manager

`ks --set-default` makes Kestrel the default file manager for your Windows account: opening a
folder or a drive (from the desktop, the Start menu, a download prompt, another program) and
Win+E start Kestrel in a new PowerShell window instead of File Explorer. No administrator rights
are needed; Explorer stays one right-click away as "Open".

```powershell
ks --set-default     # folders, drives and Win+E open Kestrel
ks --unset-default   # back to File Explorer
```

The registration points at the `ks.exe` you ran it with, so run `--set-default` again after
moving it. `--unset-default` restores exactly what was there before, including another file
manager's registration, and leaves alone anything a different program changed in the meantime.
Kestrel opens in a new Windows Terminal window with Terminal's PowerShell profile (your font,
colours and icon), running PowerShell 7 (`pwsh`) when it is on your `PATH`, otherwise Windows
PowerShell, with your PowerShell profile loaded and the `k` function set up for you. `Q` therefore leaves the window as a PowerShell prompt in the directory you quit in; `q`
leaves it in the folder you opened. Without Windows Terminal it uses a classic console window;
run `--set-default` again after installing or removing Terminal or PowerShell 7.

### Icons

The default `icons = "nerd"` needs a [Nerd Font](https://www.nerdfonts.com/), for example
JetBrainsMono Nerd Font or Cascadia Code NF. Select it in Windows Terminal under Settings >
Profiles > Appearance > Font face. Without one, icons show as boxes; set `icons = "unicode"` or
`"ascii"` instead, or try them for the session with **Switch icon set** in the palette.

### Terminal graphics

Image previews use Sixel (Windows Terminal 1.22+), Kitty or iTerm2 graphics when the terminal
supports them, and half blocks otherwise. Kestrel asks the terminal at start-up, waiting at most
250 ms, and logs the answer as `image protocol detected=...`. To force a protocol:

```toml
[preview]
image_protocol = "sixel"   # "auto" | "halfblocks" | "sixel" | "kitty" | "iterm2"
```

## Claude Code

Kestrel can run your installed Claude Code beside the file list. `c` opens it in an embedded pane
and `ctrl-\` moves the keyboard between the explorer and the pane. Marked files go to Claude as
`@` mentions with `C`, and a line range from the preview goes with `V`. Claude can use the
explorer through an MCP server, and `g a` lists every file it edited, with a diff.

**Your subscription, never API billing.** Kestrel only starts your own, unmodified `claude` CLI.
It never uses the Anthropic API or the Agent SDK, never passes `--bare`, and never reads, copies
or stores credentials. Every `claude` it starts has these variables removed from its
environment: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN` and the
`CLAUDE_CODE_USE_BEDROCK` / `_VERTEX` / `_FOUNDRY` flags. Before each launch,
`claude auth status` must report a claude.ai login. If it reports an API key, a dialog warns you
and nothing starts unless you press `y`. Kestrel never sends a prompt itself: you type to Claude
in its pane.

**Nothing is installed behind your back.** The MCP server and the edit hook are injected per
launch with `--mcp-config` and `--settings`. Nothing is added to your Claude configuration, and no
`.mcp.json` is ever written into your projects.

| Keys | Action |
|---|---|
| `c` / `g w` | open Claude in an embedded pane / in a Windows Terminal split |
| `ctrl-\` | move the keyboard between the explorer and the pane (`[claude] focus_key`) |
| `C` | send the marked files (or the file under the cursor) as `@` mentions |
| `V` | in the text preview: select lines, then `C` sends `@path#Lstart-end` |
| `alt-=` / `alt--` / `g p` | grow / shrink the pane / move it between bottom and right |
| `shift-pageup` / `shift-pagedown` | scroll the pane's history |
| `g r` / `g X` | resume this project's last session / close the pane |
| `g a` | **Claude changes**: files Claude edited (`◆` in the list), with `git diff` |

<details>
<summary>MCP tools, hooks and the optional user-scope registration</summary>

Claude Code started by Kestrel gets an MCP server named `kestrel` (`ks mcp`, which talks to the
running Kestrel over a per-process named pipe). It has these tools:

- `get_selection`: the marked files, the cursor file and the current folder
- `search_files`: fuzzy search over Kestrel's cached index
- `search_contents`: a content search
- `list_dir`: a folder listing
- `reveal`: moves the explorer cursor to a path, so you see what Claude is talking about

A `PostToolUse` hook for `Edit|Write|MultiEdit|NotebookEdit` runs `ks hook`, which tells Kestrel
which file was touched.

You do not need to register the server yourself. If you also want it in sessions you start
outside Kestrel, run `claude mcp add --scope user kestrel -- ks mcp`. Its tools then answer
"Kestrel is not reachable" in those sessions, since only Kestrel-started sessions know the pipe.

</details>

More in the wiki: [Claude Code Integration](https://github.com/daniilvinn/Kestrel/wiki/Claude-Code-Integration).

## Configuration

Kestrel writes a commented `config.toml` on first run. A mistake in it never stops Kestrel from
starting: it falls back to the defaults and shows the error on the status line. Edit the file,
then run **Reload config** from the palette; there is no need to restart.

| Path | Content |
|---|---|
| `%APPDATA%\kestrel\config.toml` | settings and key overrides (`g c` opens the folder) |
| `%APPDATA%\kestrel\themes\*.toml` | your themes |
| `%LOCALAPPDATA%\kestrel\state\` | bookmarks, visit history, Claude sessions, logs |

`KESTREL_CONFIG_DIR` and `KESTREL_STATE_DIR` move these folders, for example for a portable
install.

<details>
<summary>Every setting with its default</summary>

The same text is [docs/config.example.toml](docs/config.example.toml), which a test keeps valid.

```toml
theme = "ember"          # "ember" (dark), "paper" (light) or a file themes\<name>.toml
icons = "nerd"           # "nerd" | "unicode" | "ascii"

[sort]
key = "name"             # "name" | "size" | "mtime" | "ext"
reverse = false
dirs_first = true

[files]
show_hidden = false      # dotfiles and hidden/system files
show_ignored = false     # gitignored files in find/search
size_base = 1024         # 1024 (KiB-style) or 1000 (SI)

[preview]
enabled = true
image_protocol = "auto"  # "auto" | "halfblocks" | "sixel" | "kitty" | "iterm2"
max_bytes = 262144       # read at most this much of a text file
max_lines = 2000

[layout]
list_percent = 40        # width of the list pane, 10..=90
parent_column = false    # the parent-directory column (P), needs 100 columns

[editor]
open = ""                # empty: $VISUAL, then $EDITOR, then "code", then "notepad"
open_at_line = "code -g {file}:{line}"   # {file} and {line} are substituted
terminal = "auto"        # "auto" | "always" | "never": suspend Kestrel while the editor runs

[ui]
which_key_delay_ms = 300

[claude]
binary = ""              # empty: look `claude` up on PATH
strip_env = [            # removed from every claude process
    "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY",
]
pane_position = "bottom" # "bottom" | "right"
pane_percent = 40        # share of the screen body, 15..=85
focus_key = 'ctrl-\'     # focus toggle between the explorer and the pane

[keymap.normal]          # examples
"H" = "dir.parent"
"L" = "dir.enter"
"ctrl-t" = "tab.new"
```

</details>

**Key bindings.** `[keymap.<context>]` tables map a key sequence (`"g g"`, `"ctrl-p"`, `"alt-."`)
to a command id from [docs/keymap.md](docs/keymap.md), or to `"none"` to unbind it.
`ctrl-`/`alt-` shortcuts follow the key's position, so they keep working with a non-Latin
keyboard layout. See [Key Bindings](https://github.com/daniilvinn/Kestrel/wiki/Key-Bindings).

**Themes.** `ember` (dark) and `paper` (light) are built in. A theme file in `themes\` inherits
every colour it does not set, so three lines are enough. See
[docs/theme.example.toml](docs/theme.example.toml) and
[Themes](https://github.com/daniilvinn/Kestrel/wiki/Themes).

## Troubleshooting

Logs are in `%LOCALAPPDATA%\kestrel\state\logs\`. `KESTREL_LOG=debug` adds detail, and
`KESTREL_LOG=ks_app::input=trace` logs every key the terminal sends.

- **Icons are boxes:** the font is not a Nerd Font; see [Icons](#icons).
- **A key does nothing** (`ctrl-enter` is the usual one): the terminal did not deliver it as a
  distinct key. Use the alternative (`ctrl-o`) or rebind it.
- **`Q` does not change the directory:** the `k` function is not loaded (`Get-Command k`).
- **Claude "would bill your API key":** run `claude` and use `/login` to sign in with your
  Pro/Max account.

More fixes are on the wiki's [Troubleshooting](https://github.com/daniilvinn/Kestrel/wiki/Troubleshooting)
page.

## Performance

These were measured on a release build. The method and numbers are in
[docs/performance.md](docs/performance.md).

| Budget | Measured |
|---|---|
| key press to frame < 16 ms (p99) | 0.28 ms |
| 100k-entry folder: first rows < 150 ms, complete < 1 s | 1.1 ms / 61 ms |
| fuzzy find keystroke on 500k paths < 25 ms | 13 to 19 ms |
| content search vs ripgrep, within 1.5× | 0.52 to 0.80× of rg's time |
| memory with a 1M-path finder cache < 250 MB | 206 MB |

## Contributing

Contributions are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers the workflow,
[Conventional Commits](https://www.conventionalcommits.org/) and the checks to run before a pull
request, and [BUILD.md](BUILD.md) explains building and testing. Please read the
[Code of Conduct](CODE_OF_CONDUCT.md), and report security issues as described in
[SECURITY.md](SECURITY.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE) © Daniil Vinnik
