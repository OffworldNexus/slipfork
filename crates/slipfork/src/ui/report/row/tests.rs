use std::num::NonZeroUsize;

use super::{Counter, Done, FailureFacts, Outcome, Row, RowLevel, StepSettlement};
use crate::ui::Palette;

fn counter(index: usize, total: usize) -> Option<Counter> {
    Some(Counter { index, total })
}

fn red(mark: &str, word: &str) -> (String, String) {
    (
        format!("\u{1b}[31m{mark}\u{1b}[0m"),
        format!("\u{1b}[31m{word}\u{1b}[0m"),
    )
}

#[test]
fn the_plain_icon_state_table_is_byte_exact() {
    let plain = Palette::new(false);
    for (outcome, icon, state) in [
        (
            Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(1).unwrap(),
            }),
            "✓",
            "built 1 step",
        ),
        (
            Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(3).unwrap(),
            }),
            "✓",
            "built 3 steps",
        ),
        (Outcome::Ok(Done::Pulled), "✓", "pulled"),
        (Outcome::Ok(Done::Ready), "✓", "ready"),
        (Outcome::Ok(Done::Running), "✓", "running"),
        (Outcome::Ok(Done::Settled), "✓", "done"),
        (Outcome::Ok(Done::Step), "✓", ""),
        (Outcome::Cached, "✓", "cached"),
        (Outcome::Skipped, "•", "up to date"),
        (Outcome::Running, "→", "running"),
        (Outcome::Failed { at: None }, "✗", "failed"),
        (Outcome::Failed { at: counter(2, 3) }, "✗", "failed at 2/3"),
        (Outcome::Refused, "✗", "refused"),
        (Outcome::Problem, "!", ""),
        (Outcome::Stopped, "○", "stopped"),
        (Outcome::Orphaned, "•", "orphaned"),
    ] {
        assert_eq!(outcome.icon(plain), icon, "{outcome:?}");
        assert_eq!(outcome.state(plain), state, "{outcome:?}");
    }
}

#[test]
fn the_colored_icon_state_table_uses_the_role_codes() {
    let colored = Palette::new(true);
    let (failed_icon, failed_state) = red("✗", "failed");
    for (outcome, icon, state) in [
        (
            Outcome::Ok(Done::Ready),
            "\u{1b}[32m✓\u{1b}[0m".to_owned(),
            "\u{1b}[32mready\u{1b}[0m".to_owned(),
        ),
        (
            Outcome::Cached,
            "\u{1b}[32m✓\u{1b}[0m".to_owned(),
            "\u{1b}[32mcached\u{1b}[0m".to_owned(),
        ),
        (
            Outcome::Skipped,
            "\u{1b}[38;5;244m•\u{1b}[0m".to_owned(),
            "\u{1b}[32mup to date\u{1b}[0m".to_owned(),
        ),
        (
            Outcome::Running,
            "→".to_owned(),
            "\u{1b}[32mrunning\u{1b}[0m".to_owned(),
        ),
        (
            Outcome::Failed { at: None },
            failed_icon.clone(),
            failed_state.clone(),
        ),
        (Outcome::Refused, failed_icon.clone(), red("x", "refused").1),
        (
            Outcome::Stopped,
            "\u{1b}[38;5;244m○\u{1b}[0m".to_owned(),
            "\u{1b}[33mstopped\u{1b}[0m".to_owned(),
        ),
        (
            Outcome::Orphaned,
            "\u{1b}[33m•\u{1b}[0m".to_owned(),
            "\u{1b}[33morphaned\u{1b}[0m".to_owned(),
        ),
    ] {
        assert_eq!(outcome.icon(colored), icon, "{outcome:?}");
        assert_eq!(outcome.state(colored), state, "{outcome:?}");
    }
    assert_eq!(
        Outcome::Ok(Done::Built {
            steps: NonZeroUsize::new(2).unwrap()
        })
        .state(colored),
        "\u{1b}[32mbuilt\u{1b}[0m 2 steps"
    );
}

#[test]
fn built_or_cached_maps_zero_to_cached_and_n_to_built() {
    assert_eq!(Done::built_or_cached(0), Outcome::Cached);
    assert_eq!(
        Done::built_or_cached(1),
        Outcome::Ok(Done::Built {
            steps: NonZeroUsize::new(1).unwrap()
        })
    );
    assert_eq!(
        Done::built_or_cached(9),
        Outcome::Ok(Done::Built {
            steps: NonZeroUsize::new(9).unwrap()
        })
    );
}

