//! `server_sessions` runs `agent list` on the host and parses the rows.

use rmux_lib::server::parse_list;

#[test]
fn parse_list_rows() {
    // `agent list` prints tab-separated rows: name\tpid\tage\tattached|detached\tcommand.
    let out = "term-abc-1\t1234\t42\tdetached\tsh\n\
               claude-xyz-9\t5678\t9001\tattached\tclaude --resume cafebabe\n";
    let rows = parse_list(out);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "term-abc-1");
    assert_eq!(rows[0].pid, Some(1234));
    assert_eq!(rows[0].age_seconds, 42);
    assert!(!rows[0].attached);
    assert_eq!(rows[1].name, "claude-xyz-9");
    assert!(rows[1].attached);
    assert_eq!(rows[1].command.as_deref(), Some("claude --resume cafebabe"));
}

#[test]
fn parse_list_empty() {
    assert!(parse_list("").is_empty());
    // A row whose name is absent (blank line / nothing to parse) is skipped.
    assert!(parse_list("\t-\t0\tdetached\t\n").is_empty());
}
