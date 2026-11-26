use super::diagnostics::phase_note;
use super::{instruction_width, planned_instruction_width};
use crate::dockerfile::DockerfileParser;
use crate::step_log::StepPlace;
use slipfork_world::snapshot::PhaseTiming;
use std::time::Duration;

#[test]
fn phase_note_reports_the_real_split_and_honest_total() {
    let timing = PhaseTiming {
        clone: Duration::ZERO,
        exec: Duration::from_secs_f64(3.02),
        seal: Duration::from_secs_f64(4.21),
        record: Duration::from_secs_f64(0.01),
        publish: Duration::ZERO,
    };
    assert_eq!(
        phase_note(StepPlace::new(6, 7, 0), timing),
        "step 6/7 clone 0.00s exec 3.02s seal 4.21s record 0.01s publish 0.00s total 7.24s"
    );
}

#[test]
fn widest_keyword_spans_mixed_from_copy_add_run_workdir() {
    let source = "FROM python:3.14-trixie AS base\n\
                  COPY . /app\n\
                  ADD x /y\n\
                  RUN echo hi\n\
                  WORKDIR /code\n";
    let stages = DockerfileParser::parse(source).expect("the fixture parses");
    assert_eq!(instruction_width(&stages), "WORKDIR".len());
}

#[test]
fn planned_width_honours_the_target_prefix() {
    let source = "FROM alpine AS base\n\
                  COPY . /app\n\
                  FROM alpine AS build\n\
                  RUN true\n\
                  FROM alpine AS later\n\
                  WORKDIR /code\n";
    assert_eq!(
        planned_instruction_width(source, Some("build")).expect("the target exists"),
        "COPY".len(),
        "a later stage's WORKDIR never widens the requested prefix"
    );
    assert_eq!(
        planned_instruction_width(source, None).expect("the final stage"),
        "WORKDIR".len()
    );
}
