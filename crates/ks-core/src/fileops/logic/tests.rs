use std::path::PathBuf;
use std::time::SystemTime;

use super::*;
use crate::config::Config;
use crate::entry::Entry;
use crate::fileops::BulkRetry;
use crate::fileops::{
    BulkPlanResult, ConflictInfo, ForegroundSpec, ItemMeta, JobDone, JobProgress,
};
use crate::generation::Gen;
use crate::key::KeyChord;
use crate::keymap::Invocation;
use crate::keymap::Keymap;
use crate::msg::Msg;
use crate::msg::TimerKind;
use crate::state::{Places, StartParams};
use crate::update::update;

fn state() -> AppState {
    let mut state = AppState::new(StartParams {
        cwd: PathBuf::from("/work/proj"),
        home: Some(PathBuf::from("/work")),
        places: Places::default(),
        config: Config::default(),
        keymap: Keymap::defaults(),
        size: Size::new(80, 24),
        now: SystemTime::UNIX_EPOCH,
        utc_offset_secs: 0,
    });
    let rows = state.viewport_height();
    state.view.push_chunk(
        vec![
            Entry::dir("src"),
            Entry::file("a.txt", 10),
            Entry::file("b.tar.gz", 20),
            Entry::file(".env", 1),
            Entry::file("c.rs", 30),
        ],
        rows,
    );
    state.view.finish_loading();
    // These tests assert exact effect lists: keep the preview pane (and its requests) out.
    state.preview.user_visible = Some(false);
    state.dirty = false;
    state
}

/// Plays keys (space separated) and returns every effect produced.
fn keys(state: &mut AppState, text: &str) -> Vec<Effect> {
    let mut all = Vec::new();
    for part in text.split(' ') {
        let chord = if part == "space" {
            KeyChord::char(' ')
        } else {
            part.parse::<KeyChord>().unwrap()
        };
        all.extend(update(state, Msg::Key(chord)));
    }
    all
}

/// Types literal text (every char is a key).
fn typed(state: &mut AppState, text: &str) {
    for c in text.chars() {
        update(state, Msg::Key(KeyChord::char(c)));
    }
}

fn cursor_to(state: &mut AppState, name: &str) {
    let rows = state.viewport_height();
    let target = (0..state.view.len())
        .find(|&i| state.view.row(i).unwrap().display_name() == name)
        .unwrap();
    let delta = target as isize - state.view.cursor.index as isize;
    state.view.move_cursor(delta, rows);
}

fn p(name: &str) -> PathBuf {
    PathBuf::from("/work/proj").join(name)
}

fn status(state: &AppState) -> String {
    state
        .status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default()
}

fn submits(effects: &[Effect]) -> Vec<(&JobRequest, bool)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::SubmitJob { request, keep_both } => Some((request, *keep_both)),
            _ => None,
        })
        .collect()
}

// ----- yank and paste -----

#[test]
fn yank_uses_marks_else_the_cursor_and_reports_the_count() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "y");
    assert_eq!(
        s.fileops.register,
        Some(Register {
            paths: vec![p("a.txt")],
            cut: false
        })
    );
    assert_eq!(status(&s), "1 yanked");

    cursor_to(&mut s, "a.txt");
    keys(&mut s, "space space x");
    // Marks are taken in list order, then cleared.
    assert_eq!(
        s.fileops.register,
        Some(Register {
            paths: vec![p("a.txt"), p("b.tar.gz")],
            cut: true
        })
    );
    assert_eq!(status(&s), "2 cut");
    assert_eq!(s.view.marks_count(), 0);
}

#[test]
fn paste_copies_into_the_current_directory() {
    let mut s = state();
    s.fileops.register = Some(Register {
        paths: vec![
            PathBuf::from("/elsewhere/x.txt"),
            PathBuf::from("/elsewhere/d"),
        ],
        cut: false,
    });
    let effects = keys(&mut s, "p");
    assert_eq!(
        submits(&effects),
        vec![(
            &JobRequest::Copy {
                items: vec![
                    (PathBuf::from("/elsewhere/x.txt"), p("x.txt")),
                    (PathBuf::from("/elsewhere/d"), p("d")),
                ]
            },
            false
        )]
    );
    assert!(s.fileops.register.is_some(), "a copy can be pasted again");
}

#[test]
fn pasting_a_copy_next_to_its_source_keeps_both() {
    let mut s = state();
    s.fileops.register = Some(Register {
        paths: vec![p("a.txt"), PathBuf::from("/other/z.txt")],
        cut: false,
    });
    let effects = keys(&mut s, "p");
    let jobs = submits(&effects);
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|(r, keep)| *keep
        && matches!(r, JobRequest::Copy { items } if items == &vec![(p("a.txt"), p("a.txt"))])));
    assert!(jobs.iter().any(|(_, keep)| !*keep));
}

#[test]
fn paste_of_a_cut_moves_and_empties_the_register() {
    let mut s = state();
    s.fileops.register = Some(Register {
        paths: vec![PathBuf::from("/elsewhere/x.txt")],
        cut: true,
    });
    let effects = keys(&mut s, "p");
    assert!(matches!(
        submits(&effects).as_slice(),
        [(JobRequest::Move { .. }, false)]
    ));
    assert!(s.fileops.register.is_some(), "kept until the move succeeds");
    update(
        &mut s,
        Msg::Job(JobMsg::Finished(done(
            1,
            OpKind::Move,
            JobStatus::Cancelled,
        ))),
    );
    assert!(
        s.fileops.register.is_some(),
        "a cancelled move keeps the cut"
    );
    let mut finished = done(2, OpKind::Move, JobStatus::Done);
    finished.removed = vec![PathBuf::from("/elsewhere/x.txt")];
    update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    assert!(s.fileops.register.is_none());

    // Cutting something from this very directory and pasting it here does nothing.
    s.fileops.register = Some(Register {
        paths: vec![p("a.txt")],
        cut: true,
    });
    let effects = keys(&mut s, "p");
    assert!(submits(&effects).is_empty());
    assert_eq!(status(&s), "Already in this directory");
}

#[test]
fn paste_without_a_register_explains_itself() {
    let mut s = state();
    let effects = keys(&mut s, "p");
    assert!(submits(&effects).is_empty());
    assert!(status(&s).starts_with("Nothing to paste"));
}

// ----- trash, delete, undo -----

