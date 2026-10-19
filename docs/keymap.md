# Keymap

> Generated from the built-in keymap and command registry. Do not edit by hand:
> run `cargo run -p ks-app -- --dump-keymap > docs/keymap.md` (or
> `KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`) after changing a default binding.

Every command is also available in the command palette (`:`) and listed in the help
overlay (`?`), both of which show your own bindings after rebinding.

## Rebinding

Add `[keymap.<context>]` tables to `config.toml`. The key is a space-separated key
sequence, the value a command id from the tables below:

```toml
[keymap.normal]
"ctrl-t" = "tab.new"          # add a binding
"d" = "file.delete"           # rebind an existing key
"q" = "none"                  # unbind
"m <char>" = "bookmark.set"   # <char> passes the next typed key to the command
```

Contexts: `normal`, `visual` (inherits `normal`), `prompt`, `filter`, `finder`, `grep`,
`palette`, `help`, `confirm`, `jobs`, `claude`, `bookmarks`, `jump`, `conflict`, `pager`,
`menu`, `claude_changes`; each one is listed below under "Other contexts". In the help
overlay and the palette a command that only exists in one of them shows its key with the
context, for example `ctrl-r (finder)`. Binding a prefix (`g`) removes the longer
defaults that start with it (`g g`, `g h`, ...); the status line tells you when that
happens. Key names: single characters, `space`, `enter`, `esc`, `tab`, `backspace`,
`delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`,
with `ctrl-`, `alt-` prefixes (`ctrl-p`, `alt-.`). Reload the file without restarting from
the palette (`Reload config`).

## Normal mode

### Navigation

| Keys | Command | Id | What it does |
|---|---|---|---|
| `j`, `down` | Move cursor down | `cursor.down` | Move the cursor one row down. |
| `k`, `up` | Move cursor up | `cursor.up` | Move the cursor one row up. |
| `h`, `left`, `backspace` | Go to parent directory | `dir.parent` | Go to the parent directory, keeping the cursor on the directory you left. |
| `l`, `right`, `enter` | Enter directory / open file | `dir.enter` | Enter the directory under the cursor, or open the file. |
| `g g`, `home` | Jump to top | `cursor.top` | Jump to the first entry. |
| `G`, `end` | Jump to bottom | `cursor.bottom` | Jump to the last entry. |
| `ctrl-d`, `pagedown` | Scroll half a page down | `page.half_down` | Scroll half a page down. |
| `ctrl-u`, `pageup` | Scroll half a page up | `page.half_up` | Scroll half a page up. |
| `ctrl-o` | History: back | `history.back` | Go back to the previous directory. |
| `tab` | History: forward | `history.forward` | Go forward again after going back. |
| `~`, `g h` | Go to home directory | `go.home` | Jump to your home directory. |
| `g d` | Go to Downloads | `go.downloads` | Jump to the Downloads folder. |
| `g D` | Go to Desktop | `go.desktop` | Jump to the Desktop folder. |
| `g c` | Go to config directory | `go.config` | Jump to Kestrel's own config directory. |
| `g /` | Go to This PC (drives) | `go.drives` | Show all drives with free space. |

### Search

| Keys | Command | Id | What it does |
|---|---|---|---|
| `/` | Filter current directory | `filter.open` | Fuzzy-filter the current directory as you type. |
| `ctrl-p`, `f` | Find file (recursive fuzzy) | `find.file` | Find a file by fuzzy path match, recursively. |
| `ctrl-f`, `s` | Search file contents | `search.contents` | Search inside files (live grep). |
| — | Find file: cycle scope (cwd, git root, home, drive) | `finder.scope` | Find file: cycle the search scope (current directory, git root, home, drive). |
| — | Find file: toggle hidden files | `finder.hidden` | Find file: include or skip hidden files. |
| — | Find file: toggle gitignored files | `finder.gitignore` | Find file: include or skip files ignored by git. |
| — | Find file: open with default app | `finder.open` | Find file: open the marked results, or the highlighted one, with the default app. |
| — | Find file: mark result | `finder.mark` | Find file: mark the highlighted result and move down. |
| — | Search contents: toggle regex | `grep.regex` | Search contents: toggle regular-expression matching. |
| — | Search contents: toggle whole word | `grep.word` | Search contents: toggle whole-word matching. |
| — | Search contents: switch query/glob field | `grep.field` | Search contents: switch between the query and glob fields. |
| — | Search contents: open results as a list | `grep.to_list` | Search contents: show the files with matches as a results list in a new tab. |
| — | Search contents: mark the hit | `grep.mark` | Search contents: mark the highlighted hit so alt-c sends all marked hits to Claude. |

