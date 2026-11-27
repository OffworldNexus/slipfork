use super::{FinalVerb, ImageStream, StepFacts, step_row};
use crate::daemon::image::ImageFrame;
use crate::ui::report::{ReportRenderer, WallZone};
use crate::ui::{Terminal, UiMode};

fn renderer(mode: UiMode) -> ReportRenderer {
    ReportRenderer::new(
        Terminal {
            cols: 100,
            rows: 30,
            mode,
            stdin_tty: false,
        },
        WallZone::fixed(0).expect("utc"),
    )
}

fn step(
    index: usize,
    total: usize,
    instruction: &str,
    args: &str,
    state: &str,
    elapsed_ms: u64,
    exit_code: Option<i32>,
) -> ImageFrame {
    ImageFrame::Step {
        index,
        total,
        instruction: instruction.to_owned(),
        args: args.to_owned(),
        state: state.to_owned(),
        elapsed_ms,
        exit_code,
        phase: None,
        width: 7,
    }
}

fn rendered(mode: UiMode, json: bool, frames: &[ImageFrame]) -> String {
    let mut buffer = Vec::new();
    {
        let mut stream = ImageStream::new(
            renderer(mode),
            json,
            false,
            FinalVerb::Build,
            &mut buffer,
            None,
        );
        for frame in frames {
            stream.frame(frame);
        }
    }
    String::from_utf8(buffer).expect("utf8 image stream")
}

#[test]
fn a_plain_stream_renders_cold_warm_and_failing_frames_byte_exactly() {
    let frames = [
        step(1, 2, "RUN", "make", "started", 0, None),
        step(1, 2, "RUN", "make", "done", 41, None),
        ImageFrame::Log {
            stream: "stdout".to_owned(),
            line: "compiled \u{1b}[31mred\u{1b}[0m".to_owned(),
        },
        step(2, 2, "COPY", ". /app", "failed", 12, Some(3)),
    ];
    let shown = rendered(UiMode::Plain, false, &frames);
    assert_eq!(
        shown,
        "[1/2] run     make ✓ (41ms)\n  │ compiled red\n[2/2] copy    . /app ✗ (12ms) (exit 3)\n"
    );
    assert!(!shown.contains('\u{1b}'), "{shown:?}");
    assert!(!shown.contains('\r'), "{shown:?}");
}

#[test]
fn a_json_stream_forwards_every_step_including_started() {
    let frames = [
        step(1, 1, "RUN", "make", "started", 0, None),
        step(1, 1, "RUN", "make", "done", 41, None),
    ];
    let shown = rendered(UiMode::Json, true, &frames);
    assert!(!shown.contains('\u{1b}'), "{shown:?}");
    let lines: Vec<serde_json::Value> = shown
        .lines()
        .map(|line| serde_json::from_str(line).expect("every JSON line parses"))
        .collect();
    assert!(
        lines.iter().any(|line| line["event"] == "step"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line["event"] == "step" && line["state"] == "started"),
        "started steps are forwarded under --json: {lines:?}"
    );
}

#[test]
fn a_colour_tty_keeps_the_substep_role_and_padding() {
    let frames = [step(1, 1, "RUN", "make", "done", 41, None)];
    let shown = rendered(UiMode::Interactive { color: true }, false, &frames);
    assert!(
        shown.contains("\u{1b}[38;5;132mrun\u{1b}[0m    "),
        "{shown:?}"
    );
    assert!(!shown.contains('\r'), "{shown:?}");
}

#[test]
fn a_step_row_uses_the_plan_width_without_growing() {
    let row = step_row(StepFacts {
        index: 1,
        total: 1,
        instruction: "WORKDIR",
        args: "/app",
        state: "done",
        elapsed_ms: 1,
        exit_code: None,
        width: 4,
    });
    assert_eq!(
        row.line(crate::ui::report::Palette::new(false)),
        "[1/1] workdir /app ✓ (1ms)"
    );
}

#[test]
fn a_pull_final_row_shows_the_shared_digest_cell() {
    let mut buffer = Vec::new();
    {
        let mut stream = ImageStream::new(
            renderer(UiMode::Plain),
            false,
            false,
            FinalVerb::Pull,
            &mut buffer,
            None,
        );
        stream.final_row(&serde_json::json!({
            "reference": "ghcr.io/acme/app:latest",
            "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "cached": false,
        }));
    }
    assert_eq!(
        String::from_utf8(buffer).expect("utf8"),
        "✓ Pulled ghcr.io/acme/app:latest sha256:111111111111 sealed\n"
    );
}

#[test]
fn a_build_final_row_shortens_the_image_id_and_counts_hits() {
    let mut buffer = Vec::new();
    {
        let mut stream = ImageStream::new(
            renderer(UiMode::Plain),
            false,
            false,
            FinalVerb::Build,
            &mut buffer,
            None,
        );
        stream.final_row(&serde_json::json!({
            "component": "api",
            "image_id": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            "cache_hits": 2,
            "cache_misses": 3,
        }));
    }
    let shown = String::from_utf8(buffer).expect("utf8");
    assert_eq!(
        shown,
        "✓ Built api sha256:222222222222 (2 hits, 3 misses)\n"
    );
    assert!(!shown.contains('\u{1b}'), "{shown:?}");
    assert!(!shown.contains('\r'), "{shown:?}");
}

#[test]
fn a_pull_note_is_dim_only_in_colour_and_never_dropped() {
    let mut buffer = Vec::new();
    {
        let mut stream = ImageStream::new(
            renderer(UiMode::Plain),
            false,
            false,
            FinalVerb::Pull,
            &mut buffer,
            None,
        );
        stream.frame(&ImageFrame::Note {
            text: "pulled layer".to_owned(),
        });
    }
    assert_eq!(String::from_utf8(buffer).expect("utf8"), "  pulled layer\n");
}
