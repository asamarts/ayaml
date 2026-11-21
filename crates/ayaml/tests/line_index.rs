//! The public line index against positions worked out from the text alone.

use std::collections::BTreeMap;

use ayaml::{ColumnEncoding, LineIndex, Location, Position, PositionError};

const ENCODINGS: [ColumnEncoding; 3] = [
    ColumnEncoding::Utf8,
    ColumnEncoding::Scalar,
    ColumnEncoding::Utf16,
];

/// A line of `text`: where it starts, where its columns start, and where its content ends.
#[derive(Clone, Copy, Debug)]
struct Line {
    start: usize,
    columns: usize,
    end: usize,
}

/// The lines of `text`, found by walking its characters: a line ends at LF, at CRLF, and at a
/// CR alone. Its columns start where it does, but for the first line of a text that a
/// byte-order mark opens, whose columns start after that one mark.
fn lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut characters = text.char_indices().peekable();
    let mut close = |start: usize, end: usize| {
        let hidden = if start == 0 && text.starts_with('\u{feff}') {
            '\u{feff}'.len_utf8()
        } else {
            0
        };
        lines.push(Line {
            start,
            columns: start + hidden,
            end,
        });
    };
    while let Some((at, character)) = characters.next() {
        match character {
            '\n' => {
                close(start, at);
                start = at + 1;
            }
            '\r' => {
                close(start, at);
                start = at + 1;
                if characters.next_if(|&(_, next)| next == '\n').is_some() {
                    start += 1;
                }
            }
            _ => {}
        }
    }
    close(start, text.len());
    lines
}

fn width(character: char, encoding: ColumnEncoding) -> usize {
    match encoding {
        ColumnEncoding::Utf8 => character.len_utf8(),
        ColumnEncoding::Utf16 => character.len_utf16(),
        _ => 1,
    }
}

/// What the index has to say of `offset`, from the text alone.
fn position_of(
    text: &str,
    lines: &[Line],
    offset: usize,
    encoding: ColumnEncoding,
) -> Result<Position, PositionError> {
    if offset > text.len() {
        return Err(PositionError::OutOfBounds);
    }
    let number = lines
        .iter()
        .rposition(|line| line.start <= offset)
        .expect("a first line");
    let line = lines[number];
    if offset > line.end {
        return Err(PositionError::InsideCrLf);
    }
    if offset <= line.columns {
        return Ok(Position::new(number, 0));
    }
    if encoding == ColumnEncoding::Utf8 {
        return Ok(Position::new(number, offset - line.columns));
    }
    if !text.is_char_boundary(offset) {
        return Err(PositionError::InvalidUtf8Boundary);
    }
    let column = text[line.columns..offset]
        .chars()
        .map(|character| width(character, encoding))
        .sum();
    Ok(Position::new(number, column))
}

/// Every text of up to `length` pieces of `alphabet`.
fn texts(alphabet: &[&str], length: usize) -> Vec<String> {
    let mut all = vec![String::new()];
    let mut last = all.clone();
    for _ in 0..length {
        last = last
            .iter()
            .flat_map(|text| alphabet.iter().map(move |piece| format!("{text}{piece}")))
            .collect();
        all.extend(last.iter().cloned());
    }
    all
}