### Selection

| Keys | Command | Id | What it does |
|---|---|---|---|
| `space` | Toggle mark | `mark.toggle` | Mark or unmark the entry under the cursor. |
| `v` | Visual (range select) mode | `mode.visual` | Mark a range of entries while moving the cursor. |
| `ctrl-a` | Mark all | `mark.all` | Mark every visible entry. |

### File operations

| Keys | Command | Id | What it does |
|---|---|---|---|
| `y` | Copy | `clipboard.copy` | Copy the marked entries (or the one under the cursor). |
| `x` | Cut | `clipboard.cut` | Cut the marked entries to move them. |
| `p` | Paste | `clipboard.paste` | Paste copied or cut entries into this directory. |
| `Y` | Copy path to clipboard | `path.copy` | Copy absolute path(s) to the clipboard. |
| `r` | Rename | `file.rename` | Rename the entry under the cursor. |
| `R` | Bulk rename in editor | `file.bulk_rename` | Rename many entries by editing their names in your editor. |
| `a` | Create file or directory | `file.create` | Create a file; a trailing slash creates a directory. |
| `d` | Move to Recycle Bin | `file.trash` | Move the entries to the Recycle Bin. |
| `D` | Delete permanently | `file.delete` | Delete the entries for good after typed confirmation. |
| `X` | Extract archive | `file.extract` | Extract ZIP and RAR archives here; several top-level items get their own folder. |
| `u` | Undo last file operation | `file.undo` | Undo the last rename, move, create or trash. |
| `o` | Open with default app | `open.default` | Open with the default application. |
| `O` | Open with... | `open.with` | Choose which application opens the file. |
| `e` | Open in editor | `open.editor` | Open in your editor. |
| `!` | Run shell command | `shell.run` | Run a shell command in this directory. |

### View

| Keys | Command | Id | What it does |
|---|---|---|---|
| `.` | Toggle hidden files | `view.hidden` | Show or hide hidden files. |
| `S n` | Sort by name | `sort.name` | Sort by name; again to reverse. |
| `S s` | Sort by size | `sort.size` | Sort by size; again to reverse. |
| `S m` | Sort by modified time | `sort.mtime` | Sort by modified time; again to reverse. |
| `S e` | Sort by extension | `sort.ext` | Sort by extension; again to reverse. |
| `w` | Toggle preview | `preview.toggle` | Show or hide the preview pane. |
| `P` | Toggle parent column | `preview.parent_column` | Show or hide the parent directory column. |
| `i` | Toggle metadata view | `preview.metadata` | Switch the preview between content and metadata. |
| `J` | Scroll preview down | `preview.scroll_down` | Scroll the preview down. |
| `K` | Scroll preview up | `preview.scroll_up` | Scroll the preview up. |

### Tabs and bookmarks

| Keys | Command | Id | What it does |
|---|---|---|---|
| `m <char>` | Set bookmark | `bookmark.set` | Remember this directory under a key. |
| `' <char>` | Jump to bookmark | `bookmark.jump` | Jump to the directory saved under a key. |
| `M` | List bookmarks | `bookmark.list` | List the saved bookmarks and jump to one. |
| — | Delete bookmark | `bookmark.delete` | Delete the selected bookmark in the bookmark list. |
| `z` | Frecency jump | `frecency.jump` | Jump to a frequently or recently used directory. |
| `t` | New tab | `tab.new` | Open a new tab in this directory. |
| `1-9` | Switch to tab | `tab.goto` | Switch to a numbered tab. |
| `ctrl-w` | Close tab | `tab.close` | Close the current tab. |

### Claude Code

