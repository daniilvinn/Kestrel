//! The keymap as data: key sequences bound to actions, per [`KeyContext`].
//!
//! * A binding is a sequence of [`KeyPattern`]s (`g g`, `ctrl-p`, `m <char>`).
//! * [`Keymap::resolve`] consumes one key at a time together with the keys already
//!   pending, and answers "action", "need more keys" or "no binding".
//! * Users override bindings with `[keymap.<context>]` tables in `config.toml`;
//!   see [`Keymap::apply_overrides`].

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::action::Action;
use crate::key::{Key, KeyChord, KeyParseError};
use crate::mode::KeyContext;
use crate::mode::KeyContext as C;

/// One step of a key sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyPattern {
    /// Exactly this chord.
    Chord(KeyChord),
    /// Any unmodified character key (`<char>`); the character is passed to the action
    /// as its argument (`m <char>` sets the bookmark named by the next key).
    AnyChar,
}

impl KeyPattern {
    /// `None` when `chord` does not match; `Some(captured)` when it does.
    fn matches(self, chord: KeyChord) -> Option<Option<char>> {
        match self {
            KeyPattern::Chord(c) => (c == chord).then_some(None),
            KeyPattern::AnyChar => match chord {
                KeyChord {
                    key: Key::Char(c),
                    mods,
                } if mods.is_empty() => Some(Some(c)),
                _ => None,
            },
        }
    }

    /// True when some key press could match both patterns.
    fn overlaps(self, other: KeyPattern) -> bool {
        match (self, other) {
            (KeyPattern::AnyChar, KeyPattern::AnyChar) => true,
            (KeyPattern::AnyChar, KeyPattern::Chord(c))
            | (KeyPattern::Chord(c), KeyPattern::AnyChar) => {
                self.matches(c).is_some() || other.matches(c).is_some()
            }
            (KeyPattern::Chord(a), KeyPattern::Chord(b)) => a == b,
        }
    }
}

impl fmt::Display for KeyPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyPattern::Chord(c) => write!(f, "{c}"),
            KeyPattern::AnyChar => f.write_str("<char>"),
        }
    }
}

impl FromStr for KeyPattern {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "<char>" {
            Ok(KeyPattern::AnyChar)
        } else {
            s.parse().map(KeyPattern::Chord)
        }
    }
}

/// An action together with its optional character argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub action: Action,
    pub arg: Option<char>,
}

/// Why a binding could not be built from text.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BindingError {
    #[error("no keys given")]
    NoKeys,
    #[error("invalid key: {0}")]
    Key(#[from] KeyParseError),
    #[error("unknown action `{0}`")]
    UnknownAction(String),
    #[error("action argument must be a single character, got `{0}`")]
    BadArg(String),
    #[error("`<char>` may appear only once and cannot be combined with an explicit argument")]
    AmbiguousArg,
}

/// A key sequence bound to an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: Vec<KeyPattern>,
    pub action: Action,
    /// Fixed argument (`tab.goto:3`). Mutually exclusive with `<char>` in `keys`.
    pub arg: Option<char>,
}

impl Binding {
    /// Parses `keys` (space separated, e.g. `"g g"`) and `target` (`"action"` or
    /// `"action:arg"`).
    pub fn parse(keys: &str, target: &str) -> Result<Self, BindingError> {
        let keys = keys
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<KeyPattern>, _>>()?;
        if keys.is_empty() {
            return Err(BindingError::NoKeys);
        }
        let (id, arg) = match target.split_once(':') {
            Some((id, arg)) => {
                let mut chars = arg.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => (id, Some(c)),
                    _ => return Err(BindingError::BadArg(arg.to_owned())),
                }
            }
            None => (target, None),
        };
        let action =
            Action::from_id(id.trim()).ok_or_else(|| BindingError::UnknownAction(id.to_owned()))?;
        let wildcards = keys.iter().filter(|k| **k == KeyPattern::AnyChar).count();
        if wildcards > 1 || (wildcards == 1 && arg.is_some()) {
            return Err(BindingError::AmbiguousArg);
        }
        Ok(Self { keys, action, arg })
    }

    /// The keys as text, e.g. `g g`.
    pub fn keys_display(&self) -> String {
        join_patterns(&self.keys)
    }
}