#[test]
fn trash_takes_the_marked_entries_without_asking() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "space space");
    let effects = keys(&mut s, "d");
    assert_eq!(
        submits(&effects),
        vec![(
            &JobRequest::Trash {
                paths: vec![p("a.txt"), p("b.tar.gz")]
            },
            false
        )]
    );
    assert!(s.overlays.is_empty(), "trash is not a modal question");
    assert_eq!(s.view.marks_count(), 0);
}

#[test]
fn trash_without_marks_uses_the_cursor_entry() {
    let mut s = state();
    cursor_to(&mut s, "c.rs");
    let effects = keys(&mut s, "d");
    assert_eq!(
        submits(&effects),
        vec![(
            &JobRequest::Trash {
                paths: vec![p("c.rs")]
            },
            false
        )]
    );
}

#[test]
fn extract_takes_the_marked_archives_and_skips_other_files() {
    let mut s = state();
    let rows = s.viewport_height();
    s.view.push_chunk(
        vec![Entry::file("pics.zip", 5), Entry::file("movie.RAR", 9)],
        rows,
    );
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "space");
    // Sorted by name: movie.RAR, then pics.zip.
    cursor_to(&mut s, "movie.RAR");
    keys(&mut s, "space space");
    let effects = keys(&mut s, "X");
    assert_eq!(
        submits(&effects),
        vec![(
            &JobRequest::Extract {
                archives: vec![p("movie.RAR"), p("pics.zip")],
                dest_dir: p(""),
            },
            false
        )]
    );
    assert_eq!(s.view.marks_count(), 0);
}

#[test]
fn extract_on_a_non_archive_explains_itself() {
    let mut s = state();
    cursor_to(&mut s, "c.rs");
    let effects = keys(&mut s, "X");
    assert!(submits(&effects).is_empty());
    assert_eq!(status(&s), "Not a ZIP or RAR archive");
}

#[test]
fn permanent_delete_needs_the_typed_word() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "D");
    assert_eq!(s.overlays, vec![Overlay::Confirm]);
    let confirm = s.fileops.confirm.as_ref().unwrap();
    assert_eq!(confirm.token.as_deref(), Some("yes"));
    assert!(confirm.danger);

    // Enter with nothing typed does not delete.
    let effects = keys(&mut s, "enter");
    assert!(submits(&effects).is_empty());
    assert!(s.fileops.confirm.as_ref().unwrap().mismatch);

    // Letters that are bound in the list (`q`, `d`) are text here.
    typed(&mut s, "no");
    assert!(keys(&mut s, "enter").is_empty());
    keys(&mut s, "ctrl-u");
    typed(&mut s, "YES");
    let effects = keys(&mut s, "enter");
    assert_eq!(
        submits(&effects),
        vec![(
            &JobRequest::DeletePermanent {
                paths: vec![p("a.txt")]
            },
            false
        )]
    );
    assert!(s.overlays.is_empty());
    assert!(s.fileops.confirm.is_none());
}

#[test]
fn multi_item_delete_asks_for_the_count() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "space space space D");
    assert_eq!(
        s.fileops.confirm.as_ref().unwrap().token.as_deref(),
        Some("3")
    );
    typed(&mut s, "yes");
    assert!(keys(&mut s, "enter").is_empty(), "yes is not the count");
    keys(&mut s, "ctrl-u");
    typed(&mut s, "3");
    let effects = keys(&mut s, "enter");
    assert!(matches!(
        submits(&effects).as_slice(),
        [(JobRequest::DeletePermanent { paths }, false)] if paths.len() == 3
    ));
}

#[test]
fn escape_cancels_the_delete_confirmation() {
    let mut s = state();
    keys(&mut s, "D");
    typed(&mut s, "yes");
    let effects = keys(&mut s, "esc");
    assert!(submits(&effects).is_empty());
    assert!(s.overlays.is_empty());
    assert!(s.fileops.confirm.is_none());
}

#[test]
fn undo_asks_the_runner() {
    let mut s = state();
    assert_eq!(keys(&mut s, "u"), vec![Effect::Undo]);
}

// ----- rename and create -----

#[test]
fn rename_prompt_is_prefilled_with_the_cursor_before_the_extension() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "r");
    assert_eq!(s.mode, Mode::Prompt(PromptKind::Rename));
    let prompt = s.fileops.prompt.as_ref().unwrap();
    assert_eq!(prompt.input.text(), "a.txt");
    assert_eq!(prompt.input.cursor(), 1);

    // Multi-dot names split at the last dot; dotfiles and directories keep the end.
    let rows = s.viewport_height();
    s.view.toggle_hidden(rows); // `.env` must be shown to be renamed
    for (name, cursor) in [("b.tar.gz", 5), (".env", 4), ("src", 3)] {
        keys(&mut s, "esc");
        cursor_to(&mut s, name);
        keys(&mut s, "r");
        assert_eq!(
            s.fileops.prompt.as_ref().unwrap().input.cursor(),
            cursor,
            "{name}"
        );
    }
}

#[test]
fn rename_submits_the_edited_name() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "r");
    typed(&mut s, "bc");
    let effects = keys(&mut s, "enter");
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Rename { .. }))
            .collect::<Vec<_>>(),
        vec![&Effect::Rename {
            from: p("a.txt"),
            new_name: "abc.txt".into()
        }]
    );
    assert_eq!(s.mode, Mode::Normal);
    assert!(s.fileops.prompt.is_none());
}

#[test]
fn typing_in_a_prompt_never_triggers_actions() {
    let mut s = state();
    keys(&mut s, "a");
    typed(&mut s, "qdDx");
    assert_eq!(s.quit, None);
    assert_eq!(s.fileops.prompt.as_ref().unwrap().input.text(), "qdDx");
    keys(&mut s, "ctrl-u");
    assert_eq!(s.fileops.prompt.as_ref().unwrap().input.text(), "");
}

#[test]
fn unchanged_or_empty_rename_submits_nothing() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "r");
    let effects = keys(&mut s, "enter");
    assert!(!effects.iter().any(|e| matches!(e, Effect::Rename { .. })));
    assert_eq!(status(&s), "Name unchanged");

    keys(&mut s, "r ctrl-a ctrl-k");
    let effects = keys(&mut s, "enter");
    assert!(!effects.iter().any(|e| matches!(e, Effect::Rename { .. })));
    assert_eq!(status(&s), "A name cannot be empty");
}

