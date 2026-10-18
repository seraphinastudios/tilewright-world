// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{Document, ErrorCode, Limits, MAX_SAFE_INTEGER, Segment, Span, Value};

fn parse(source: &str) -> Result<Document, ErrorCode> {
    Document::parse(source, &Limits::default()).map_err(|error| error.code)
}

fn code(source: &str) -> ErrorCode {
    parse(source).expect_err("source must be rejected")
}

const CHUNK: &str = r#"// world/overworld/layers/0/3_37.jsonc
{
  "format": "tilewright.chunk/1",
  "plane": "overworld",
  "layer": 0,
  "chunk": [3, 37],
  /* terrain grids */
  "terrain": {
    "floor": {"palette": {"g": "grass_1", "s": "sand_2"}, "rows": ["gs", "sg"]},
    "height": {"fill": 0}
  },
  "objects": [
    {"at": [7, 0], "proto": "tree_1"},
    {"at": [2, 2], "proto": "chest", "inventory": [{"proto": "coin", "fields": {"count": 3}}]}
  ]
}
"#;

#[test]
fn parses_a_chunk_and_reads_values_by_path() {
    let document = parse(CHUNK).unwrap();
    let root = document.root();
    assert_eq!(
        root.get("format").and_then(Value::as_str),
        Some("tilewright.chunk/1")
    );
    assert_eq!(
        root.pointer(&[Segment::Key("chunk"), Segment::Index(1)])
            .and_then(Value::as_i64),
        Some(37)
    );
    let count = root
        .pointer(&[
            Segment::Key("objects"),
            Segment::Index(1),
            Segment::Key("inventory"),
            Segment::Index(0),
            Segment::Key("fields"),
            Segment::Key("count"),
        ])
        .unwrap();
    assert_eq!(count.as_i64(), Some(3));
    assert_eq!(document.text(count.span()), "3");
    let keys: Vec<&str> = root
        .as_object()
        .unwrap()
        .iter()
        .map(|member| member.key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["format", "plane", "layer", "chunk", "terrain", "objects"]
    );
}

fn walk(document: &Document, value: &Value) {
    let text = document.text(value.span());
    match value {
        Value::String { .. } => assert!(text.starts_with('"') && text.ends_with('"')),
        Value::Array { items, .. } => {
            assert!(text.starts_with('[') && text.ends_with(']'));
            for item in items {
                walk(document, item);
            }
        }
        Value::Object { members, .. } => {
            assert!(text.starts_with('{') && text.ends_with('}'));
            for member in members {
                let key = document.text(member.key_span);
                assert_eq!(key, format!("\"{}\"", member.key));
                walk(document, &member.value);
            }
        }
        _ => assert!(!text.is_empty()),
    }
}

#[test]
fn spans_cover_exact_source_text() {
    let document = parse(CHUNK).unwrap();
    walk(&document, document.root());
}

#[test]
fn replace_keeps_every_byte_outside_the_span() {
    let document = parse(CHUNK).unwrap();
    let fill = document
        .root()
        .pointer(&[
            Segment::Key("terrain"),
            Segment::Key("height"),
            Segment::Key("fill"),
        ])
        .unwrap();
    let edited = document
        .replace(fill.span(), "12", &Limits::default())
        .unwrap();
    let expected = CHUNK.replacen("\"fill\": 0", "\"fill\": 12", 1);
    assert_eq!(edited.source(), expected);
    assert!(edited.source().contains("/* terrain grids */"));
    assert!(
        edited
            .source()
            .starts_with("// world/overworld/layers/0/3_37.jsonc\n")
    );
}

#[test]
fn replace_rejects_invalid_spans_and_invalid_results() {
    let document = parse("{\"a\": \"é\"}").unwrap();
    let limits = Limits::default();
    let invalid = |span| document.replace(span, "1", &limits).unwrap_err().code;
    assert_eq!(invalid(Span { start: 4, end: 99 }), ErrorCode::InvalidEdit);
    assert_eq!(invalid(Span { start: 8, end: 9 }), ErrorCode::InvalidEdit);
    let value = document.root().get("a").unwrap().span();
    assert_eq!(
        document.replace(value, "1,", &limits).unwrap_err().code,
        ErrorCode::TrailingComma
    );
}

