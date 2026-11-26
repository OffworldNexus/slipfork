use std::path::PathBuf;
use std::time::Duration;

use slipfork_image::{StepEvent, StepPlace, StepState, StepTrace};

use serde_json::Value;
use slipfork_world::LogStore;

use super::{
    AttemptFailure, ImageAttempt, ImageSettle, failed_note, settlement, started_note, step_label,
    success_note,
};
use crate::ui::report::StepSettlement;
use crate::world::RunNotes;

/// A capture divider names the instruction's position, keyword, and args as the author wrote
/// them, with no trailing space when the instruction takes none.
#[test]
fn a_step_label_is_one_position_keyword_and_args_line() {
    let copy = StepEvent::new(
        StepPlace::new(2, 3, 2),
        "COPY",
        ". /app",
        StepState::Started,
        Duration::ZERO,
        None,
    );
    assert_eq!(step_label(&copy), "2/3 COPY . /app");

    let bare = StepEvent::new(
        StepPlace::new(1, 1, 1),
        "RUN",
        "",
        StepState::Started,
        Duration::ZERO,
        None,
    );
    assert_eq!(step_label(&bare), "1/1 RUN");
}

/// A heredoc `RUN` carries ANSI, newlines, and tabs: the row strips the escapes and stays one
/// physical line, because the capture frames rows on `\t`.
#[test]
fn a_step_label_collapses_control_space_onto_one_line() {
    let heredoc = StepEvent::new(
        StepPlace::new(3, 3, 9),
        "RUN",
        "\u{1b}[1msh -c 'echo one\necho two\t\techo three'\u{1b}[0m",
        StepState::Started,
        Duration::ZERO,
        None,
    );
    let label = step_label(&heredoc);
    assert_eq!(label, "3/3 RUN sh -c 'echo one echo two echo three'");
    assert!(
        !label.contains(&['\u{1b}', '\n', '\r', '\t'][..]),
        "a step row must never carry ANSI or a row break: {label:?}"
    );
}

/// The note a build publishes when a step STARTS names its instruction, line, and phase.
#[test]
fn a_started_step_note_names_its_instruction_and_source_line() {
    let dockerfile = PathBuf::from("/ctx/Dockerfile");
    let event = StepEvent::new(
        StepPlace::new(2, 4, 7),
        "COPY",
        "--from=missing-stage /x /y",
        StepState::Started,
        Duration::ZERO,
        None,
    );
    let note = started_note(Some(&dockerfile), &event);
    assert_eq!(note.phase.as_deref(), Some("copy"));
    assert_eq!(
        note.instruction.as_deref(),
        Some("--from=missing-stage /x /y")
    );
    assert_eq!(note.source.as_deref(), Some("Dockerfile:7"));
    assert_eq!(note.exit_code, None);
    assert_eq!(note.reason, None);

    let step_less = failed_note(&dockerfile);
    assert_eq!(step_less.phase, None);
    assert_eq!(step_less.instruction, None);
    assert_eq!(step_less.source.as_deref(), Some("Dockerfile"));
}

/// A successful build names its last instruction rather than the Dockerfile label.
#[test]
fn a_successful_build_names_its_last_instruction_not_the_dockerfile_label() {
    let node = slipfork_world::ImageNode {
        id: "web".to_owned(),
        directory: PathBuf::from("."),
        dockerfile: Some(PathBuf::from("docker/Dockerfile")),
        from: None,
        line: 3,
    };
    let last = StepTrace {
        phase: "run",
        command: "npm run build".to_owned(),
        line: 7,
    };
    let note = success_note(std::slice::from_ref(&node), "web", Some(&last));
    assert_eq!(note.phase.as_deref(), Some("run"));
    assert_eq!(note.instruction.as_deref(), Some("npm run build"));
    assert_eq!(note.source.as_deref(), Some("Dockerfile:7"));
    assert_eq!(note.exit_code, Some(0));

    let unused = success_note(std::slice::from_ref(&node), "web", None);
    assert_eq!(unused.phase.as_deref(), Some("Dockerfile"));
    assert_eq!(unused.source.as_deref(), Some("Dockerfile"));
    assert_eq!(unused.instruction, None);
}

#[test]
fn image_settlements_publish_exact_stable_json_without_fabricated_steps() {
    let value = |settlement: &StepSettlement| {
        serde_json::to_value(settlement).expect("serialize a settlement")
    };
    assert_eq!(
        value(&settlement(ImageSettle::Cached, 7, None)),
        serde_json::json!({ "state": "cached", "duration_ms": 7 })
    );
    assert_eq!(
        value(&settlement(ImageSettle::Pulled, 7, None)),
        serde_json::json!({
            "state": "completed",
            "done": "pulled",
            "duration_ms": 7,
            "run_id": null,
        })
    );
    assert_eq!(
        value(&settlement(ImageSettle::Pulled, 7, Some("abc"))),
        serde_json::json!({
            "state": "completed",
            "done": "pulled",
            "duration_ms": 7,
            "run_id": "abc",
        })
    );
    assert_eq!(
        value(&settlement(
            ImageSettle::Built(std::num::NonZeroUsize::new(2).unwrap()),
            9,
            Some("abc")
        )),
        serde_json::json!({
            "state": "completed",
            "done": { "built": { "steps": 2 } },
            "duration_ms": 9,
            "run_id": "abc",
        })
    );
}