#[test]
fn escape_leaves_the_prompt_without_acting() {
    let mut s = state();
    keys(&mut s, "r");
    typed(&mut s, "zzz");
    let effects = keys(&mut s, "esc");
    assert!(!effects.iter().any(|e| matches!(e, Effect::Rename { .. })));
    assert_eq!(s.mode, Mode::Normal);
    assert!(s.fileops.prompt.is_none());
}

#[test]
fn create_prompt_passes_the_raw_input() {
    let mut s = state();
    keys(&mut s, "a");
    assert_eq!(s.mode, Mode::Prompt(PromptKind::Create));
    typed(&mut s, "sub/new/");
    let effects = keys(&mut s, "enter");
    assert!(effects.contains(&Effect::Create {
        base: PathBuf::from("/work/proj"),
        input: "sub/new/".into()
    }));
    assert_eq!(s.mode, Mode::Normal);
}

// ----- conflicts -----

fn conflict(job: u64) -> Msg {
    let meta = |len| ItemMeta {
        is_dir: false,
        is_link: false,
        len,
        modified: None,
    };
    Msg::Job(JobMsg::Conflict(ConflictInfo {
        job: JobId(job),
        src: PathBuf::from("/a/report.final.txt"),
        dst: p("report.final.txt"),
        src_meta: meta(10),
        dst_meta: meta(20),
    }))
}

/// Lets the grace timer of the front conflict dialog fire.
fn arm(state: &mut AppState) {
    let gen = state.fileops.arm_gen.current();
    update(
        state,
        Msg::Timer {
            kind: TimerKind::ConflictArm,
            gen,
        },
    );
}

/// A conflict arrives and its grace period passes.
fn show_conflict(state: &mut AppState, job: u64) {
    update(state, conflict(job));
    arm(state);
}

fn started(job: u64) -> Msg {
    Msg::Job(JobMsg::Started {
        id: JobId(job),
        kind: OpKind::Copy,
        description: "Copying 2 items".into(),
    })
}

fn resolves(effects: &[Effect]) -> Vec<(JobId, ConflictAnswer, bool)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::ResolveConflict {
                job,
                answer,
                apply_to_all,
            } => Some((*job, answer.clone(), *apply_to_all)),
            _ => None,
        })
        .collect()
}

#[test]
fn conflict_dialog_answers_with_single_keys() {
    for (key, expected) in [
        ("O", ConflictAnswer::Overwrite),
        ("s", ConflictAnswer::Skip),
        ("k", ConflictAnswer::KeepBoth),
        ("c", ConflictAnswer::Cancel),
        ("enter", ConflictAnswer::KeepBoth),
        ("esc", ConflictAnswer::Cancel),
    ] {
        let mut s = state();
        update(&mut s, started(1));
        show_conflict(&mut s, 1);
        assert_eq!(s.overlays, vec![Overlay::Conflict]);
        assert_eq!(s.fileops.jobs[0].status, JobStatus::AwaitingConflict);
        let effects = keys(&mut s, key);
        assert_eq!(
            resolves(&effects),
            vec![(JobId(1), expected, false)],
            "{key}"
        );
        assert!(s.overlays.is_empty(), "{key}");
        assert_eq!(s.fileops.jobs[0].status, JobStatus::Running);
    }
}

#[test]
fn apply_to_all_is_a_toggle_that_rides_along() {
    let mut s = state();
    update(&mut s, started(1));
    show_conflict(&mut s, 1);
    keys(&mut s, "a");
    assert!(s.fileops.conflicts[0].apply_to_all);
    keys(&mut s, "a");
    assert!(!s.fileops.conflicts[0].apply_to_all);
    keys(&mut s, "a");
    let effects = keys(&mut s, "s");
    assert_eq!(
        resolves(&effects),
        vec![(JobId(1), ConflictAnswer::Skip, true)]
    );
}

#[test]
fn conflict_rename_uses_an_inline_prompt() {
    let mut s = state();
    update(&mut s, started(1));
    show_conflict(&mut s, 1);
    keys(&mut s, "r");
    let input = s.fileops.conflicts[0].rename.as_ref().unwrap();
    assert_eq!(input.text(), "report.final.txt");
    assert_eq!(input.cursor(), "report.final".len());

    // Keys are text now, `o` does not overwrite.
    typed(&mut s, "2o");
    assert!(resolves(&keys(&mut s, "ctrl-a")).is_empty());

    // Escape only leaves the inline prompt; the dialog stays.
    assert!(resolves(&keys(&mut s, "esc")).is_empty());
    assert!(s.fileops.conflicts[0].rename.is_none());
    assert_eq!(s.overlays, vec![Overlay::Conflict]);

    keys(&mut s, "r");
    typed(&mut s, "_new");
    let effects = keys(&mut s, "enter");
    assert_eq!(
        resolves(&effects),
        vec![(
            JobId(1),
            ConflictAnswer::Rename("report.final_new.txt".into()),
            false
        )]
    );
    assert!(s.overlays.is_empty());
}

#[test]
fn conflicts_queue_and_finished_jobs_drop_their_dialog() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, started(2));
    show_conflict(&mut s, 1);
    show_conflict(&mut s, 2);
    assert_eq!(s.fileops.conflicts.len(), 2);
    assert_eq!(s.overlays, vec![Overlay::Conflict]);

    let effects = keys(&mut s, "O");
    assert_eq!(resolves(&effects)[0].0, JobId(1));
    assert_eq!(s.overlays, vec![Overlay::Conflict], "the next one is shown");
    assert!(
        !s.fileops.conflicts[0].armed,
        "the next one gets its own grace period"
    );
    arm(&mut s);
    assert_eq!(s.fileops.conflicts[0].info.job, JobId(2));

    // Job 2 is cancelled from the Jobs overlay meanwhile: its question is moot.
    update(
        &mut s,
        Msg::Job(JobMsg::Finished(done(
            2,
            OpKind::Copy,
            JobStatus::Cancelled,
        ))),
    );
    assert!(s.fileops.conflicts.is_empty());
    assert!(s.overlays.is_empty());
}

// ----- jobs -----

fn done(id: u64, kind: OpKind, status: JobStatus) -> JobDone {
    JobDone {
        id: JobId(id),
        kind,
        status,
        items_done: 1,
        items_skipped: 0,
        bytes_done: 0,
        errors: Vec::new(),
        created: Vec::new(),
        removed: Vec::new(),
        consumed_sources: Vec::new(),
        is_undo: false,
        elapsed_ms: 5,
    }
}