#[test]
fn accepts_comments_everywhere_whitespace_is_allowed() {
    let source = "/*a*/{/*b*/\"k\"/*c*/:/*d*/[/*e*/1/*f*/,//g\n2]//h\n}/*i*/";
    let document = parse(source).unwrap();
    assert_eq!(
        document
            .root()
            .pointer(&[Segment::Key("k"), Segment::Index(1)])
            .and_then(Value::as_i64),
        Some(2)
    );
}

#[test]
fn decodes_escapes_and_surrogate_pairs() {
    let document =
        parse(r#"["\"\\\/\b\f\n\r\t", "\u00e9\u4e2d", "\ud83d\ude00", "plain é"]"#).unwrap();
    let items = document.root().as_array().unwrap();
    assert_eq!(items[0].as_str(), Some("\"\\/\u{8}\u{c}\n\r\t"));
    assert_eq!(items[1].as_str(), Some("é中"));
    assert_eq!(items[2].as_str(), Some("😀"));
    assert_eq!(items[3].as_str(), Some("plain é"));
}

#[test]
fn numbers_follow_json_grammar_and_integer_range() {
    let document =
        parse("[0, -0, 12, -7, 1.5, -2.25e3, 1E2, 9007199254740991, -9007199254740991]").unwrap();
    let items = document.root().as_array().unwrap();
    assert_eq!(items[0].as_i64(), Some(0));
    assert_eq!(items[2].as_i64(), Some(12));
    assert_eq!(items[3].as_i64(), Some(-7));
    assert_eq!(items[4].as_f64(), Some(1.5));
    assert_eq!(items[5].as_f64(), Some(-2250.0));
    assert_eq!(items[6].kind(), "number");
    assert_eq!(items[7].as_i64(), Some(MAX_SAFE_INTEGER));
    assert_eq!(items[8].as_i64(), Some(-MAX_SAFE_INTEGER));
    for bad in [
        "01", "1.", ".5", "+1", "1e", "1.e3", "0x10", "-", "1ee2", "NaN", "Infinity",
    ] {
        let code = code(&format!("[{bad}]"));
        assert!(
            matches!(code, ErrorCode::InvalidNumber | ErrorCode::ExpectedValue),
            "{bad}: {code:?}"
        );
    }
    assert_eq!(code("9007199254740992"), ErrorCode::IntegerOutOfRange);
    assert_eq!(code("-99999999999999999999"), ErrorCode::IntegerOutOfRange);
    assert_eq!(code("1e400"), ErrorCode::NonFiniteNumber);
    assert_eq!(
        code(&format!("1.{}", "0".repeat(200))),
        ErrorCode::NumberTooLong
    );
}

#[test]
fn rejects_each_strictness_rule() {
    let cases = [
        ("[1, 2,]", ErrorCode::TrailingComma),
        ("{\"a\": 1,}", ErrorCode::TrailingComma),
        ("{\"a\": 1, \"a\": 2}", ErrorCode::DuplicateKey),
        ("{\"\\u0061\": 1, \"a\": 2}", ErrorCode::DuplicateKey),
        ("\u{feff}{}", ErrorCode::ByteOrderMark),
        ("{'a': 1}", ErrorCode::ExpectedKey),
        ("{a: 1}", ErrorCode::ExpectedKey),
        ("['a']", ErrorCode::ExpectedValue),
        ("{\"a\" 1}", ErrorCode::ExpectedColon),
        ("[1 2]", ErrorCode::ExpectedCommaOrEnd),
        ("{} {}", ErrorCode::TrailingContent),
        ("", ErrorCode::ExpectedValue),
        ("  // only a comment\n", ErrorCode::ExpectedValue),
        ("[1", ErrorCode::UnexpectedEnd),
        ("{\"a\": 1", ErrorCode::UnexpectedEnd),
        ("\"abc", ErrorCode::UnterminatedString),
        ("\"a\nb\"", ErrorCode::ControlCharacterInString),
        ("\"\\x41\"", ErrorCode::InvalidEscape),
        ("\"\\u12\"", ErrorCode::InvalidUnicodeEscape),
        ("\"\\ud800\"", ErrorCode::InvalidUnicodeEscape),
        ("\"\\udc00\"", ErrorCode::InvalidUnicodeEscape),
        ("\"\\ud83d\\u0041\"", ErrorCode::InvalidUnicodeEscape),
        ("\"\\u0000\"", ErrorCode::NulInString),
        ("{\"\\u0000\": 1}", ErrorCode::NulInString),
        ("/* open", ErrorCode::UnterminatedComment),
        ("/ 1", ErrorCode::UnexpectedCharacter),
        ("tru", ErrorCode::ExpectedValue),
        ("nul", ErrorCode::ExpectedValue),
    ];
    for (source, expected) in cases {
        assert_eq!(code(source), expected, "{source:?}");
    }
}

#[test]
fn reports_line_and_column() {
    let error =
        Document::parse("{\n  \"a\": 1,\n  \"é\": [1,]\n}", &Limits::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::TrailingComma);
    assert_eq!((error.line, error.column), (3, 10));
    assert_eq!(
        error.to_string(),
        "jsonc.trailing_comma at line 3, column 10"
    );
}

#[test]
fn enforces_structural_limits_while_reading() {
    let limits = Limits {
        max_depth: 3,
        max_values: 10,
        max_array_items: 4,
        max_object_keys: 2,
        max_string_bytes: 8,
        max_comments: 2,
        max_lines: 3,
        max_line_bytes: 40,
        max_input_bytes: 64,
        ..Limits::default()
    };
    let check = |source: &str| {
        Document::parse(source, &limits)
            .map(|_| ())
            .map_err(|error| error.code)
    };
    assert_eq!(check("[[[1]]]"), Ok(()));
    assert_eq!(check("[[[[1]]]]"), Err(ErrorCode::DepthLimit));
    assert_eq!(check("[1,2,3,4]"), Ok(()));
    assert_eq!(check("[1,2,3,4,5]"), Err(ErrorCode::ArrayLimit));
    assert_eq!(
        check("{\"a\":1,\"b\":2,\"c\":3}"),
        Err(ErrorCode::ObjectLimit)
    );
    assert_eq!(check("[[1,2],[3,4],[5,6],7]"), Err(ErrorCode::ValueLimit));
    assert_eq!(check("\"12345678\""), Ok(()));
    assert_eq!(check("\"123456789\""), Err(ErrorCode::StringTooLong));
    assert_eq!(check("\"12345678\\n\""), Err(ErrorCode::StringTooLong));
    assert_eq!(check("/*1*//*2*//*3*/1"), Err(ErrorCode::TooManyComments));
    assert_eq!(check("1\n\n\n"), Err(ErrorCode::TooManyLines));
    assert_eq!(
        check(&format!("\"{}\"", "a".repeat(39))),
        Err(ErrorCode::LineTooLong)
    );
    assert_eq!(check(&" ".repeat(65)), Err(ErrorCode::InputTooLarge));
}

#[test]
fn deep_nesting_is_rejected_without_stack_exhaustion() {
    let source = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(code(&source), ErrorCode::DepthLimit);
}

#[test]
fn random_input_never_panics() {
    const ALPHABET: &[u8] = b"{}[]:,\"\\/*-+.0123456789eEtrufalsn \n\tabu\x01";
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..20_000 {
        let mut text = Vec::new();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let length = usize::try_from(state % 48).unwrap();
        for _ in 0..length {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index = usize::try_from(state % ALPHABET.len() as u64).unwrap();
            text.push(ALPHABET[index]);
        }
        let source = String::from_utf8(text).unwrap();
        let _ = Document::parse(source, &Limits::default());
    }
}

#[test]
fn canonical_output_is_minified_and_keeps_number_spelling() {
    let source = "// c\n{ \"b\": [1, -0, 2.50, -0.0, 1e3], /* x */ \"a\": \"q\\\"\\\\\\n\\u0001\\u00e9\", \"n\": null, \"t\": true }\n";
    let document = parse(source).unwrap();
    assert_eq!(
        document.canonical(document.root()),
        "{\"b\":[1,-0,2.50,-0.0,1e3],\"a\":\"q\\\"\\\\\\n\\u0001\u{e9}\",\"n\":null,\"t\":true}"
    );
    let reparsed = parse(&document.canonical(document.root())).unwrap();
    assert_eq!(
        reparsed.canonical(reparsed.root()),
        document.canonical(document.root())
    );
}