/// On every short text of line breaks, marks, and characters of each width, every offset has
/// the position that the text alone gives it, or the same reason for having none, and every
/// position on every line converts to the offset that has it.
#[test]
fn every_offset_and_every_position_of_a_short_text_convert_as_the_text_says() {
    let alphabet = ["a", "\n", "\r", "é", "😀", "\u{feff}", "\u{85}"];
    let mut all = texts(&alphabet, 4);
    all.extend([
        "a: 1\r\nb: 2\rc: 3\nd".to_owned(),
        "\u{feff}\u{feff}k: é\r\n\u{feff}  - 😀😀\n\n".to_owned(),
        "x".repeat(300) + "\r\n" + &"é".repeat(300),
        // Lines long enough that a column is found by counting whole blocks of bytes, with
        // characters of each width across the blocks' ends.
        "a".to_owned() + &"é".repeat(300) + "\n" + &"😀".repeat(150) + "\r\nab" + &"😀".repeat(70),
        "\u{feff}".to_owned() + &"\u{feff}é😀a".repeat(90),
    ]);
    let (mut offsets, mut positions) = (0_usize, 0_usize);
    for text in &all {
        let index = LineIndex::new(text);
        let lines = lines(text);
        assert_eq!(index.line_count(), lines.len(), "{text:?}");
        for encoding in ENCODINGS {
            // Each position that an offset has, with the first offset that has it.
            let mut placed = BTreeMap::new();
            for offset in 0..=text.len() + 2 {
                let expected = position_of(text, &lines, offset, encoding);
                assert_eq!(
                    index.to_position(offset, encoding),
                    expected,
                    "{text:?} at {offset} in {encoding:?}"
                );
                offsets += 1;
                if let Ok(position) = expected
                    && offset >= lines[position.line].columns
                {
                    placed.entry(position).or_insert(offset);
                }
            }
            for (number, line) in lines.iter().enumerate() {
                let content = &text[line.columns..line.end];
                let units: usize = match encoding {
                    ColumnEncoding::Utf8 => content.len(),
                    _ => content
                        .chars()
                        .map(|character| width(character, encoding))
                        .sum(),
                };
                for column in 0..=units + 2 {
                    let position = Position::new(number, column);
                    let expected = match placed.get(&position) {
                        Some(&offset) => Ok(offset),
                        None if column < units => Err(PositionError::SplitSurrogate),
                        None => Err(PositionError::OutOfBounds),
                    };
                    assert_eq!(
                        index.to_offset(position, encoding),
                        expected,
                        "{text:?} at {position:?} in {encoding:?}"
                    );
                    positions += 1;
                }
            }
            for line in lines.len()..lines.len() + 2 {
                assert_eq!(
                    index.to_offset(Position::new(line, 0), encoding),
                    Err(PositionError::OutOfBounds),
                    "{text:?} at line {line}"
                );
            }
        }
    }
    assert!(all.len() > 2_800, "{} texts", all.len());
    assert!(
        offsets > 50_000 && positions > 30_000,
        "{offsets} offsets, {positions} positions"
    );
}

/// The rules that the format contract gives the positions layer, each on its own.
#[test]
fn the_index_keeps_the_contract_s_rules_for_positions() {
    let at = |text: &str, offset, encoding| LineIndex::new(text).to_position(offset, encoding);
    // Offsets count a leading mark's bytes, and columns leave it out; an offset inside the
    // mark shares the first visible column, in every encoding.
    for encoding in ENCODINGS {
        for offset in 0..=3 {
            assert_eq!(
                at("\u{feff}a: 1\n", offset, encoding),
                Ok(Position::new(0, 0))
            );
        }
        assert_eq!(at("\u{feff}a: 1\n", 4, encoding), Ok(Position::new(0, 1)));
        // A line ending's first byte is the column after the line's last character, the byte
        // between a CR and its LF is no position, and a CR alone ends a line.
        assert_eq!(at("ab\r\ncd", 2, encoding), Ok(Position::new(0, 2)));
        assert_eq!(at("ab\r\ncd", 3, encoding), Err(PositionError::InsideCrLf));
        assert_eq!(at("ab\r\ncd", 4, encoding), Ok(Position::new(1, 0)));
        assert_eq!(at("ab\rcd", 3, encoding), Ok(Position::new(1, 0)));
        // The end of the input is a position, and nothing past it is.
        assert_eq!(at("ab\n", 3, encoding), Ok(Position::new(1, 0)));
        assert_eq!(at("ab\n", 4, encoding), Err(PositionError::OutOfBounds));
        // NEL, LS, and PS are content.
        assert_eq!(
            at("\u{85}\u{2028}\u{2029}x", 8, encoding).map(|position| position.line),
            Ok(0)
        );
    }
    // A byte column needs no character boundary; the other two do.
    assert_eq!(at("é\n", 1, ColumnEncoding::Utf8), Ok(Position::new(0, 1)));
    for encoding in [ColumnEncoding::Scalar, ColumnEncoding::Utf16] {
        assert_eq!(
            at("é\n", 1, encoding),
            Err(PositionError::InvalidUtf8Boundary)
        );
    }
    // A character outside the basic plane is one scalar value and two UTF-16 code units, and
    // no UTF-16 column stands between the two.
    let index = LineIndex::new("a😀b");
    assert_eq!(
        index.to_position(5, ColumnEncoding::Scalar),
        Ok(Position::new(0, 2))
    );
    assert_eq!(
        index.to_position(5, ColumnEncoding::Utf16),
        Ok(Position::new(0, 3))
    );
    assert_eq!(
        index.to_offset(Position::new(0, 3), ColumnEncoding::Utf16),
        Ok(5)
    );
    assert_eq!(
        index.to_offset(Position::new(0, 2), ColumnEncoding::Utf16),
        Err(PositionError::SplitSurrogate)
    );
    assert_eq!(
        index.to_offset(Position::new(0, 2), ColumnEncoding::Utf8),
        Ok(2)
    );
    // A line ending is not more columns, and a line that isn't there has no offset.
    let index = LineIndex::new("ab\r\ncd");
    assert_eq!(
        index.to_offset(Position::new(0, 2), ColumnEncoding::Utf8),
        Ok(2)
    );
    for column in [3, 4] {
        assert_eq!(
            index.to_offset(Position::new(0, column), ColumnEncoding::Utf8),
            Err(PositionError::OutOfBounds)
        );
    }
    assert_eq!(
        index.to_offset(Position::new(2, 0), ColumnEncoding::Utf8),
        Err(PositionError::OutOfBounds)
    );
    // Column 0 of the first line is the byte after the one mark that the columns leave out.
    // Every other mark is a character of its line: one that begins a later line, and a second
    // one at the start, which an editor shows as it shows any character.
    let index = LineIndex::new("\u{feff}a\n\u{feff}b");
    assert_eq!(
        index.to_offset(Position::new(0, 0), ColumnEncoding::Scalar),
        Ok(3)
    );
    assert_eq!(
        index.to_offset(Position::new(1, 0), ColumnEncoding::Scalar),
        Ok(5)
    );
    assert_eq!(
        index.to_offset(Position::new(1, 1), ColumnEncoding::Utf16),
        Ok(8)
    );
    assert_eq!(index.line_count(), 2);
    let index = LineIndex::new("\u{feff}\u{feff}A=1\n");
    assert_eq!(
        index.to_position(4, ColumnEncoding::Scalar),
        Err(PositionError::InvalidUtf8Boundary)
    );
    assert_eq!(
        index.to_position(6, ColumnEncoding::Scalar),
        Ok(Position::new(0, 1))
    );
    assert_eq!(
        index.to_offset(Position::new(0, 1), ColumnEncoding::Scalar),
        Ok(6)
    );
    // What an editor shows for a line that a mark begins: the `x` of the second line is at
    // UTF-16 column 4, and that position is its offset again.
    let text = "a: 1\n\u{feff}b: x\n";
    let index = LineIndex::new(text);
    let x = text.find('x').expect("an x");
    assert_eq!(
        index.to_position(x, ColumnEncoding::Utf16),
        Ok(Position::new(1, 4))
    );
    assert_eq!(
        index.to_offset(Position::new(1, 4), ColumnEncoding::Utf16),
        Ok(x)
    );
    assert_eq!(LineIndex::new("").line_count(), 1);
    assert_eq!(LineIndex::new("a\n").line_count(), 2);
    for reason in [
        PositionError::OutOfBounds,
        PositionError::InvalidUtf8Boundary,
        PositionError::SplitSurrogate,
        PositionError::InsideCrLf,
    ] {
        assert_ne!(reason.to_string(), "");
    }
}