#[test]
fn job_messages_drive_the_table_and_the_spinner_set() {
    let mut s = state();
    update(
        &mut s,
        Msg::Job(JobMsg::Queued {
            id: JobId(7),
            kind: OpKind::Copy,
            description: "Copying big.iso".into(),
        }),
    );
    assert!(s.has_running_jobs());
    assert_eq!(s.fileops.jobs[0].status, JobStatus::Queued);

    update(&mut s, started(7));
    update(
        &mut s,
        Msg::Job(JobMsg::Progress {
            id: JobId(7),
            progress: JobProgress {
                bytes_done: 50,
                bytes_total: 200,
                ..JobProgress::default()
            },
        }),
    );
    assert_eq!(s.fileops.jobs[0].progress.percent(), Some(25));
    assert_eq!(s.fileops.jobs[0].status, JobStatus::Running);

    update(
        &mut s,
        Msg::Job(JobMsg::Finished(done(7, OpKind::Copy, JobStatus::Done))),
    );
    assert!(!s.has_running_jobs());
    assert_eq!(s.fileops.jobs[0].status, JobStatus::Done);
    assert!(s.fileops.jobs[0].summary.is_some());
    assert!(status(&s).starts_with("Copied"), "{}", status(&s));
}

#[test]
fn a_finished_job_refreshes_the_directory_it_touched_and_reveals_the_new_item() {
    let mut s = state();
    let mut report = done(1, OpKind::Create, JobStatus::Done);
    report.created = vec![p("new.txt")];
    let effects = update(&mut s, Msg::Job(JobMsg::Finished(report)));
    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::ListDir { path, .. } if path == &PathBuf::from("/work/proj"))
        ),
        "{effects:?}"
    );
    assert_eq!(status(&s), "Created new.txt · u to undo");

    // A job elsewhere does not re-list this directory.
    let mut report = done(2, OpKind::Copy, JobStatus::Done);
    report.created = vec![PathBuf::from("/elsewhere/x")];
    let effects = update(&mut s, Msg::Job(JobMsg::Finished(report)));
    assert!(!effects.iter().any(|e| matches!(e, Effect::ListDir { .. })));
}

#[test]
fn trash_result_mentions_undo_and_permanent_delete_does_not() {
    let (_, trash) = job_summary(&done(1, OpKind::Trash, JobStatus::Done));
    assert!(
        trash.contains("Recycle Bin") && trash.ends_with("u to undo"),
        "{trash}"
    );
    let (_, delete) = job_summary(&done(1, OpKind::Delete, JobStatus::Done));
    assert!(!delete.contains("undo"), "{delete}");
}

#[test]
fn failures_and_cancellations_are_reported_with_a_level() {
    let mut failed = done(1, OpKind::Copy, JobStatus::Failed);
    failed.errors = vec!["x.txt: Access is denied.".into()];
    failed.items_done = 0;
    let (level, text) = job_summary(&failed);
    assert_eq!(level, Level::Error);
    assert!(
        text.contains("1 failed") && text.contains("Access is denied"),
        "{text}"
    );

    let (level, text) = job_summary(&done(1, OpKind::Copy, JobStatus::Cancelled));
    assert_eq!(level, Level::Warning);
    assert!(text.contains("cancelled"), "{text}");
}

#[test]
fn jobs_overlay_lists_selects_and_cancels() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, started(2));
    update(
        &mut s,
        Msg::Job(JobMsg::Finished(done(1, OpKind::Copy, JobStatus::Done))),
    );
    keys(&mut s, "g j");
    assert_eq!(s.overlays, vec![Overlay::Jobs]);
    // Newest first: job 2 (running), then job 1 (done).
    assert_eq!(s.fileops.jobs[0].id, JobId(2));
    let effects = keys(&mut s, "x");
    assert!(effects.contains(&Effect::CancelJob(JobId(2))));

    keys(&mut s, "j");
    assert_eq!(s.fileops.jobs_selected, 1);
    let effects = keys(&mut s, "x");
    assert!(!effects.iter().any(|e| matches!(e, Effect::CancelJob(_))));
    assert!(status(&s).contains("already finished"));
    keys(&mut s, "k");
    assert_eq!(s.fileops.jobs_selected, 0);

    keys(&mut s, "esc");
    assert!(s.overlays.is_empty());
}

// ----- menus, open, editor -----

#[test]
fn copy_path_menu_offers_three_formats() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "Y");
    assert_eq!(s.overlays, vec![Overlay::Menu]);
    assert_eq!(s.fileops.menu.as_ref().unwrap().items.len(), 3);

    // Enter takes the highlighted entry (absolute) ...
    let effects = keys(&mut s, "enter");
    assert_eq!(
        effects,
        vec![Effect::CopyPaths {
            paths: vec![p("a.txt")],
            format: PathFormatKind::Absolute,
            cwd: PathBuf::from("/work/proj")
        }]
    );
    assert!(s.overlays.is_empty());

    // ... the hot keys pick directly.
    keys(&mut s, "Y");
    let effects = keys(&mut s, "n");
    assert!(matches!(
        effects.as_slice(),
        [Effect::CopyPaths {
            format: PathFormatKind::Name,
            ..
        }]
    ));
    keys(&mut s, "Y j j enter");
    assert!(s.overlays.is_empty());
    keys(&mut s, "Y esc");
    assert!(s.overlays.is_empty() && s.fileops.menu.is_none());
}

#[test]
fn open_with_lists_handlers_from_the_runner() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    let effects = keys(&mut s, "O");
    assert_eq!(
        effects,
        vec![Effect::ListOpenHandlers {
            paths: vec![p("a.txt")]
        }]
    );
    update(
        &mut s,
        Msg::OpenHandlers {
            paths: vec![p("a.txt")],
            names: vec!["Notepad".into(), "VS Code".into()],
        },
    );
    assert_eq!(s.overlays, vec![Overlay::Menu]);
    let effects = keys(&mut s, "2");
    assert_eq!(
        effects,
        vec![Effect::OpenWith {
            paths: vec![p("a.txt")],
            handler: "VS Code".into()
        }]
    );
}

#[test]
fn open_and_edit_emit_the_shared_launch_effects() {
    let mut s = state();
    cursor_to(&mut s, "c.rs");
    assert_eq!(
        keys(&mut s, "o"),
        vec![Effect::OpenDefault { path: p("c.rs") }]
    );
    assert_eq!(
        keys(&mut s, "e"),
        vec![Effect::OpenInEditor {
            path: p("c.rs"),
            line: None
        }]
    );
    // Enter on a file opens it with the default app.
    assert_eq!(
        keys(&mut s, "l"),
        vec![Effect::OpenDefault { path: p("c.rs") }]
    );
}