fn join_patterns(keys: &[KeyPattern]) -> String {
    keys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

enum Matched {
    Exact(Option<char>),
    Prefix,
}

/// Matches `seq` against the start of `binding`.
fn match_binding(binding: &Binding, seq: &[KeyChord]) -> Option<Matched> {
    if seq.len() > binding.keys.len() {
        return None;
    }
    let mut captured = None;
    for (pattern, chord) in binding.keys.iter().zip(seq) {
        captured = pattern.matches(*chord)?.or(captured);
    }
    Some(if seq.len() == binding.keys.len() {
        Matched::Exact(captured)
    } else {
        Matched::Prefix
    })
}

/// True when one sequence is a prefix of the other (or they overlap completely), which
/// would make resolution ambiguous.
fn sequences_conflict(a: &[KeyPattern], b: &[KeyPattern]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.overlaps(*y))
}

/// Outcome of feeding one key to [`Keymap::resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The sequence is complete.
    Action(Invocation),
    /// The keys so far are a proper prefix of at least one binding.
    Pending,
    /// Nothing is bound to this sequence.
    NoMatch,
}

/// What follows a pending prefix, for the which-key popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationTarget {
    Action(Action),
    /// More keys are needed after this one.
    Group,
}

/// One possible next key after a pending prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Continuation {
    pub key: KeyPattern,
    pub target: ContinuationTarget,
}

/// A rejected default binding (a bug in the built-in table).
#[derive(Debug, Error)]
pub enum KeymapError {
    #[error("default binding `{keys}` in `{context}`: {source}")]
    Invalid {
        context: KeyContext,
        keys: String,
        source: BindingError,
    },
    #[error("default binding `{keys}` in `{context}` conflicts with `{existing}`")]
    Conflict {
        context: KeyContext,
        keys: String,
        existing: String,
    },
}

/// All bindings, grouped by context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    tables: BTreeMap<KeyContext, Vec<Binding>>,
}

impl Keymap {
    /// Bindings of one context, in definition order.
    pub fn bindings(&self, ctx: KeyContext) -> &[Binding] {
        self.tables.get(&ctx).map_or(&[], Vec::as_slice)
    }

    /// Builds a keymap from `(context, keys, action)` triples, rejecting conflicts.
    pub fn from_specs(specs: &[(KeyContext, &str, &str)]) -> Result<Self, KeymapError> {
        let mut map = Keymap::default();
        for &(context, keys, target) in specs {
            let binding = Binding::parse(keys, target).map_err(|source| KeymapError::Invalid {
                context,
                keys: keys.to_owned(),
                source,
            })?;
            let table = map.tables.entry(context).or_default();
            if let Some(existing) = table
                .iter()
                .find(|b| sequences_conflict(&b.keys, &binding.keys))
            {
                return Err(KeymapError::Conflict {
                    context,
                    keys: keys.to_owned(),
                    existing: existing.keys_display(),
                });
            }
            table.push(binding);
        }
        Ok(map)
    }