| Keys | Command | Id | What it does |
|---|---|---|---|
| — | Search contents: send the hit to Claude | `grep.send_claude` | Search contents: send the highlighted hit to the Claude pane as an @path#Lnn mention. |
| `V` | Select preview lines (visual lines) | `preview.select_lines` | Select lines of the text preview (j/k extend, esc cancels) to send as @path#La-b to Claude. |
| — | Send the selected preview lines to Claude | `preview.lines_send` | Send the selected preview lines to the Claude pane as an @path#La-b mention. |
| `c` | Open Claude | `claude.open` | Open Claude Code beside the explorer. |
| `C` | Send marked files to Claude | `claude.send_marked` | Send the marked files to Claude as @ mentions. |
| `g r` | Resume last Claude session | `claude.resume` | Resume the last Claude session of this project. |
| `g w` | Open Claude in Windows Terminal split | `claude.split` | Open Claude in a Windows Terminal split pane. |
| `ctrl-\` | Toggle focus between explorer and Claude | `claude.focus` | Move the keyboard between the explorer and the Claude pane. |
| `g X` | Close Claude pane | `claude.close` | Close the Claude pane and end its session process. |
| `alt-=` | Grow Claude pane | `claude.grow` | Make the Claude pane larger. |
| `alt--` | Shrink Claude pane | `claude.shrink` | Make the Claude pane smaller. |
| `g p` | Move Claude pane (bottom / right) | `claude.position` | Move the Claude pane between the bottom and the right. |
| — | Scroll Claude pane up | `claude.scroll_up` | Scroll the Claude pane towards older output. |
| — | Scroll Claude pane down | `claude.scroll_down` | Scroll the Claude pane towards newer output. |
| `g a` | Claude changes | `claude.changes` | List the files Claude changed, with their git diff. |
| — | Clear Claude changes | `claude.changes_clear` | Forget the list of files Claude changed. |
| — | Proceed with API billing anyway | `claude.proceed` | Launch Claude although it would bill the API key (billing question only). |
| — | Toggle the git diff in the Claude changes view | `claude.changes_diff` | Show or hide the git diff of the selected changed file. |

### Application

| Keys | Command | Id | What it does |
|---|---|---|---|
| `:` | Command palette | `palette.open` | Open the command palette. |
| `?` | Help | `help.open` | Show all key bindings and commands. |
| `g j` | Jobs overlay | `jobs.open` | Show running background jobs. |
| — | Cancel selected job | `job.cancel` | Cancel the selected job in the jobs overlay. |
| — | Clear finished jobs | `jobs.clear` | Drop finished jobs from the jobs overlay. |
| `esc` | Cancel / close | `cancel` | Close the top overlay, leave the mode, clear marks. |
| — | Accept / confirm | `accept` | Confirm in prompts and overlays. |
| — | Reload config | `config.reload` | Re-read config.toml, the keymap and the theme. |
| — | Switch theme | `theme.switch` | Choose a colour theme for this session. |
| — | Switch icon set | `icons.switch` | Choose nerd, unicode or ascii icons for this session. |
| `q`, `ctrl-c` | Quit | `quit` | Quit Kestrel. |
| `Q` | Quit and cd shell to current directory | `quit.cd` | Quit and make the shell wrapper cd to this directory. |

## Other contexts

### `visual` (also uses every `normal` binding it does not override)

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `v` | Visual (range select) mode | `mode.visual` |

### `prompt`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |

### `filter`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-n` | Move cursor down | `cursor.down` |
| `ctrl-p` | Move cursor up | `cursor.up` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `pageup` | Scroll half a page up | `page.half_up` |

### `finder`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-n` | Move cursor down | `cursor.down` |
| `ctrl-p` | Move cursor up | `cursor.up` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `pageup` | Scroll half a page up | `page.half_up` |
| `ctrl-r` | Find file: cycle scope (cwd, git root, home, drive) | `finder.scope` |
| `alt-.` | Find file: toggle hidden files | `finder.hidden` |
| `alt-g` | Find file: toggle gitignored files | `finder.gitignore` |
| `ctrl-enter` | Find file: open with default app | `finder.open` |
| `ctrl-o` | Find file: open with default app | `finder.open` |
| `tab` | Find file: mark result | `finder.mark` |

### `grep`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-n` | Move cursor down | `cursor.down` |
| `ctrl-p` | Move cursor up | `cursor.up` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `pageup` | Scroll half a page up | `page.half_up` |
| `ctrl-r` | Search contents: toggle regex | `grep.regex` |
| `ctrl-w` | Search contents: toggle whole word | `grep.word` |
| `tab` | Search contents: switch query/glob field | `grep.field` |
| `backtab` | Search contents: switch query/glob field | `grep.field` |
| `ctrl-l` | Search contents: open results as a list | `grep.to_list` |
| `alt-c` | Search contents: send the hit to Claude | `grep.send_claude` |
| `alt-m` | Search contents: mark the hit | `grep.mark` |