#[test]
fn cached_and_skipped_rows_never_print_a_run_id() {
    let plain = Palette::new(false);
    for state in [Outcome::Cached, Outcome::Skipped] {
        let row = Row {
            level: RowLevel::Outer,
            counter: counter(1, 2),
            keyword: Some("setup".to_owned()),
            target: Some("api".to_owned()),
            state,
            command: Some("uv sync".to_owned()),
            timing_ms: Some(41),
            id: Some("3f9c21".to_owned()),
        };
        assert!(!row.line(plain).contains('#'), "{}", row.line(plain));
        assert!(!state.shows_run_id());
    }
    let executed = Row {
        level: RowLevel::Outer,
        counter: counter(1, 2),
        keyword: Some("setup".to_owned()),
        target: Some("api".to_owned()),
        state: Outcome::Ok(Done::Built {
            steps: NonZeroUsize::new(1).unwrap(),
        }),
        command: Some("uv sync".to_owned()),
        timing_ms: Some(41),
        id: Some("3f9c21".to_owned()),
    };
    let line = executed.line(plain);
    assert!(line.starts_with("[1/2] SETUP api "), "{line}");
    assert!(line.ends_with(" (41ms) #3f9c21"), "{line}");
}

#[test]
fn every_step_settlement_maps_to_its_outcome() {
    let facts = FailureFacts {
        code: "build".to_owned(),
        kind: "RUN".to_owned(),
        phase: Some("build".to_owned()),
        target: Some("api".to_owned()),
        at: counter(2, 3),
        exit_code: Some(1),
        run_id: Some("abc".to_owned()),
        reason: "exit 1".to_owned(),
        runtime_ms: Some(9),
        next: "slipfork logs".to_owned(),
    };
    assert_eq!(
        StepSettlement::Completed {
            done: Done::Pulled,
            duration_ms: 3,
            run_id: Some("abc".to_owned()),
        }
        .outcome(),
        Outcome::Ok(Done::Pulled)
    );
    assert_eq!(
        StepSettlement::Cached { duration_ms: 3 }.outcome(),
        Outcome::Cached
    );
    assert_eq!(
        StepSettlement::Skipped { duration_ms: 3 }.outcome(),
        Outcome::Skipped
    );
    assert_eq!(
        StepSettlement::Failed(facts.clone()).outcome(),
        Outcome::Failed { at: counter(2, 3) }
    );
}

#[test]
fn settlement_wire_shape_is_stable_and_round_trips() {
    let value = |settlement: &StepSettlement| {
        serde_json::to_value(settlement).expect("serialize a settlement")
    };
    assert_eq!(
        value(&StepSettlement::Cached { duration_ms: 7 }),
        serde_json::json!({ "state": "cached", "duration_ms": 7 })
    );
    assert_eq!(
        value(&StepSettlement::Skipped { duration_ms: 7 }),
        serde_json::json!({ "state": "skipped", "duration_ms": 7 })
    );
    assert_eq!(
        value(&StepSettlement::Completed {
            done: Done::Built {
                steps: NonZeroUsize::new(2).unwrap(),
            },
            duration_ms: 9,
            run_id: Some("abc".to_owned()),
        }),
        serde_json::json!({
            "state": "completed",
            "done": { "built": { "steps": 2 } },
            "duration_ms": 9,
            "run_id": "abc",
        })
    );
    assert_eq!(
        value(&StepSettlement::Completed {
            done: Done::Pulled,
            duration_ms: 3,
            run_id: None,
        }),
        serde_json::json!({
            "state": "completed",
            "done": "pulled",
            "duration_ms": 3,
            "run_id": null,
        })
    );
    let failed = StepSettlement::Failed(FailureFacts {
        code: "build".to_owned(),
        kind: "RUN".to_owned(),
        phase: Some("build".to_owned()),
        target: Some("api".to_owned()),
        at: counter(2, 3),
        exit_code: Some(1),
        run_id: Some("abc".to_owned()),
        reason: "exit 1".to_owned(),
        runtime_ms: Some(9),
        next: "slipfork logs".to_owned(),
    });
    let envelope = value(&failed);
    assert_eq!(
        envelope,
        serde_json::json!({
            "state": "failed",
            "code": "build",
            "kind": "RUN",
            "phase": "build",
            "target": "api",
            "at": { "index": 2, "total": 3 },
            "exit_code": 1,
            "run_id": "abc",
            "reason": "exit 1",
            "runtime_ms": 9,
            "next": "slipfork logs",
        })
    );
    assert_eq!(
        serde_json::from_value::<StepSettlement>(envelope).expect("deserialize a settlement"),
        failed
    );
}