// ----- shell and pager -----

#[test]
fn shell_command_opens_a_pager_that_streams() {
    let mut s = state();
    keys(&mut s, "!");
    assert_eq!(s.mode, Mode::Prompt(PromptKind::Shell));
    typed(&mut s, "git status");
    let effects = keys(&mut s, "enter");
    assert_eq!(
        effects,
        vec![Effect::RunShell {
            id: 0,
            command: "git status".into(),
            cwd: PathBuf::from("/work/proj")
        }]
    );
    assert_eq!(s.overlays, vec![Overlay::Pager]);
    assert_eq!(s.mode, Mode::Normal);

    for i in 0..100 {
        update(
            &mut s,
            Msg::Shell(ShellMsg::Line {
                id: 0,
                text: format!("line {i}"),
                stderr: i % 10 == 0,
            }),
        );
    }
    // A line of some other run is ignored.
    update(
        &mut s,
        Msg::Shell(ShellMsg::Line {
            id: 9,
            text: "stale".into(),
            stderr: false,
        }),
    );
    let pager = s.fileops.pager.as_ref().unwrap();
    assert_eq!(pager.lines.len(), 100);
    assert!(pager.follow);
    assert_eq!(pager.scroll, 100 - pager_rows(s.size), "follows the output");

    // Scrolling up leaves follow mode; `G` returns to it.
    keys(&mut s, "k");
    assert!(!s.fileops.pager.as_ref().unwrap().follow);
    keys(&mut s, "g g");
    assert_eq!(s.fileops.pager.as_ref().unwrap().scroll, 0);
    keys(&mut s, "G");
    assert!(s.fileops.pager.as_ref().unwrap().follow);

    update(
        &mut s,
        Msg::Shell(ShellMsg::Exited {
            id: 0,
            code: Some(0),
        }),
    );
    assert_eq!(
        s.fileops.pager.as_ref().unwrap().status,
        PagerStatus::Exited(Some(0))
    );
}

#[test]
fn ctrl_c_cancels_the_process_and_escape_closes_the_pager() {
    let mut s = state();
    keys(&mut s, "!");
    typed(&mut s, "sleep 99");
    keys(&mut s, "enter");
    let effects = keys(&mut s, "ctrl-c");
    assert_eq!(effects, vec![Effect::CancelShell(0)]);
    assert_eq!(
        s.overlays,
        vec![Overlay::Pager],
        "ctrl-c leaves the output open"
    );
    assert_eq!(s.quit, None);

    // Closing a pager whose command still runs kills it, too.
    let effects = keys(&mut s, "esc");
    assert_eq!(effects, vec![Effect::CancelShell(0)]);
    assert!(s.overlays.is_empty());
    assert!(s.fileops.pager.is_none());

    // A finished command needs no kill.
    keys(&mut s, "!");
    typed(&mut s, "echo");
    keys(&mut s, "enter");
    update(
        &mut s,
        Msg::Shell(ShellMsg::Exited {
            id: 1,
            code: Some(1),
        }),
    );
    assert!(keys(&mut s, "q").is_empty());
}

#[test]
fn old_lines_are_dropped_past_the_cap() {
    let mut s = state();
    keys(&mut s, "!");
    typed(&mut s, "x");
    keys(&mut s, "enter");
    for i in 0..(PAGER_MAX_LINES + 5) {
        update(
            &mut s,
            Msg::Shell(ShellMsg::Line {
                id: 0,
                text: i.to_string(),
                stderr: false,
            }),
        );
    }
    let pager = s.fileops.pager.as_ref().unwrap();
    assert_eq!(pager.lines.len(), PAGER_MAX_LINES);
    assert_eq!(pager.dropped, 5);
    assert_eq!(pager.lines[0].text, "5");
}

// ----- foreground and bulk rename -----

#[test]
fn bulk_rename_collects_marked_names_or_everything() {
    let mut s = state();
    let effects = keys(&mut s, "R");
    assert_eq!(
        effects,
        vec![Effect::BulkRenameEdit {
            dir: PathBuf::from("/work/proj"),
            names: vec![
                "src".into(),
                "a.txt".into(),
                "b.tar.gz".into(),
                "c.rs".into()
            ],
            reuse: None,
        }],
        "hidden .env is not shown and so not renamed"
    );

    cursor_to(&mut s, "a.txt");
    keys(&mut s, "space space");
    let effects = keys(&mut s, "R");
    assert!(matches!(
        effects.as_slice(),
        [Effect::BulkRenameEdit { names, .. }] if names == &["a.txt", "b.tar.gz"]
    ));
}

#[test]
fn bulk_rename_flows_through_suspend_preview_and_confirm() {
    let mut s = state();
    let file = PathBuf::from("/tmp/ks-bulk.txt");
    let effects = update(
        &mut s,
        Msg::Foreground(ForegroundRequest {
            spec: ForegroundSpec {
                program: "nvim".into(),
                args: vec![file.display().to_string()],
            },
            purpose: ForegroundPurpose::BulkEdit {
                dir: PathBuf::from("/work/proj"),
                names: vec!["a.txt".into()],
                file: file.clone(),
            },
        }),
    );
    let Some(Effect::RunForeground { id, .. }) = effects.first() else {
        panic!("{effects:?}");
    };
    let id = *id;

    // A stale foreground result is ignored.
    assert!(update(
        &mut s,
        Msg::ForegroundDone {
            id: id + 5,
            error: None
        }
    )
    .is_empty());

    let effects = update(&mut s, Msg::ForegroundDone { id, error: None });
    assert_eq!(
        effects,
        vec![Effect::BulkRenamePlan {
            dir: PathBuf::from("/work/proj"),
            names: vec!["a.txt".into()],
            file,
            error: None
        }]
    );

    let steps = vec![(p("a.txt"), p("z.txt"))];
    update(
        &mut s,
        Msg::BulkPlanned(BulkPlanResult::Planned {
            preview: vec!["a.txt -> z.txt".into()],
            steps: steps.clone(),
        }),
    );
    assert_eq!(s.overlays, vec![Overlay::Confirm]);
    let confirm = s.fileops.confirm.as_ref().unwrap();
    assert_eq!(confirm.lines, vec!["a.txt -> z.txt"]);
    assert!(confirm.token.is_none());

    let effects = keys(&mut s, "enter");
    assert_eq!(
        submits(&effects),
        vec![(&JobRequest::BulkRename { steps }, false)]
    );
    assert!(s.overlays.is_empty());
}