/// An index is built once and read from many threads, and a clone of it reads the same.
#[test]
fn an_index_is_shared_between_threads() {
    fn shared<T: Send + Sync + Clone>() {}
    shared::<LineIndex<'_>>();
    shared::<Position>();
    shared::<Location>();
    shared::<PositionError>();
    let text = "a: 1\nb: é\n";
    let index = LineIndex::new(text);
    let copy = index.clone();
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                assert_eq!(
                    index.to_position(8, ColumnEncoding::Scalar),
                    Ok(Position::new(1, 3))
                );
            });
        }
    });
    assert_eq!(
        copy.to_position(8, ColumnEncoding::Utf8),
        index.to_position(8, ColumnEncoding::Utf8)
    );
}

/// A reader places an error where the index places its span's start, counted from one, on
/// every line that begins with no mark, so a caller can turn any span of such a document into
/// a place the same way. One mark that opens the input is such a line's too.
#[test]
fn a_reader_s_error_stands_where_the_index_puts_its_span() {
    let documents = [
        "a: [1, 2\n",
        "\u{feff}a: 1\r\nb: [\r\n",
        "\u{feff}a: [\n",
        "é: 1\né: 2\n",
        "- 😀\n- {a: 1, a: 2}\n",
        "key: 'unclosed\n",
        "a:\r  - 1\r  b: 2\r",
        "\u{feff}x: *missing\n",
        "a: 1\n\tb: 2\n",
        "- !!int text\n",
    ];
    let mut placed = 0;
    for document in documents {
        let index = LineIndex::new(document);
        for error in [
            ayaml::parse_value(document).err(),
            ayaml::from_str::<ayaml::Value>(document).err(),
            ayaml::from_str::<Vec<i64>>(document).err(),
            #[cfg(feature = "libyaml-compat")]
            ayaml::compat::from_str::<ayaml::Value>(document).err(),
        ]
        .into_iter()
        .flatten()
        {
            let (Some(span), Some(location)) = (error.span(), error.location()) else {
                continue;
            };
            let position = index.to_position(span.start(), ColumnEncoding::Scalar);
            assert_eq!(
                position.map(Location::from),
                Ok(location),
                "{document:?}: {error}"
            );
            placed += 1;
        }
    }
    assert!(placed >= 20, "{placed} errors were placed");

    // On a line that begins with a mark, past the one that opens the input, the readers leave
    // the mark out of the columns and the index counts it: one column apart.
    for (document, by) in [
        ("\u{feff}\u{feff}x: *missing\n", 1),
        ("\u{feff}\u{feff}\u{feff}x: *missing\n", 2),
        ("--- a\n...\n\u{feff}b: *missing\n", 1),
    ] {
        let error = ayaml::parse_value(document)
            .or_else(|_| ayaml::parse_documents(document).map(|_| ayaml::Value::Null))
            .expect_err("an error");
        let (Some(span), Some(location)) = (error.span(), error.location()) else {
            panic!("{document:?}: {error} has no place");
        };
        let position = index_position(document, span.start());
        assert_eq!(position.line(), location.line(), "{document:?}: {error}");
        assert_eq!(
            position.column(),
            location.column() + by,
            "{document:?}: {error}"
        );
    }
}

