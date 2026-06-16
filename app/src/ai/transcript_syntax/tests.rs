use super::*;

fn highlighted(code: &str) -> Vec<(String, SyntaxToken)> {
    let chars: Vec<char> = code.chars().collect();
    highlight_spans(code)
        .into_iter()
        .map(|(range, token)| (chars[range].iter().collect(), token))
        .collect()
}

#[test]
fn splits_prose_and_fenced_code() {
    let text = "Here is a fix:\n```rust\nfn main() {}\n```\nDone.\n";
    let segments = split_segments(text);

    assert_eq!(
        segments,
        vec![
            TranscriptSegment::Prose("Here is a fix:\n".to_string()),
            TranscriptSegment::Code {
                language: Some("rust".to_string()),
                code: "fn main() {}\n".to_string(),
            },
            TranscriptSegment::Prose("Done.\n".to_string()),
        ]
    );
}

#[test]
fn fence_without_language_has_no_language() {
    let segments = split_segments("```\nplain\n```");
    assert_eq!(
        segments,
        vec![TranscriptSegment::Code {
            language: None,
            code: "plain\n".to_string(),
        }]
    );
}

#[test]
fn unterminated_fence_runs_to_end() {
    let segments = split_segments("intro\n```py\nx = 1\n");
    assert_eq!(
        segments,
        vec![
            TranscriptSegment::Prose("intro\n".to_string()),
            TranscriptSegment::Code {
                language: Some("py".to_string()),
                code: "x = 1\n".to_string(),
            },
        ]
    );
}

#[test]
fn plain_prose_is_a_single_segment() {
    let segments = split_segments("just some text");
    assert_eq!(
        segments,
        vec![TranscriptSegment::Prose("just some text".to_string())]
    );
}

#[test]
fn highlights_keywords_strings_and_numbers() {
    let spans = highlighted(r#"let x = "hi";"#);
    assert_eq!(
        spans,
        vec![
            ("let".to_string(), SyntaxToken::Keyword),
            (r#""hi""#.to_string(), SyntaxToken::StringLiteral),
        ]
    );

    let nums = highlighted("y = 42");
    assert_eq!(nums, vec![("42".to_string(), SyntaxToken::Number)]);
}

#[test]
fn highlights_line_and_block_comments() {
    assert_eq!(
        highlighted("x // trailing"),
        vec![("// trailing".to_string(), SyntaxToken::Comment)]
    );
    assert_eq!(
        highlighted("a /* mid */ b"),
        vec![("/* mid */".to_string(), SyntaxToken::Comment)]
    );
    assert_eq!(
        highlighted("# shell comment"),
        vec![("# shell comment".to_string(), SyntaxToken::Comment)]
    );
}

#[test]
fn keyword_match_is_whole_word_only() {
    // `iffy` contains `if` but must not be highlighted.
    assert!(highlighted("iffy = 1")
        .iter()
        .all(|(text, token)| !(text == "if" && *token == SyntaxToken::Keyword)));
}

#[test]
fn number_glued_to_identifier_is_not_highlighted() {
    // The `8` in `utf8` is part of the identifier, not a number literal.
    assert!(highlighted("utf8")
        .iter()
        .all(|(_, token)| *token != SyntaxToken::Number));
}

#[test]
fn unterminated_string_stops_at_line_end() {
    let spans = highlighted("\"oops\nnext");
    assert_eq!(
        spans.first(),
        Some(&("\"oops".to_string(), SyntaxToken::StringLiteral))
    );
}

fn styled(text: &str) -> (String, Vec<(String, InlineStyle)>) {
    let (display, ranges) = parse_inline_markdown(text);
    let chars: Vec<char> = display.chars().collect();
    let spans = ranges
        .into_iter()
        .map(|(range, style)| (chars[range].iter().collect(), style))
        .collect();
    (display, spans)
}

#[test]
fn parses_inline_bold_italic_and_code() {
    let (display, spans) = styled("a **bold** and *italic* and `code` end");
    assert_eq!(display, "a bold and italic and code end");
    assert_eq!(
        spans,
        vec![
            ("bold".to_string(), InlineStyle::Bold),
            ("italic".to_string(), InlineStyle::Italic),
            ("code".to_string(), InlineStyle::Code),
        ]
    );
}

#[test]
fn underscores_in_identifiers_are_not_italic() {
    let (display, spans) = styled("call snake_case_name here");
    assert_eq!(display, "call snake_case_name here");
    assert!(spans.is_empty());
}

#[test]
fn code_span_content_is_not_reparsed() {
    let (display, spans) = styled("`a*b*c`");
    assert_eq!(display, "a*b*c");
    assert_eq!(spans, vec![("a*b*c".to_string(), InlineStyle::Code)]);
}

#[test]
fn escaped_markers_render_literally() {
    let (display, spans) = styled(r"a \*literal\* star");
    assert_eq!(display, "a *literal* star");
    assert!(spans.is_empty());
}

#[test]
fn classifies_headings_bullets_and_normal() {
    assert_eq!(
        classify_prose_line("## Title here"),
        ProseLine::Heading {
            level: 2,
            text: "Title here".to_string()
        }
    );
    assert_eq!(
        classify_prose_line("- a point"),
        ProseLine::Bullet {
            text: "a point".to_string()
        }
    );
    assert_eq!(
        classify_prose_line("just text"),
        ProseLine::Normal {
            text: "just text".to_string()
        }
    );
    // A bare `#` with no space is not a heading.
    assert_eq!(
        classify_prose_line("#nospace"),
        ProseLine::Normal {
            text: "#nospace".to_string()
        }
    );
}