#[test]
fn invalid_bulk_rename_shows_problems_and_cannot_be_confirmed() {
    let mut s = state();
    update(
        &mut s,
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 2: duplicate name `x`".into()],
            retry: None,
        }),
    );
    let effects = keys(&mut s, "enter");
    assert!(submits(&effects).is_empty());
    assert!(s.overlays.is_empty());

    update(&mut s, Msg::BulkPlanned(BulkPlanResult::Unchanged));
    assert_eq!(status(&s), "No names changed");
}

#[test]
fn editor_foreground_refreshes_afterwards_and_reports_failures() {
    let mut s = state();
    let effects = update(
        &mut s,
        Msg::Foreground(ForegroundRequest {
            spec: ForegroundSpec {
                program: "nvim".into(),
                args: vec![],
            },
            purpose: ForegroundPurpose::Editor,
        }),
    );
    let Some(Effect::RunForeground { id, .. }) = effects.first() else {
        panic!();
    };
    let effects = update(
        &mut s,
        Msg::ForegroundDone {
            id: *id,
            error: Some("boom".into()),
        },
    );
    assert!(effects.iter().any(|e| matches!(e, Effect::ListDir { .. })));
    assert!(status(&s).contains("boom"));
}

#[test]
fn file_actions_do_nothing_in_the_drives_view() {
    let mut s = state();
    s.show_drives = true;
    for k in ["y", "d", "D", "r", "R", "a", "!", "Y"] {
        let effects = keys(&mut s, k);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SubmitJob { .. } | Effect::BulkRenameEdit { .. })),
            "{k}"
        );
        assert!(s.overlays.is_empty(), "{k}");
        assert!(s.fileops.prompt.is_none(), "{k}");
        assert_eq!(s.mode, Mode::Normal, "{k}");
    }
}

#[test]
fn job_cancel_works_without_the_overlay_for_the_palette() {
    let mut s = state();
    let mut effects = Vec::new();
    let cancel = Invocation {
        action: Action::JobCancel,
        arg: None,
    };
    assert!(handle_action(&mut s, &cancel, &mut effects));
    assert!(!effects.iter().any(|e| matches!(e, Effect::CancelJob(_))));
    assert_eq!(status(&s), "No job is running");

    update(&mut s, started(4));
    effects.clear();
    assert!(handle_action(&mut s, &cancel, &mut effects));
    assert!(effects.contains(&Effect::CancelJob(JobId(4))));
}

#[test]
fn opening_a_prompt_ends_visual_mode() {
    let mut s = state();
    keys(&mut s, "v j");
    assert_eq!(s.mode, Mode::Visual);
    assert_eq!(s.view.marks_count(), 2);
    keys(&mut s, "r");
    assert_eq!(s.mode, Mode::Prompt(PromptKind::Rename));
    keys(&mut s, "esc");
    // Moving now must not stretch the old range.
    keys(&mut s, "j");
    assert_eq!(s.view.marks_count(), 2);
}

#[test]
fn open_and_edit_act_on_every_mark() {
    let mut s = state();
    keys(&mut s, "v j");
    assert_eq!(s.view.marks_count(), 2);
    let effects = keys(&mut s, "e");
    let n = effects
        .iter()
        .filter(|e| matches!(e, Effect::OpenInEditor { .. }))
        .count();
    assert_eq!(n, 1, "the directory is skipped");
    assert_eq!(s.view.marks_count(), 0);
    let mut s = state();
    keys(&mut s, "v j");
    let n = keys(&mut s, "o")
        .iter()
        .filter(|e| matches!(e, Effect::OpenDefault { .. }))
        .count();
    assert_eq!(n, 2, "open handles directories too");
}

#[test]
fn open_refuses_a_flood_of_marks() {
    let mut s = state();
    let rows: Vec<Entry> = (0..20)
        .map(|i| Entry::file(format!("f{i}.txt"), 1))
        .collect();
    s.view.push_chunk(rows, 24);
    keys(&mut s, "v G");
    assert!(s.view.marks_count() > MAX_LAUNCH);
    for key in ["o", "e"] {
        let effects = keys(&mut s, key);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::OpenDefault { .. } | Effect::OpenInEditor { .. })),
            "{key} must not launch"
        );
    }
    assert!(s.view.marks_count() > MAX_LAUNCH, "marks are kept");
}

#[test]
fn jobs_selection_follows_the_row_when_new_jobs_arrive() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, started(2));
    keys(&mut s, "g j");
    keys(&mut s, "j");
    let selected = s.fileops.jobs[s.fileops.jobs_selected].id;
    update(&mut s, started(3));
    assert_eq!(s.fileops.jobs[s.fileops.jobs_selected].id, selected);
}

#[test]
fn jobs_overlay_clear_drops_finished_rows_only() {
    let mut s = state();
    update(&mut s, started(4));
    keys(&mut s, "g j");
    keys(&mut s, "c");
    assert_eq!(s.fileops.jobs.len(), 1, "running job stays");
}

#[test]
fn quitting_with_running_jobs_asks_first() {
    let mut s = state();
    update(&mut s, started(4));
    keys(&mut s, "q");
    assert_eq!(s.quit, None);
    assert_eq!(s.overlays.last(), Some(&Overlay::Confirm));
    keys(&mut s, "escape");
    assert_eq!(s.quit, None, "esc keeps Kestrel open");
    keys(&mut s, "q");
    keys(&mut s, "enter");
    assert_eq!(s.quit, Some(QuitKind::Exit));
}

#[test]
fn quitting_without_jobs_is_immediate() {
    let mut s = state();
    keys(&mut s, "q");
    assert_eq!(s.quit, Some(QuitKind::Exit));
}

#[test]
fn printable_keys_never_reach_the_keymap_from_any_text_input() {
    // Shell prompt.
    let mut s = state();
    keys(&mut s, "!");
    typed(&mut s, "qdDxyp!jk");
    assert_eq!(s.quit, None);
    assert_eq!(s.fileops.prompt.as_ref().unwrap().input.text(), "qdDxyp!jk");

    // Rename prompt.
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    keys(&mut s, "r");
    typed(&mut s, "qQuD");
    assert_eq!(s.quit, None);
    assert!(s.fileops.register.is_none());

    // The inline rename of a conflict dialog.
    let mut s = state();
    update(&mut s, started(1));
    show_conflict(&mut s, 1);
    keys(&mut s, "r");
    typed(&mut s, "qosk");
    assert_eq!(s.quit, None);
    let dialog = s.fileops.conflicts.front().unwrap();
    assert!(dialog.rename.is_some(), "still renaming");
    assert!(
        resolves(&keys(&mut s, "ctrl-u")).is_empty(),
        "nothing answered"
    );
}