/// How many columns a reader's error stands from the index's position of its span's start, by
/// the rule that the index's documentation gives. `line` is the error's line from its first
/// byte, `before` the marks of that line that stand before the error, and `first` whether it
/// is the input's first line.
fn columns_apart(compat: bool, first: bool, line: &str, before: usize) -> isize {
    let mark = '\u{feff}';
    let marks = (line.len() - line.trim_start_matches(mark).len()) / mark.len_utf8();
    let before = isize::try_from(before).expect("a few marks");
    if compat {
        // The run that opens the input is left out but for the mark that the index leaves out
        // too, and one column is counted where a document marker follows the run.
        if !first || marks == 0 {
            return 0;
        }
        let rest = line.trim_start_matches(mark);
        let marker = (rest.starts_with("---") || rest.starts_with("..."))
            && rest[3..]
                .chars()
                .next()
                .is_none_or(|next| matches!(next, ' ' | '\t' | '\n' | '\r'));
        let counted = isize::from(marker && before == isize::try_from(marks).expect("few"));
        counted - (before - 1).max(0)
    } else if first {
        -(before - 1).max(0)
    } else {
        -before
    }
}

/// The rule holds for every error of every short text of marks, markers, breaks, and the
/// characters that make a document wrong, in each reader.
#[test]
fn a_reader_s_error_is_as_many_columns_from_the_index_as_the_rule_says() {
    let pieces = [
        "\u{feff}", "---", "...", " ", "\n", "x", ":", "[", "\"", "*a", "\t",
    ];
    #[cfg(feature = "libyaml-compat")]
    let compat_options = ayaml::compat::ParseOptions::new();
    let (mut placed, mut apart) = (0_usize, 0_usize);
    for text in texts(&pieces, 5) {
        let errors = [
            (false, ayaml::parse_value(&text).err()),
            (false, ayaml::parse_documents(&text).err()),
            (false, ayaml::from_str::<ayaml::Value>(&text).err()),
            (
                false,
                ayaml::parse_events(&text)
                    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                    .err(),
            ),
            #[cfg(feature = "libyaml-compat")]
            (true, ayaml::compat::from_str::<ayaml::Value>(&text).err()),
            #[cfg(feature = "libyaml-compat")]
            (
                true,
                ayaml::compat::parse_events_eager_with(&text, &compat_options).err(),
            ),
        ];
        for (compat, error) in errors {
            let Some(error) = error else {
                continue;
            };
            let (Some(span), Some(location)) = (error.span(), error.location()) else {
                panic!("{text:?}: {error} has no place");
            };
            let shown = index_position(&text, span.start());
            let line_start = text[..span.start()]
                .rfind(['\n', '\r'])
                .map_or(0, |at| at + 1);
            let line = &text[line_start..];
            let run = line.len() - line.trim_start_matches('\u{feff}').len();
            let before = (span.start() - line_start).min(run) / '\u{feff}'.len_utf8();
            let expected = columns_apart(compat, line_start == 0, line, before);
            assert_eq!(location.line(), shown.line(), "{text:?}: {error}");
            assert_eq!(
                isize::try_from(location.column()).expect("a column")
                    - isize::try_from(shown.column()).expect("a column"),
                expected,
                "{text:?} in {}: {error}",
                if compat { "compat" } else { "default" }
            );
            placed += 1;
            apart += usize::from(expected != 0);
        }
    }
    assert!(
        placed > 100_000 && apart > 5_000,
        "{placed} placed, {apart} apart"
    );
}

fn index_position(document: &str, offset: usize) -> Location {
    LineIndex::new(document)
        .to_position(offset, ColumnEncoding::Scalar)
        .map(Location::from)
        .expect("a position")
}
