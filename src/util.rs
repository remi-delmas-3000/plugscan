// Sanitization for strings that cross a trust boundary before reaching the
// terminal or the catalog: web page captures, bundle metadata, resolver
// fields. Control characters (including ANSI escape introducers) can rewrite
// or mask terminal output, so they never survive ingestion.

/// Single-line fields: names, versions, URLs. All control chars removed.
pub fn sanitize_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Multi-line prose (manual walkthrough steps): newlines survive, every other
/// control char is removed.
pub fn sanitize_text(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}