#[test]
fn open_with_acts_on_every_mark() {
    let mut s = state();
    keys(&mut s, "v j");
    let effects = keys(&mut s, "O");
    let [Effect::ListOpenHandlers { paths }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(paths.len(), 2);
    update(
        &mut s,
        Msg::OpenHandlers {
            paths: paths.clone(),
            names: vec!["Notepad".into()],
        },
    );
    assert_eq!(s.fileops.menu.as_ref().unwrap().title, "Open 2 items with");
    let effects = keys(&mut s, "1");
    let [Effect::OpenWith { paths: opened, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(opened, paths);
    assert_eq!(s.view.marks_count(), 0, "marks are consumed");
}

#[test]
fn invalid_bulk_rename_with_a_kept_file_reopens_or_discards_it() {
    let retry = BulkRetry {
        dir: PathBuf::from("/work/proj"),
        names: vec!["a.txt".into()],
        file: PathBuf::from("/tmp/kestrel-bulk-rename-1.txt"),
    };
    let invalid = |retry: &BulkRetry| {
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 1: empty name".into()],
            retry: Some(retry.clone()),
        })
    };
    let mut s = state();
    update(&mut s, invalid(&retry));
    let confirm = s.fileops.confirm.as_ref().unwrap();
    assert_eq!(confirm.accept_label, "edit again");
    assert!(confirm.lines.last().unwrap().contains("kept"));
    let effects = keys(&mut s, "enter");
    assert_eq!(
        effects,
        vec![Effect::BulkRenameEdit {
            dir: retry.dir.clone(),
            names: retry.names.clone(),
            reuse: Some(retry.file.clone()),
        }]
    );
    assert!(s.overlays.is_empty());

    update(&mut s, invalid(&retry));
    let effects = keys(&mut s, "esc");
    assert!(effects.contains(&Effect::BulkRenameDiscard {
        file: retry.file.clone()
    }));
    assert!(s.overlays.is_empty());
    assert_eq!(status(&s), "Bulk rename discarded");
}

#[test]
fn n_on_the_bulk_reedit_dialog_discards_the_kept_file() {
    let retry = BulkRetry {
        dir: PathBuf::from("/work/proj"),
        names: vec!["a.txt".into()],
        file: PathBuf::from("/tmp/kestrel-bulk-rename-2.txt"),
    };
    let mut s = state();
    update(
        &mut s,
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 1: empty name".into()],
            retry: Some(retry.clone()),
        }),
    );
    let effects = keys(&mut s, "n");
    assert!(effects.contains(&Effect::BulkRenameDiscard { file: retry.file }));
    assert!(s.overlays.is_empty());
    assert_eq!(status(&s), "Bulk rename discarded");
}

#[test]
fn quitting_over_the_bulk_reedit_dialog_discards_the_kept_file() {
    let retry = BulkRetry {
        dir: PathBuf::from("/work/proj"),
        names: vec!["a.txt".into()],
        file: PathBuf::from("/tmp/kestrel-bulk-rename-3.txt"),
    };
    let mut s = state();
    update(
        &mut s,
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 1: empty name".into()],
            retry: Some(retry.clone()),
        }),
    );
    let mut effects = Vec::new();
    crate::fileops::request_quit(&mut s, crate::state::QuitKind::Exit, &mut effects);
    assert!(effects.contains(&Effect::BulkRenameDiscard { file: retry.file }));
}

fn open_reedit(s: &mut AppState, file: &str) -> BulkRetry {
    let retry = BulkRetry {
        dir: PathBuf::from("/work/proj"),
        names: vec!["a.txt".into()],
        file: PathBuf::from(file),
    };
    update(
        s,
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 1: empty name".into()],
            retry: Some(retry.clone()),
        }),
    );
    retry
}

#[test]
fn interrupt_over_the_bulk_reedit_dialog_discards_the_kept_file() {
    let mut s = state();
    let retry = open_reedit(&mut s, "/tmp/kestrel-bulk-rename-4.txt");
    let effects = update(&mut s, Msg::Interrupt);
    assert!(effects.contains(&Effect::BulkRenameDiscard { file: retry.file }));
    assert!(s.quit.is_some());
}

#[test]
fn asking_to_delete_over_the_bulk_reedit_dialog_discards_the_kept_file() {
    let mut s = state();
    cursor_to(&mut s, "a.txt");
    let retry = open_reedit(&mut s, "/tmp/kestrel-bulk-rename-5.txt");
    let mut effects = Vec::new();
    ask_delete(&mut s, &mut effects);
    assert!(effects.contains(&Effect::BulkRenameDiscard { file: retry.file }));
    assert!(!matches!(
        s.fileops.confirm.as_ref().and_then(|c| c.action.as_ref()),
        Some(ConfirmAction::BulkReedit(_))
    ));
}

#[test]
fn a_retry_reusing_the_same_edit_file_does_not_discard_it() {
    let mut s = state();
    let retry = open_reedit(&mut s, "/tmp/kestrel-bulk-rename-6.txt");
    let effects = update(
        &mut s,
        Msg::BulkPlanned(BulkPlanResult::Invalid {
            problems: vec!["line 2: duplicate".into()],
            retry: Some(retry.clone()),
        }),
    );
    assert!(
        !effects.contains(&Effect::BulkRenameDiscard { file: retry.file }),
        "the new dialog still points at the file"
    );
    assert!(matches!(
        s.fileops.confirm.as_ref().and_then(|c| c.action.as_ref()),
        Some(ConfirmAction::BulkReedit(_))
    ));
}