/// A cache hit performs no work: every held inner row is dropped and only the `cached` verdict
/// is published, while a real build releases the same rows in order.
#[test]
fn a_cache_hit_drops_held_inner_rows_and_a_build_releases_them() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let dir = tempfile::Builder::new()
        .prefix("slipfork-progress-withhold-")
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory");
    let store = LogStore::at(
        dir.path().join("store"),
        slipfork_world::WorldName::parse("demo").expect("valid world name"),
    );

    let frames = Rc::new(RefCell::new(Vec::new()));
    {
        let sink = Rc::clone(&frames);
        let mut emit = move |frame: Value| sink.borrow_mut().push(frame);
        let mut attempt = ImageAttempt {
            emit: &mut emit,
            withheld: Vec::new(),
            live: false,
            total: 1,
            step: 1,
            label: "image app".to_owned(),
            image: "app".to_owned(),
            index: None,
            inner_total: None,
            run: None,
            notes: RunNotes::new(&store),
            nodes: &[],
            dockerfile: None,
            started: None,
            wrote: false,
            lines: false,
        };
        attempt
            .events(|log| {
                log.announce("FROM", "python:3.13-slim");
                log.done(Duration::ZERO);
                Ok(())
            })
            .expect("a cached pull never fails");
        attempt.finish(ImageSettle::Cached, 3, None, 0);
    }
    let cached = frames.borrow();
    assert_eq!(
        cached
            .iter()
            .map(|frame| frame["event"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["finished"],
        "a cache hit publishes only its verdict: {cached:?}"
    );
    assert_eq!(cached[0]["state"], "cached");
    drop(cached);

    frames.borrow_mut().clear();
    {
        let sink = Rc::clone(&frames);
        let mut emit = move |frame: Value| sink.borrow_mut().push(frame);
        let mut attempt = ImageAttempt {
            emit: &mut emit,
            withheld: Vec::new(),
            live: false,
            total: 1,
            step: 1,
            label: "image app".to_owned(),
            image: "app".to_owned(),
            index: None,
            inner_total: None,
            run: None,
            notes: RunNotes::new(&store),
            nodes: &[],
            dockerfile: None,
            started: None,
            wrote: false,
            lines: false,
        };
        attempt
            .events(|log| {
                log.announce("FROM", "python:3.13-slim");
                log.tail(slipfork_world::snapshot::TailPhase::Cloning);
                log.done(Duration::ZERO);
                Ok(())
            })
            .expect("a real build never fails here");
        attempt.finish(
            ImageSettle::Built(std::num::NonZeroUsize::new(1).expect("one built step")),
            9,
            None,
            1,
        );
    }
    let built = frames.borrow();
    assert_eq!(
        built
            .iter()
            .map(|frame| frame["event"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["step", "step", "step", "finished"],
        "a real build releases every held row, in order: {built:?}"
    );
}

#[test]
fn emitted_attempt_frames_carry_typed_objects() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let dir = tempfile::Builder::new()
        .prefix("slipfork-progress-frame-")
        .tempdir_in("/var/tmp")
        .expect("create a disk temp directory");
    let store = LogStore::at(
        dir.path().join("store"),
        slipfork_world::WorldName::parse("demo").expect("valid world name"),
    );
    let frames = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&frames);
    let mut emit = move |frame: Value| sink.borrow_mut().push(frame);
    let mut attempt = ImageAttempt {
        emit: &mut emit,
        withheld: Vec::new(),
        live: true,
        lines: false,
        total: 1,
        step: 1,
        label: "image api".to_owned(),
        image: "api".to_owned(),
        index: None,
        inner_total: None,
        run: None,
        notes: RunNotes::new(&store),
        nodes: &[],
        dockerfile: None,
        started: None,
        wrote: true,
    };
    attempt.finish(
        ImageSettle::Built(std::num::NonZeroUsize::new(2).unwrap()),
        9,
        None,
        2,
    );
    let finished = frames
        .borrow()
        .last()
        .cloned()
        .expect("finish emits one frame");
    assert_eq!(
        finished["settlement"],
        serde_json::json!({
            "state": "completed",
            "done": { "built": { "steps": 2 } },
            "duration_ms": 9,
            "run_id": null,
        })
    );
    assert!(finished["settlement"].is_object());

    attempt.fail(
        AttemptFailure::bare("build `api` failed: boom\nnext: fix it"),
        5,
    );
    let failed = frames
        .borrow()
        .last()
        .cloned()
        .expect("fail emits one frame");
    assert!(failed["failure"].is_object());
    assert_eq!(failed["failure"]["code"], "image-build");
    assert_eq!(failed["failure"]["kind"], "image");
    assert_eq!(failed["failure"]["next"], "fix it");
}
