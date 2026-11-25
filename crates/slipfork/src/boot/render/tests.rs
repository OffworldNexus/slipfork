use std::time::Duration;

use super::{
    BarProgress, OuterWidths, completed_row, dropped_row, outer_widths, panel_line, percent_of,
    status_line,
};
use crate::ui::Palette;
use crate::ui::report::{Counter, Done, Outcome, Row};

/// M3.149: an overfull fraction (`4/3`, the shape a late `step` frame produces) must render a
/// full 24-cell bar and `100%`, never `capacity overflow` from `cells - filled`.
#[test]
fn an_overfull_fraction_clamps_to_a_full_bar_and_one_hundred_percent() {
    assert_eq!(percent_of(4, 3), 100);
    assert_eq!(percent_of(u64::MAX, 1), 100);
    assert_eq!(percent_of(0, 1), 0);

    let progress = BarProgress {
        done: 3,
        total: 3,
        num: 4,
        den: 3,
    };
    let plain = status_line('⠋', progress, "start web", Duration::from_secs(1), 120);
    assert!(plain.contains("3/3"), "{plain:?}");
    assert!(plain.contains("100%"), "{plain:?}");
    assert!(
        plain.contains(&format!("[{}]", "#".repeat(24))),
        "a full 24-cell bar: {plain:?}"
    );
    assert!(!plain.contains("panicked"), "{plain:?}");

    let styled = panel_line('⠋', progress, "start web", Duration::from_secs(1), 120);
    assert!(styled.contains(&"█".repeat(24)), "{styled:?}");
    assert!(
        !styled.contains('░'),
        "no empty cells when full: {styled:?}"
    );
}

/// A prep frame has no lifecycle state: its `detail` is the command cell and state reads `done`.
#[test]
fn a_prep_frame_lands_its_detail_in_the_command_column() {
    let widths = outer_widths(&[("fork".to_owned(), "snapshot".to_owned())]);
    let row = Row::settled(
        Some(Counter { index: 1, total: 5 }),
        "fork",
        "snapshot",
        Outcome::Ok(Done::Settled),
        Some(34),
    )
    .with_command(Some("project + world data".to_owned()));
    assert_eq!(
        completed_row(&row, &widths, Palette::new(false)),
        "✓ [1/5] fork  snapshot  done  project + world data  34ms"
    );
    let styled = completed_row(&row, &widths, Palette::new(true));
    assert!(styled.contains("\u{1b}[32mdone\u{1b}[0m"), "{styled:?}");
    assert!(
        styled.contains("project + world data  \u{1b}[38;5;244m34ms"),
        "{styled:?}"
    );
    assert!(!styled.contains("\u{1b}[2m"), "no SGR 2: {styled:?}");
}

#[test]
fn plan_widths_hold_every_row_in_the_same_columns() {
    let plan = vec![
        ("image".to_owned(), "api".to_owned()),
        ("setup".to_owned(), "uv sync".to_owned()),
        ("service".to_owned(), "web".to_owned()),
    ];
    let widths = outer_widths(&plan);
    assert_eq!(
        widths,
        OuterWidths {
            kind: 5,
            target: 7,
            state: 0
        }
    );
    let cached = Row::settled(
        Some(Counter { index: 1, total: 3 }),
        "image",
        "api",
        Outcome::Cached,
        Some(8),
    );
    let settled = Row::settled(
        Some(Counter { index: 2, total: 3 }),
        "setup",
        "uv sync",
        Outcome::Ok(Done::Declined),
        Some(0),
    );
    assert_eq!(
        completed_row(&cached, &widths, Palette::new(false)),
        "✓ [1/3] image  api      cached  8ms"
    );
    assert_eq!(
        completed_row(&cached, &widths, Palette::new(true)),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mcached\u{1b}[0m  \u{1b}[38;5;244m8ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(&settled, &widths, Palette::new(false)),
        "• [2/3] setup  uv sync  up to date  <1ms"
    );
    assert!(!completed_row(&cached, &widths, Palette::new(false)).contains('\u{1b}'));
}

/// M3.133: `drop`'s four rows share one plan and one verdict; the bytes are pinned plain and styled.
#[test]
fn drop_rows_share_one_plan_and_a_single_verdict() {
    let rows: Vec<(String, String)> = ["stop", "stop", "delete", "delete"]
        .into_iter()
        .map(|kind| (kind.to_owned(), "jello".to_owned()))
        .collect();
    let widths = outer_widths(&rows);
    assert_eq!((widths.kind, widths.target, widths.state), (6, 5, 0));

    let stop = Row::settled(
        Some(Counter { index: 2, total: 4 }),
        "stop",
        "jello",
        Outcome::Ok(Done::Settled),
        Some(100),
    )
    .with_command(Some("pods stopped".to_owned()));
    let delete = Row::settled(
        Some(Counter { index: 4, total: 4 }),
        "delete",
        "jello",
        Outcome::Ok(Done::Settled),
        Some(101),
    )
    .with_command(Some("project + world data".to_owned()));
    let plain_stop = completed_row(&stop, &widths, Palette::new(false));
    assert_eq!(
        plain_stop,
        "✓ [2/4] stop    jello  done  pods stopped  100ms"
    );
    let plain_delete = completed_row(&delete, &widths, Palette::new(false));
    assert_eq!(
        plain_delete,
        "✓ [4/4] delete  jello  done  project + world data  101ms"
    );
    assert!(!plain_stop.contains('\u{1b}') && !plain_delete.contains('\u{1b}'));

    let styled_stop = completed_row(&stop, &widths, Palette::new(true));
    for needle in [
        "\u{1b}[35mstop\u{1b}[0m",
        "\u{1b}[1mjello\u{1b}[0m",
        "\u{1b}[32mdone\u{1b}[0m",
        "\u{1b}[38;5;244m[2/4]\u{1b}[0m",
        "\u{1b}[38;5;244m100ms\u{1b}[0m",
    ] {
        assert!(
            styled_stop.contains(needle),
            "{styled_stop:?} missing {needle:?}"
        );
    }
    let plain_verdict = dropped_row("jello", 101, false);
    assert_eq!(plain_verdict, "✓ Dropped jello (101ms)");
    let styled_verdict = dropped_row("jello", 101, true);
    assert!(styled_verdict.contains("\u{1b}[1mjello\u{1b}[0m"));
    assert!(styled_verdict.contains("\u{1b}[38;5;244m(101ms)\u{1b}[0m"));
}