#[test]
fn a_fresh_conflict_dialog_swallows_keys_typed_before_it_appeared() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, conflict(1));
    assert!(!s.fileops.conflicts[0].armed);
    // The user was typing "foOs" into something; none of it answers the question.
    for k in ["f", "o", "O", "s", "k", "c", "a", "r", "enter", "esc"] {
        let effects = keys(&mut s, k);
        assert!(resolves(&effects).is_empty(), "{k}");
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::StartTimer {
                    kind: TimerKind::ConflictArm,
                    ..
                }
            )),
            "{k}: typing keeps postponing the arming"
        );
    }
    assert_eq!(s.overlays, vec![Overlay::Conflict]);
    assert!(!s.fileops.conflicts[0].apply_to_all);
    assert!(s.fileops.conflicts[0].rename.is_none());

    // A timer from before the last key is stale.
    let stale = Gen::from_raw(s.fileops.arm_gen.current().raw() - 1);
    update(
        &mut s,
        Msg::Timer {
            kind: TimerKind::ConflictArm,
            gen: stale,
        },
    );
    assert!(!s.fileops.conflicts[0].armed);
    arm(&mut s);
    assert!(s.fileops.conflicts[0].armed);
    let effects = keys(&mut s, "s");
    assert_eq!(
        resolves(&effects),
        vec![(JobId(1), ConflictAnswer::Skip, false)]
    );
}

#[test]
fn overwrite_needs_shift_o() {
    let mut s = state();
    update(&mut s, started(1));
    show_conflict(&mut s, 1);
    assert!(resolves(&keys(&mut s, "o")).is_empty(), "plain o is inert");
    assert_eq!(s.overlays, vec![Overlay::Conflict]);
    assert_eq!(
        resolves(&keys(&mut s, "O")),
        vec![(JobId(1), ConflictAnswer::Overwrite, false)]
    );
}

#[test]
fn finishing_a_move_keeps_a_newer_cut() {
    let mut s = state();
    s.fileops.register = Some(Register {
        paths: vec![PathBuf::from("/elsewhere/b.txt")],
        cut: true,
    });
    // The first move (of a.txt) completes after the user cut b.txt.
    let mut finished = done(1, OpKind::Move, JobStatus::Done);
    finished.removed = vec![PathBuf::from("/elsewhere/a.txt")];
    update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    assert_eq!(
        s.fileops.register.as_ref().map(|r| r.paths.clone()),
        Some(vec![PathBuf::from("/elsewhere/b.txt")])
    );
}

#[test]
fn finishing_a_move_consumes_replaced_and_merged_sources() {
    let mut s = state();
    s.fileops.register = Some(Register {
        paths: vec![PathBuf::from("/elsewhere/A")],
        cut: true,
    });
    // Overwrite/merge moves list the source only in `consumed_sources`.
    let mut finished = done(1, OpKind::Move, JobStatus::Done);
    finished.consumed_sources = vec![PathBuf::from("/elsewhere/A")];
    update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    assert!(s.fileops.register.is_none());
}

#[test]
fn disarmed_conflict_dialog_blocks_ctrl_c_and_each_key_restarts_the_timer() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, conflict(1));
    let gens = |effects: &[Effect]| -> Vec<u64> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::StartTimer {
                    kind: TimerKind::ConflictArm,
                    gen,
                    ..
                } => Some(gen.raw()),
                _ => None,
            })
            .collect()
    };
    let a = gens(&keys(&mut s, "ctrl-c"));
    assert!(resolves(&keys(&mut s, "ctrl-c")).is_empty());
    let b = gens(&keys(&mut s, "x"));
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert!(b[0] > a[0], "a newer key supersedes the older timer");
    assert!(!s.fileops.conflicts[0].armed);
}

#[test]
fn a_queued_conflict_dialog_gets_its_own_grace_period() {
    let mut s = state();
    update(&mut s, started(1));
    update(&mut s, started(2));
    show_conflict(&mut s, 1);
    update(&mut s, conflict(2));
    let effects = keys(&mut s, "s");
    assert_eq!(resolves(&effects)[0].0, JobId(1));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            kind: TimerKind::ConflictArm,
            ..
        }
    )));
    assert!(!s.fileops.conflicts[0].armed);
    // A key pressed right away does not answer job 2.
    assert!(resolves(&keys(&mut s, "s")).is_empty());
    assert_eq!(s.fileops.conflicts.len(), 1);

    // Job 3 queues behind job 2; job 2 finishing elsewhere re-arms the next front dialog.
    update(&mut s, started(3));
    update(&mut s, conflict(3));
    let effects = update(
        &mut s,
        Msg::Job(JobMsg::Finished(done(
            2,
            OpKind::Copy,
            JobStatus::Cancelled,
        ))),
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            kind: TimerKind::ConflictArm,
            ..
        }
    )));
    assert!(!s.fileops.conflicts[0].armed);
    assert_eq!(s.fileops.conflicts[0].info.job, JobId(3));
}

#[test]
fn file_op_completion_does_not_relist_over_a_results_list() {
    let mut s = state();
    let listing = |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::ListDir { .. }));
    let mut finished = done(1, OpKind::Delete, JobStatus::Done);
    finished.removed = vec![p("a.txt")];

    // Control: in the plain directory view the finished delete re-lists.
    let effects = update(&mut s, Msg::Job(JobMsg::Finished(finished.clone())));
    assert!(listing(&effects), "{effects:?}");

    // A find/grep results list (a labelled view) keeps its rows.
    let mut s = state();
    s.view.label = Some("find: foo".into());
    let effects = update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    assert!(!listing(&effects), "{effects:?}");
}

#[test]
fn file_op_completion_prunes_removed_rows_from_a_results_list() {
    let mut s = state();
    s.view.label = Some("find: foo".into());
    let entry = |n: &str| Entry::new(n, crate::entry::EntryKind::File);
    s.view
        .replace_entries(vec![entry("a.txt"), entry("sub/b.txt"), entry("c.txt")], 20);
    s.view.finish_loading();
    let mut finished = done(1, OpKind::Delete, JobStatus::Done);
    finished.removed = vec![p("a.txt"), PathBuf::from("/work/proj").join("sub/b.txt")];
    let _ = update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    let names: Vec<_> = s.view.entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("c.txt")]);
}

#[test]
fn file_op_completion_renames_rows_in_a_results_list() {
    let mut s = state();
    s.view.label = Some("find: foo".into());
    let entry = |n: &str| Entry::new(n, crate::entry::EntryKind::File);
    s.view
        .replace_entries(vec![entry("a.txt"), entry("c.txt")], 20);
    s.view.finish_loading();
    let mut finished = done(1, OpKind::Rename, JobStatus::Done);
    finished.removed = vec![p("a.txt")];
    finished.created = vec![p("z.txt")];
    let _ = update(&mut s, Msg::Job(JobMsg::Finished(finished)));
    let names: Vec<_> = s.view.entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(
        names,
        vec![
            std::ffi::OsString::from("c.txt"),
            std::ffi::OsString::from("z.txt")
        ]
    );
}