    /// Applies `[keymap.<context>]` tables from the config. Invalid entries are skipped and
    /// reported; one bad line never discards the rest.
    ///
    /// A user binding replaces any built-in binding it conflicts with (binding `g` alone
    /// removes the `g g`, `g h`, ... defaults). The value `"none"` removes a binding.
    pub fn apply_overrides(
        &mut self,
        overrides: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Vec<String> {
        let mut issues = Vec::new();
        let mut user_defined: HashSet<(KeyContext, Vec<KeyPattern>)> = HashSet::new();
        for (ctx_name, entries) in overrides {
            let ctx = match ctx_name.parse::<KeyContext>() {
                Ok(ctx) => ctx,
                Err(e) => {
                    issues.push(e);
                    continue;
                }
            };
            for (keys, target) in entries {
                if matches!(target.trim(), "none" | "") {
                    match keys
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<Vec<KeyPattern>, _>>()
                    {
                        Ok(seq) => {
                            let table = self.tables.entry(ctx).or_default();
                            let before = table.len();
                            table.retain(|b| b.keys != seq);
                            if table.len() == before {
                                issues.push(format!(
                                    "keymap.{ctx} `{keys}` = `none`: nothing is bound to it"
                                ));
                            }
                        }
                        Err(e) => issues.push(format!("keymap.{ctx} `{keys}`: {e}")),
                    }
                    continue;
                }
                let binding = match Binding::parse(keys, target) {
                    Ok(b) => b,
                    Err(e) => {
                        issues.push(format!("keymap.{ctx} `{keys}` = `{target}`: {e}"));
                        continue;
                    }
                };
                let table = self.tables.entry(ctx).or_default();
                let clashes_with_user = table.iter().find(|b| {
                    b.keys != binding.keys
                        && sequences_conflict(&b.keys, &binding.keys)
                        && user_defined.contains(&(ctx, b.keys.clone()))
                });
                if let Some(other) = clashes_with_user {
                    issues.push(format!(
                        "keymap.{ctx} `{keys}` conflicts with your binding `{}`",
                        other.keys_display()
                    ));
                    continue;
                }
                // Binding `g` removes every `g ...` default (and `g x` removes a plain `g`).
                // Replacing the very same keys is an ordinary rebind and stays silent.
                let shadowed: Vec<String> = table
                    .iter()
                    .filter(|b| {
                        b.keys != binding.keys && sequences_conflict(&b.keys, &binding.keys)
                    })
                    .map(Binding::keys_display)
                    .collect();
                if !shadowed.is_empty() {
                    issues.push(format!(
                        "keymap.{ctx} `{keys}` shadows `{}`, which no longer work",
                        shadowed.join("`, `")
                    ));
                }
                table.retain(|b| !sequences_conflict(&b.keys, &binding.keys));
                user_defined.insert((ctx, binding.keys.clone()));
                table.push(binding);
            }
        }
        issues
    }

    /// Feeds `key` after the already `pending` keys.
    pub fn resolve(&self, ctx: KeyContext, pending: &[KeyChord], key: KeyChord) -> Resolution {
        let mut seq = pending.to_vec();
        seq.push(key);

        let mut is_prefix = false;
        let mut fallback_hit = None;
        for (depth, context) in self.chain(ctx).enumerate() {
            for binding in self.bindings(context) {
                match match_binding(binding, &seq) {
                    Some(Matched::Exact(arg)) => {
                        let invocation = Invocation {
                            action: binding.action,
                            arg: binding.arg.or(arg),
                        };
                        if depth == 0 {
                            return Resolution::Action(invocation);
                        }
                        fallback_hit.get_or_insert(invocation);
                    }
                    Some(Matched::Prefix) => is_prefix = true,
                    None => {}
                }
            }
        }
        if is_prefix {
            Resolution::Pending
        } else if let Some(invocation) = fallback_hit {
            Resolution::Action(invocation)
        } else {
            Resolution::NoMatch
        }
    }

    /// Possible next keys after `pending`, in binding definition order.
    pub fn continuations(&self, ctx: KeyContext, pending: &[KeyChord]) -> Vec<Continuation> {
        let mut found: Vec<Continuation> = Vec::new();
        for context in self.chain(ctx) {
            for binding in self.bindings(context) {
                if !matches!(match_binding(binding, pending), Some(Matched::Prefix)) {
                    continue;
                }
                let key = binding.keys[pending.len()];
                let target = if binding.keys.len() == pending.len() + 1 {
                    ContinuationTarget::Action(binding.action)
                } else {
                    ContinuationTarget::Group
                };
                match found.iter_mut().find(|c| c.key == key) {
                    Some(existing) if existing.target != target => {
                        existing.target = ContinuationTarget::Group;
                    }
                    Some(_) => {}
                    None => found.push(Continuation { key, target }),
                }
            }
        }
        found
    }

    /// Key sequences bound to `action` in `ctx` (including its fallback context).
    pub fn keys_for(&self, ctx: KeyContext, action: Action) -> Vec<String> {
        self.chain(ctx)
            .flat_map(|context| self.bindings(context))
            .filter(|b| b.action == action)
            .map(Binding::keys_display)
            .collect()
    }

    fn chain(&self, ctx: KeyContext) -> impl Iterator<Item = KeyContext> {
        std::iter::once(ctx).chain(ctx.fallback())
    }
}

/// The built-in bindings. Keys are space-separated sequences.
pub const DEFAULT_BINDINGS: &[(KeyContext, &str, &str)] = &[
    (C::Normal, "j", "cursor.down"),
    (C::Normal, "down", "cursor.down"),
    (C::Normal, "k", "cursor.up"),
    (C::Normal, "up", "cursor.up"),
    (C::Normal, "h", "dir.parent"),
    (C::Normal, "left", "dir.parent"),
    (C::Normal, "backspace", "dir.parent"),
    (C::Normal, "l", "dir.enter"),
    (C::Normal, "right", "dir.enter"),
    (C::Normal, "enter", "dir.enter"),
    (C::Normal, "g g", "cursor.top"),
    (C::Normal, "G", "cursor.bottom"),
    (C::Normal, "home", "cursor.top"),
    (C::Normal, "end", "cursor.bottom"),
    (C::Normal, "ctrl-d", "page.half_down"),
    (C::Normal, "pagedown", "page.half_down"),
    (C::Normal, "ctrl-u", "page.half_up"),
    (C::Normal, "pageup", "page.half_up"),
    (C::Normal, "ctrl-o", "history.back"),
    // ctrl-i is indistinguishable from tab in a terminal.
    (C::Normal, "tab", "history.forward"),
    (C::Normal, "~", "go.home"),
    (C::Normal, "g h", "go.home"),
    (C::Normal, "g d", "go.downloads"),
    (C::Normal, "g D", "go.desktop"),
    (C::Normal, "g c", "go.config"),
    (C::Normal, "g /", "go.drives"),
    (C::Normal, "g j", "jobs.open"),
    (C::Normal, "/", "filter.open"),
    (C::Normal, "ctrl-p", "find.file"),
    (C::Normal, "f", "find.file"),
    (C::Normal, "ctrl-f", "search.contents"),
    (C::Normal, "s", "search.contents"),
    (C::Normal, ":", "palette.open"),
    (C::Normal, "?", "help.open"),
    (C::Normal, "space", "mark.toggle"),
    (C::Normal, "v", "mode.visual"),
    (C::Normal, "ctrl-a", "mark.all"),
    (C::Normal, "y", "clipboard.copy"),
    (C::Normal, "x", "clipboard.cut"),
    (C::Normal, "p", "clipboard.paste"),
    (C::Normal, "Y", "path.copy"),
    (C::Normal, "r", "file.rename"),
    (C::Normal, "R", "file.bulk_rename"),
    (C::Normal, "a", "file.create"),
    (C::Normal, "d", "file.trash"),
    (C::Normal, "D", "file.delete"),
    (C::Normal, "X", "file.extract"),
    (C::Normal, "u", "file.undo"),
    (C::Normal, "o", "open.default"),
    (C::Normal, "O", "open.with"),
    (C::Normal, "e", "open.editor"),
    (C::Normal, "!", "shell.run"),
    (C::Normal, ".", "view.hidden"),
    // `S` opens the sort menu (the which-key popup); choosing the active key again reverses.
    (C::Normal, "S n", "sort.name"),
    (C::Normal, "S s", "sort.size"),
    (C::Normal, "S m", "sort.mtime"),
    (C::Normal, "S e", "sort.ext"),
    (C::Normal, "m <char>", "bookmark.set"),
    (C::Normal, "' <char>", "bookmark.jump"),
    (C::Normal, "M", "bookmark.list"),
    (C::Normal, "z", "frecency.jump"),
    (C::Normal, "t", "tab.new"),
    (C::Normal, "1", "tab.goto:1"),
    (C::Normal, "2", "tab.goto:2"),
    (C::Normal, "3", "tab.goto:3"),
    (C::Normal, "4", "tab.goto:4"),
    (C::Normal, "5", "tab.goto:5"),
    (C::Normal, "6", "tab.goto:6"),
    (C::Normal, "7", "tab.goto:7"),
    (C::Normal, "8", "tab.goto:8"),
    (C::Normal, "9", "tab.goto:9"),
    (C::Normal, "ctrl-w", "tab.close"),
    (C::Normal, "w", "preview.toggle"),
    (C::Normal, "P", "preview.parent_column"),
    (C::Normal, "i", "preview.metadata"),
    (C::Normal, "J", "preview.scroll_down"),
    (C::Normal, "K", "preview.scroll_up"),
    (C::Normal, "c", "claude.open"),
    (C::Normal, "C", "claude.send_marked"),
    (C::Normal, "V", "preview.select_lines"),
    (C::PreviewLines, "j", "cursor.down"),
    (C::PreviewLines, "down", "cursor.down"),
    (C::PreviewLines, "k", "cursor.up"),
    (C::PreviewLines, "up", "cursor.up"),
    (C::PreviewLines, "ctrl-d", "page.half_down"),
    (C::PreviewLines, "ctrl-u", "page.half_up"),
    (C::PreviewLines, "g g", "cursor.top"),
    (C::PreviewLines, "G", "cursor.bottom"),
    (C::PreviewLines, "C", "preview.lines_send"),
    (C::PreviewLines, "enter", "preview.lines_send"),
    (C::PreviewLines, "esc", "cancel"),
    (C::PreviewLines, "ctrl-c", "cancel"),
    (C::PreviewLines, "V", "cancel"),
    (C::Normal, "g r", "claude.resume"),
    (C::Normal, "g w", "claude.split"),
    (C::Normal, "g a", "claude.changes"),
    (C::Normal, "g X", "claude.close"),
    (C::Normal, "g p", "claude.position"),
    (C::Normal, "ctrl-\\", "claude.focus"),
    (C::Normal, "alt-=", "claude.grow"),
    (C::Normal, "alt--", "claude.shrink"),
    (C::Normal, "q", "quit"),
    (C::Normal, "Q", "quit.cd"),
    (C::Normal, "ctrl-c", "quit"),
    (C::Normal, "esc", "cancel"),
    // Visual mode inherits every Normal binding that it does not override.
    (C::Visual, "esc", "cancel"),
    (C::Visual, "v", "mode.visual"),
    // Text-entry modes and overlays: character input is handled by the input widget.
    (C::Prompt, "esc", "cancel"),
    (C::Prompt, "ctrl-c", "cancel"),
    (C::Prompt, "enter", "accept"),
    (C::Filter, "esc", "cancel"),
    (C::Filter, "ctrl-c", "cancel"),
    (C::Filter, "enter", "accept"),
    (C::Finder, "esc", "cancel"),
    (C::Finder, "ctrl-c", "cancel"),
    (C::Finder, "enter", "accept"),
    (C::Grep, "esc", "cancel"),
    (C::Grep, "ctrl-c", "cancel"),
    (C::Grep, "enter", "accept"),
    // In-directory filter, find file and content search. Printable characters
    // go to the input field; these are the non-text keys.
    (C::Filter, "down", "cursor.down"),
    (C::Filter, "up", "cursor.up"),
    (C::Filter, "ctrl-n", "cursor.down"),
    (C::Filter, "ctrl-p", "cursor.up"),
    (C::Filter, "pagedown", "page.half_down"),
    (C::Filter, "pageup", "page.half_up"),
    (C::Finder, "down", "cursor.down"),
    (C::Finder, "up", "cursor.up"),
    (C::Finder, "ctrl-n", "cursor.down"),
    (C::Finder, "ctrl-p", "cursor.up"),
    (C::Finder, "pagedown", "page.half_down"),
    (C::Finder, "pageup", "page.half_up"),
    (C::Finder, "ctrl-r", "finder.scope"),
    (C::Finder, "alt-.", "finder.hidden"),
    (C::Finder, "alt-g", "finder.gitignore"),
    (C::Finder, "ctrl-enter", "finder.open"),
    (C::Finder, "ctrl-o", "finder.open"),
    (C::Finder, "tab", "finder.mark"),
    (C::Grep, "down", "cursor.down"),
    (C::Grep, "up", "cursor.up"),
    (C::Grep, "ctrl-n", "cursor.down"),
    (C::Grep, "ctrl-p", "cursor.up"),
    (C::Grep, "pagedown", "page.half_down"),
    (C::Grep, "pageup", "page.half_up"),
    (C::Grep, "ctrl-r", "grep.regex"),
    (C::Grep, "ctrl-w", "grep.word"),
    (C::Grep, "tab", "grep.field"),
    (C::Grep, "backtab", "grep.field"),
    (C::Grep, "ctrl-l", "grep.to_list"),
    (C::Grep, "alt-c", "grep.send_claude"),
    (C::Grep, "alt-m", "grep.mark"),
    (C::Palette, "esc", "cancel"),
    (C::Palette, "ctrl-c", "cancel"),
    (C::Palette, "enter", "accept"),
    (C::Palette, "down", "cursor.down"),
    (C::Palette, "ctrl-n", "cursor.down"),
    (C::Palette, "up", "cursor.up"),
    (C::Palette, "ctrl-p", "cursor.up"),
    (C::Palette, "pagedown", "page.half_down"),
    (C::Palette, "pageup", "page.half_up"),
    (C::Help, "esc", "cancel"),
    (C::Help, "q", "cancel"),
    (C::Help, "?", "cancel"),
    (C::Help, "j", "cursor.down"),
    (C::Help, "down", "cursor.down"),
    (C::Help, "k", "cursor.up"),
    (C::Help, "up", "cursor.up"),
    (C::Help, "ctrl-d", "page.half_down"),
    (C::Help, "pagedown", "page.half_down"),
    (C::Help, "ctrl-u", "page.half_up"),
    (C::Help, "pageup", "page.half_up"),
    (C::Help, "g g", "cursor.top"),
    (C::Help, "home", "cursor.top"),
    (C::Help, "G", "cursor.bottom"),
    (C::Help, "end", "cursor.bottom"),
    (C::Help, "/", "filter.open"),
    (C::Confirm, "esc", "cancel"),
    (C::Confirm, "ctrl-c", "cancel"),
    (C::Confirm, "enter", "accept"),
    (C::Jobs, "esc", "cancel"),
    (C::Jobs, "q", "cancel"),
    (C::Jobs, "x", "job.cancel"),
    // Bookmark list.
    (C::Bookmarks, "esc", "cancel"),
    (C::Bookmarks, "ctrl-c", "cancel"),
    (C::Bookmarks, "q", "cancel"),
    (C::Bookmarks, "enter", "accept"),
    (C::Bookmarks, "j", "cursor.down"),
    (C::Bookmarks, "down", "cursor.down"),
    (C::Bookmarks, "k", "cursor.up"),
    (C::Bookmarks, "up", "cursor.up"),
    (C::Bookmarks, "d", "bookmark.delete"),
    (C::Bookmarks, "delete", "bookmark.delete"),
    // Frecency jump: every other key is text for the query.
    (C::Jump, "esc", "cancel"),
    (C::Jump, "ctrl-c", "cancel"),
    (C::Jump, "enter", "accept"),
    (C::Jump, "down", "cursor.down"),
    (C::Jump, "ctrl-n", "cursor.down"),
    (C::Jump, "ctrl-j", "cursor.down"),
    (C::Jump, "up", "cursor.up"),
    (C::Jump, "ctrl-p", "cursor.up"),
    (C::Jump, "ctrl-k", "cursor.up"),
    (C::Jobs, "c", "jobs.clear"),
    (C::Jobs, "j", "cursor.down"),
    (C::Jobs, "k", "cursor.up"),
    (C::Jobs, "down", "cursor.down"),
    (C::Jobs, "up", "cursor.up"),
    (C::Jobs, "g g", "cursor.top"),
    (C::Jobs, "G", "cursor.bottom"),
    (C::Conflict, "esc", "cancel"),
    (C::Conflict, "ctrl-c", "cancel"),
    (C::Conflict, "enter", "accept"),
    (C::Menu, "esc", "cancel"),
    (C::Menu, "q", "cancel"),
    (C::Menu, "ctrl-c", "cancel"),
    (C::Menu, "enter", "accept"),
    (C::Menu, "j", "cursor.down"),
    (C::Menu, "k", "cursor.up"),
    (C::Menu, "down", "cursor.down"),
    (C::Menu, "up", "cursor.up"),
    (C::Pager, "esc", "cancel"),
    (C::Pager, "q", "cancel"),
    (C::Pager, "ctrl-c", "job.cancel"),
    (C::Pager, "j", "cursor.down"),
    (C::Pager, "k", "cursor.up"),
    (C::Pager, "down", "cursor.down"),
    (C::Pager, "up", "cursor.up"),
    (C::Pager, "g g", "cursor.top"),
    (C::Pager, "G", "cursor.bottom"),
    (C::Pager, "ctrl-d", "page.half_down"),
    (C::Pager, "ctrl-u", "page.half_up"),
    (C::Pager, "pagedown", "page.half_down"),
    (C::Pager, "pageup", "page.half_up"),
    // Claude pane. While the pane has focus every key that is not bound here goes to
    // Claude verbatim, so only the keys below are ever intercepted.
    (C::Claude, "ctrl-\\", "claude.focus"),
    (C::Claude, "alt-=", "claude.grow"),
    (C::Claude, "alt--", "claude.shrink"),
    (C::Claude, "shift-pageup", "claude.scroll_up"),
    (C::Claude, "shift-pagedown", "claude.scroll_down"),
    // "Claude changes" view.
    (C::ClaudeChanges, "esc", "cancel"),
    (C::ClaudeChanges, "q", "cancel"),
    (C::ClaudeChanges, "ctrl-c", "cancel"),
    (C::ClaudeChanges, "enter", "accept"),
    (C::ClaudeChanges, "j", "cursor.down"),
    (C::ClaudeChanges, "down", "cursor.down"),
    (C::ClaudeChanges, "k", "cursor.up"),
    (C::ClaudeChanges, "up", "cursor.up"),
    (C::ClaudeChanges, "g g", "cursor.top"),
    (C::ClaudeChanges, "home", "cursor.top"),
    (C::ClaudeChanges, "G", "cursor.bottom"),
    (C::ClaudeChanges, "end", "cursor.bottom"),
    (C::ClaudeChanges, "X", "claude.changes_clear"),
    (C::ClaudeChanges, "d", "claude.changes_diff"),
];

impl Keymap {
    /// The built-in keymap.
    pub fn defaults() -> Self {
        // The table is a compile-time constant covered by `default_bindings_are_valid`.
        Self::from_specs(DEFAULT_BINDINGS).expect("built-in keymap is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    fn invoke(action: Action) -> Resolution {
        Resolution::Action(Invocation { action, arg: None })
    }

    #[test]
    fn default_bindings_are_valid() {
        let map = Keymap::from_specs(DEFAULT_BINDINGS).unwrap();
        assert!(!map.bindings(KeyContext::Normal).is_empty());
    }

    #[test]
    fn every_action_has_a_default_binding_except_the_palette_only_ones() {
        // Reachable through the command palette (phase 5) and bindable in config.toml.
        let palette_only = [
            Action::ConfigReload,
            Action::ThemeSwitch,
            Action::IconsSwitch,
            // Only meaningful inside the billing question, whose `y` is the confirm overlay's.
            Action::ClaudeProceed,
        ];
        let map = Keymap::defaults();
        for &action in Action::ALL {
            let bound = KeyContext::ALL
                .iter()
                .any(|&ctx| !map.keys_for(ctx, action).is_empty());
            assert!(
                bound || palette_only.contains(&action),
                "{action} has no default binding"
            );
        }
    }

    #[test]
    fn single_key_resolves_immediately() {
        let map = Keymap::defaults();
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("q")),
            invoke(Action::Quit)
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("ctrl-p")),
            invoke(Action::FindFile)
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("G")),
            invoke(Action::CursorBottom)
        );
    }

    #[test]
    fn prefix_keys_are_pending_then_resolve() {
        let map = Keymap::defaults();
        let g = chord("g");
        assert_eq!(map.resolve(KeyContext::Normal, &[], g), Resolution::Pending);
        assert_eq!(
            map.resolve(KeyContext::Normal, &[g], g),
            invoke(Action::CursorTop)
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[g], chord("h")),
            invoke(Action::GoHome)
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[g], chord("j")),
            invoke(Action::JobsOpen)
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[g], chord("z")),
            Resolution::NoMatch
        );
    }

    #[test]
    fn any_char_captures_the_argument() {
        let map = Keymap::defaults();
        let m = chord("m");
        assert_eq!(map.resolve(KeyContext::Normal, &[], m), Resolution::Pending);
        assert_eq!(
            map.resolve(KeyContext::Normal, &[m], chord("a")),
            Resolution::Action(Invocation {
                action: Action::BookmarkSet,
                arg: Some('a')
            })
        );
        // Modified keys are not captured.
        assert_eq!(
            map.resolve(KeyContext::Normal, &[m], chord("ctrl-a")),
            Resolution::NoMatch
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[chord("'")], chord("Z")),
            Resolution::Action(Invocation {
                action: Action::BookmarkJump,
                arg: Some('Z')
            })
        );
    }

    #[test]
    fn fixed_arguments_are_passed_through() {
        let map = Keymap::defaults();
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("3")),
            Resolution::Action(Invocation {
                action: Action::TabGoto,
                arg: Some('3')
            })
        );
    }

    #[test]
    fn visual_falls_back_to_normal_but_prefers_its_own() {
        let map = Keymap::defaults();
        assert_eq!(
            map.resolve(KeyContext::Visual, &[], chord("j")),
            invoke(Action::CursorDown)
        );
        assert_eq!(
            map.resolve(KeyContext::Visual, &[], chord("g")),
            Resolution::Pending
        );
        assert_eq!(
            map.resolve(KeyContext::Visual, &[chord("g")], chord("g")),
            invoke(Action::CursorTop)
        );
    }

    #[test]
    fn contexts_are_isolated() {
        let map = Keymap::defaults();
        // `j` is navigation in Normal but not bound in the Palette (it is text there).
        assert_eq!(
            map.resolve(KeyContext::Palette, &[], chord("j")),
            Resolution::NoMatch
        );
        assert_eq!(
            map.resolve(KeyContext::Help, &[], chord("esc")),
            invoke(Action::Cancel)
        );
    }

    #[test]
    fn continuations_list_next_keys() {
        let map = Keymap::defaults();
        let next = map.continuations(KeyContext::Normal, &[chord("g")]);
        assert!(next.contains(&Continuation {
            key: KeyPattern::Chord(chord("g")),
            target: ContinuationTarget::Action(Action::CursorTop),
        }));
        assert!(next.contains(&Continuation {
            key: KeyPattern::Chord(chord("j")),
            target: ContinuationTarget::Action(Action::JobsOpen),
        }));
        assert!(map
            .continuations(KeyContext::Normal, &[chord("q")])
            .is_empty());
    }

    #[test]
    fn keys_for_lists_every_binding() {
        let map = Keymap::defaults();
        assert_eq!(
            map.keys_for(KeyContext::Normal, Action::FindFile),
            vec!["ctrl-p", "f"]
        );
        assert_eq!(
            map.keys_for(KeyContext::Normal, Action::BookmarkSet),
            vec!["m <char>"]
        );
    }

    #[test]
    fn conflicting_defaults_are_rejected() {
        let err = Keymap::from_specs(&[
            (KeyContext::Normal, "g", "quit"),
            (KeyContext::Normal, "g g", "cursor.top"),
        ])
        .unwrap_err();
        assert!(matches!(err, KeymapError::Conflict { .. }));
        let err = Keymap::from_specs(&[
            (KeyContext::Normal, "m <char>", "bookmark.set"),
            (KeyContext::Normal, "m a", "quit"),
        ])
        .unwrap_err();
        assert!(matches!(err, KeymapError::Conflict { .. }));
        assert!(matches!(
            Keymap::from_specs(&[(KeyContext::Normal, "j", "bogus")]).unwrap_err(),
            KeymapError::Invalid { .. }
        ));
    }

    #[test]
    fn binding_parse_errors() {
        assert_eq!(Binding::parse("", "quit"), Err(BindingError::NoKeys));
        assert!(matches!(
            Binding::parse("x", "nope"),
            Err(BindingError::UnknownAction(_))
        ));
        assert!(matches!(
            Binding::parse("x", "tab.goto:12"),
            Err(BindingError::BadArg(_))
        ));
        assert_eq!(
            Binding::parse("m <char>", "bookmark.set:a"),
            Err(BindingError::AmbiguousArg)
        );
        assert!(matches!(
            Binding::parse("wat", "quit"),
            Err(BindingError::Key(_))
        ));
    }

    fn overrides(
        ctx: &str,
        entries: &[(&str, &str)],
    ) -> BTreeMap<String, BTreeMap<String, String>> {
        let table = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        BTreeMap::from([(ctx.to_owned(), table)])
    }

    #[test]
    fn overrides_rebind_unbind_and_replace_conflicting_defaults() {
        let mut map = Keymap::defaults();
        let issues = map.apply_overrides(&overrides(
            "normal",
            &[("q", "none"), ("Z Z", "quit"), ("g", "go.home")],
        ));
        // Shadowing the `g ...` family is legal but reported.
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("`g` shadows `g g`"), "{issues:?}");
        assert!(issues[0].contains("g /"), "{issues:?}");
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("q")),
            Resolution::NoMatch
        );
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("Z")),
            Resolution::Pending
        );
        // `g` alone replaced the whole `g ...` family.
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("g")),
            invoke(Action::GoHome)
        );
    }

    #[test]
    fn bad_overrides_are_reported_and_skipped() {
        let mut map = Keymap::defaults();
        let mut all = overrides("normal", &[("x", "no.such.action"), ("ctrl-q", "quit")]);
        all.extend(overrides("bogus", &[("a", "quit")]));
        let issues = map.apply_overrides(&all);
        assert_eq!(issues.len(), 2, "{issues:?}");
        // The valid line still applied.
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("ctrl-q")),
            invoke(Action::Quit)
        );
        // The invalid line did not clobber the default.
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("x")),
            invoke(Action::Cut)
        );
    }

    #[test]
    fn extending_a_single_key_binding_reports_the_shadowed_one() {
        let mut map = Keymap::defaults();
        let issues = map.apply_overrides(&overrides("normal", &[("q q", "quit")]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("`q q` shadows `q`"), "{issues:?}");
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("q")),
            Resolution::Pending
        );
    }

    #[test]
    fn plain_rebinding_of_the_same_keys_is_silent() {
        let mut map = Keymap::defaults();
        let issues = map.apply_overrides(&overrides("normal", &[("d", "file.delete")]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("d")),
            invoke(Action::DeletePermanent)
        );
    }

    #[test]
    fn unbinding_something_unbound_is_reported() {
        let mut map = Keymap::defaults();
        let issues = map.apply_overrides(&overrides("normal", &[("F5", "none"), ("j", "")]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("nothing is bound"), "{issues:?}");
        assert_eq!(
            map.resolve(KeyContext::Normal, &[], chord("j")),
            Resolution::NoMatch,
            "an empty target unbinds too"
        );
    }

    #[test]
    fn overlay_contexts_can_be_rebound() {
        let mut map = Keymap::defaults();
        let issues = map.apply_overrides(&overrides("palette", &[("ctrl-j", "cursor.down")]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            map.resolve(KeyContext::Palette, &[], chord("ctrl-j")),
            invoke(Action::CursorDown)
        );
    }

    #[test]
    fn conflicts_between_user_bindings_are_reported() {
        let mut map = Keymap::defaults();
        let issues =
            map.apply_overrides(&overrides("normal", &[("Z", "quit"), ("Z Z", "quit.cd")]));
        assert_eq!(issues.len(), 1, "{issues:?}");
    }
}
