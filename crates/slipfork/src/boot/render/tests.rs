use serde_json::json;

use super::{Node, OuterWidths, completed_row, dropped_row, header_row, outer_widths};

/// A prep frame has no lifecycle state: its `detail` is the command cell and state reads `done`.
#[test]
fn a_prep_frame_lands_its_detail_in_the_command_column() {
    let widths = outer_widths(&[("fork".to_owned(), "snapshot".to_owned())]);
    let snapshot = json!({
        "event": "finished", "step": 1, "total": 5, "kind": "fork",
        "name": "snapshot", "detail": "project + world data", "elapsed_ms": 34,
    });
    let node = Node::from_event(&snapshot).expect("a complete prep frame parses");
    assert_eq!(node.command, Some("project + world data"));
    assert_eq!(
        completed_row(&node, 1, 5, &widths, false),
        "✓ [1/5] fork  snapshot  done  project + world data  34ms"
    );
    let styled = completed_row(&node, 1, 5, &widths, true);
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
    let cached = Node {
        kind: "image",
        name: "api",
        state: "cached",
        millis: 8,
        run_id: None,
        command: None,
        success: true,
    };
    let settled = Node {
        kind: "setup",
        name: "uv sync",
        state: "up to date",
        millis: 0,
        run_id: None,
        command: None,
        success: false,
    };
    assert_eq!(
        completed_row(&cached, 1, 3, &widths, false),
        "✓ [1/3] image  api      cached  8ms"
    );
    assert_eq!(
        completed_row(&cached, 1, 3, &widths, true),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[38;5;244m[1/3]\u{1b}[0m \u{1b}[35mimage\u{1b}[0m  \u{1b}[1mapi\u{1b}[0m      \u{1b}[32mcached\u{1b}[0m  \u{1b}[38;5;244m8ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(&settled, 2, 3, &widths, false),
        "• [2/3] setup  uv sync  up to date  <1ms"
    );
    assert!(!completed_row(&cached, 1, 3, &widths, false).contains('\u{1b}'));
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
    assert_eq!(header_row(1, 4, "stop", "jello"), "→ [1/4] stop jello");
    assert_eq!(header_row(3, 4, "delete", "jello"), "→ [3/4] delete jello");

    let stop = Node {
        kind: "stop",
        name: "jello",
        state: "",
        command: Some("pods stopped"),
        millis: 100,
        run_id: None,
        success: true,
    };
    let delete = Node {
        kind: "delete",
        name: "jello",
        state: "",
        command: Some("project + world data"),
        millis: 101,
        run_id: None,
        success: true,
    };
    let plain_stop = completed_row(&stop, 2, 4, &widths, false);
    assert_eq!(
        plain_stop,
        "✓ [2/4] stop    jello  done  pods stopped  100ms"
    );
    let plain_delete = completed_row(&delete, 4, 4, &widths, false);
    assert_eq!(
        plain_delete,
        "✓ [4/4] delete  jello  done  project + world data  101ms"
    );
    assert!(!plain_stop.contains('\u{1b}') && !plain_delete.contains('\u{1b}'));

    let styled_stop = completed_row(&stop, 2, 4, &widths, true);
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