### `palette`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `down` | Move cursor down | `cursor.down` |
| `ctrl-n` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-p` | Move cursor up | `cursor.up` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `pageup` | Scroll half a page up | `page.half_up` |

### `help`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `?` | Cancel / close | `cancel` |
| `j` | Move cursor down | `cursor.down` |
| `down` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-d` | Scroll half a page down | `page.half_down` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `ctrl-u` | Scroll half a page up | `page.half_up` |
| `pageup` | Scroll half a page up | `page.half_up` |
| `g g` | Jump to top | `cursor.top` |
| `home` | Jump to top | `cursor.top` |
| `G` | Jump to bottom | `cursor.bottom` |
| `end` | Jump to bottom | `cursor.bottom` |
| `/` | Filter current directory | `filter.open` |

### `confirm`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |

### `jobs`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `x` | Cancel selected job | `job.cancel` |
| `c` | Clear finished jobs | `jobs.clear` |
| `j` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `g g` | Jump to top | `cursor.top` |
| `G` | Jump to bottom | `cursor.bottom` |

### `claude`

| Keys | Command | Id |
|---|---|---|
| `ctrl-\` | Toggle focus between explorer and Claude | `claude.focus` |
| `alt-=` | Grow Claude pane | `claude.grow` |
| `alt--` | Shrink Claude pane | `claude.shrink` |
| `shift-pageup` | Scroll Claude pane up | `claude.scroll_up` |
| `shift-pagedown` | Scroll Claude pane down | `claude.scroll_down` |

### `bookmarks`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `j` | Move cursor down | `cursor.down` |
| `down` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `up` | Move cursor up | `cursor.up` |
| `d` | Delete bookmark | `bookmark.delete` |
| `delete` | Delete bookmark | `bookmark.delete` |

### `jump`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `down` | Move cursor down | `cursor.down` |
| `ctrl-n` | Move cursor down | `cursor.down` |
| `ctrl-j` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-p` | Move cursor up | `cursor.up` |
| `ctrl-k` | Move cursor up | `cursor.up` |

### `conflict`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |

### `pager`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel selected job | `job.cancel` |
| `j` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |
| `g g` | Jump to top | `cursor.top` |
| `G` | Jump to bottom | `cursor.bottom` |
| `ctrl-d` | Scroll half a page down | `page.half_down` |
| `ctrl-u` | Scroll half a page up | `page.half_up` |
| `pagedown` | Scroll half a page down | `page.half_down` |
| `pageup` | Scroll half a page up | `page.half_up` |

### `menu`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `j` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `down` | Move cursor down | `cursor.down` |
| `up` | Move cursor up | `cursor.up` |

### `claude_changes`

| Keys | Command | Id |
|---|---|---|
| `esc` | Cancel / close | `cancel` |
| `q` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `enter` | Accept / confirm | `accept` |
| `j` | Move cursor down | `cursor.down` |
| `down` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `up` | Move cursor up | `cursor.up` |
| `g g` | Jump to top | `cursor.top` |
| `home` | Jump to top | `cursor.top` |
| `G` | Jump to bottom | `cursor.bottom` |
| `end` | Jump to bottom | `cursor.bottom` |
| `X` | Clear Claude changes | `claude.changes_clear` |
| `d` | Toggle the git diff in the Claude changes view | `claude.changes_diff` |

### `preview_lines`

| Keys | Command | Id |
|---|---|---|
| `j` | Move cursor down | `cursor.down` |
| `down` | Move cursor down | `cursor.down` |
| `k` | Move cursor up | `cursor.up` |
| `up` | Move cursor up | `cursor.up` |
| `ctrl-d` | Scroll half a page down | `page.half_down` |
| `ctrl-u` | Scroll half a page up | `page.half_up` |
| `g g` | Jump to top | `cursor.top` |
| `G` | Jump to bottom | `cursor.bottom` |
| `C` | Send the selected preview lines to Claude | `preview.lines_send` |
| `enter` | Send the selected preview lines to Claude | `preview.lines_send` |
| `esc` | Cancel / close | `cancel` |
| `ctrl-c` | Cancel / close | `cancel` |
| `V` | Cancel / close | `cancel` |
