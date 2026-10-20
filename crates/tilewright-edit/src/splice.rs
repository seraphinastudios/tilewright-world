// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Lossless edits of one JSONC document.
//!
//! An array's items, or an object's members, sit between separator
//! regions: `[R0 item0 R1 item1 ... Rn]`, where the inner regions hold the
//! comma and every region holds only whitespace and comments. Inserting an
//! item replaces one region with `before + item + after`; removing one
//! replaces its two neighbouring regions and the item with one joined
//! region. Every other byte is kept, and recording the replaced text makes
//! each edit's inverse exact.

use tilewright_jsonc::{Span, Value};

/// A step of a resolved pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Key(String),
    Index(usize),
}

/// The value at `pointer`, if there is one.
pub(crate) fn find<'v>(root: &'v Value, pointer: &[Step]) -> Option<&'v Value> {
    pointer.iter().try_fold(root, |value, step| match step {
        Step::Key(key) => value.get(key),
        Step::Index(index) => value.as_array()?.get(*index),
    })
}

/// `source` with `span` replaced by `replacement`.
pub(crate) fn splice(source: &str, span: Span, replacement: &str) -> String {
    let mut text = String::with_capacity(source.len() - span.len() + replacement.len());
    text.push_str(&source[..span.start]);
    text.push_str(replacement);
    text.push_str(&source[span.end..]);
    text
}

/// `source` with every span replaced. Spans are disjoint.
pub(crate) fn splice_all(source: &str, edits: &mut [(Span, String)]) -> String {
    edits.sort_by_key(|(span, _)| span.start);
    let mut text = String::with_capacity(source.len());
    let mut position = 0;
    for (span, replacement) in edits.iter() {
        text.push_str(&source[position..span.start]);
        text.push_str(replacement);
        position = span.end;
    }
    text.push_str(&source[position..]);
    text
}

fn is_blank_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether `text` is only whitespace.
pub(crate) fn is_blank(text: &str) -> bool {
    text.bytes().all(is_blank_byte)
}

/// The offset of each `delimiter` in `text` outside comments, if `text` is
/// whitespace, complete comments and delimiters only. A line comment must
/// end within the text, so it cannot swallow what follows.
fn delimiters(text: &str, delimiter: u8) -> Option<Vec<usize>> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte if is_blank_byte(byte) => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += text[index..].find('\n')? + 1;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += text[index + 2..].find("*/")? + 4;
            }
            byte if byte == delimiter => {
                found.push(index);
                index += 1;
            }
            _ => return None,
        }
    }
    Some(found)
}

/// Whether `text` is whitespace and complete comments around exactly
/// `count` `delimiter` bytes.
pub(crate) fn is_trivia(text: &str, delimiter: u8, count: usize) -> bool {
    delimiters(text, delimiter).is_some_and(|found| found.len() == count)
}

/// A region with one comma, split around it.
fn around_comma(region: &str) -> (&str, &str) {
    match delimiters(region, b',').as_deref() {
        Some([comma]) => (&region[..*comma], &region[comma + 1..]),
        _ => (region, ""),
    }
}

/// The whitespace that puts an item on its own line like the item after
/// `region`: a newline and that item's indentation, or a space, or nothing.
fn line_style(region: &str) -> String {
    if let Some(last) = region.rfind('\n') {
        let newline = if region[..last].ends_with('\r') {
            "\r\n"
        } else {
            "\n"
        };
        let indent: String = region[last + 1..]
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .take(MAXIMUM_INDENTATION)
            .collect();
        format!("{newline}{indent}")
    } else if region.ends_with([' ', '\t']) {
        " ".to_owned()
    } else {
        String::new()
    }
}

/// Longest indentation new text copies, so that a document indented
/// without bound cannot multiply the size of what is written into it.
const MAXIMUM_INDENTATION: usize = 64;

/// The indentation of the line that holds `offset`, at most
/// [`MAXIMUM_INDENTATION`] characters.
fn indentation(source: &str, offset: usize) -> String {
    let start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    source[start..]
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .take(MAXIMUM_INDENTATION)
        .collect()
}

/// The indentation for a new item at `index` of `container`: that of a
/// neighbouring item, or one level deeper than the container's line.
pub(crate) fn item_indentation(source: &str, container: &Value, index: usize) -> String {
    match Sequence::of(container) {
        Some(sequence) if sequence.len() > 0 => {
            let item = sequence.items[index.min(sequence.len() - 1)];
            indentation(source, item.start)
        }
        _ => format!("{}  ", indentation(source, container.span().start)),
    }
}

/// The items of an array or the members of an object, and the regions
/// between them.
pub(crate) struct Sequence {
    /// Just after the opening bracket.
    start: usize,
    /// At the closing bracket.
    end: usize,
    /// Item spans; a member runs from its key to the end of its value.
    items: Vec<Span>,
    /// For members, each key's span and each value's span.
    keys: Vec<Span>,
    values: Vec<Span>,
    pub object: bool,
}

impl Sequence {
    pub fn of(value: &Value) -> Option<Self> {
        let span = value.span();
        let (items, keys, values, object) = match value {
            Value::Array { items, .. } => {
                let spans: Vec<Span> = items.iter().map(Value::span).collect();
                (spans.clone(), Vec::new(), spans, false)
            }
            Value::Object { members, .. } => (
                members
                    .iter()
                    .map(|member| Span {
                        start: member.key_span.start,
                        end: member.value.span().end,
                    })
                    .collect(),
                members.iter().map(|member| member.key_span).collect(),
                members.iter().map(|member| member.value.span()).collect(),
                true,
            ),
            _ => return None,
        };
        Some(Self {
            start: span.start + 1,
            end: span.end - 1,
            items,
            keys,
            values,
            object,
        })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The region before item `index`, or after the last item when `index`
    /// is the length.
    pub fn region(&self, index: usize) -> Span {
        Span {
            start: if index == 0 {
                self.start
            } else {
                self.items[index - 1].end
            },
            end: self.items.get(index).map_or(self.end, |item| item.start),
        }
    }

    /// The text between the first member's key and value, if it is plain
    /// (`": "`, `":"`); otherwise `": "`.
    fn colon<'s>(&self, source: &'s str) -> &'s str {
        match (self.keys.first(), self.values.first()) {
            (Some(key), Some(value)) => {
                let colon = &source[key.end..value.start];
                if colon
                    .bytes()
                    .all(|byte| byte == b':' || is_blank_byte(byte))
                    && is_trivia(colon, b':', 1)
                {
                    colon
                } else {
                    ": "
                }
            }
            _ => ": ",
        }
    }
}

/// The separator text around an inserted item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub before: String,
    pub after: String,
}

impl Layout {
    /// Whether this layout fits an insertion at `index` of `length` items:
    /// `before` holds a comma unless the item is first, and `after` one
    /// unless it is last.
    pub fn fits(&self, index: usize, length: usize) -> bool {
        is_trivia(&self.before, b',', usize::from(index > 0))
            && is_trivia(&self.after, b',', usize::from(index < length))
    }
}

/// The layout of an insertion at `index` when none is given: the new item
/// takes the line style of its neighbours, and comments stay where they
/// are.
fn inferred_layout(source: &str, sequence: &Sequence, index: usize) -> Layout {
    let length = sequence.len();
    let region = |at: usize| &source[sequence.region(at).start..sequence.region(at).end];
    if length == 0 {
        let inner = region(0);
        return Layout {
            before: if is_blank(inner) {
                String::new()
            } else {
                inner.to_owned()
            },
            after: String::new(),
        };
    }
    // The style between two items, or with one item, its line style if
    // the container spans lines and a space if it does not.
    let style = if length >= 2 {
        line_style(region(1))
    } else if region(0).contains('\n') {
        line_style(region(0))
    } else {
        " ".to_owned()
    };
    if index == 0 {
        Layout {
            before: region(0).to_owned(),
            after: format!(",{style}"),
        }
    } else if index < length {
        let (before_comma, after_comma) = around_comma(region(index));
        Layout {
            before: format!("{before_comma},{style}"),
            after: format!(",{after_comma}"),
        }
    } else {
        Layout {
            before: format!(",{style}"),
            after: region(length).to_owned(),
        }
    }
}

/// Whether `separator` fits a removal at `index` of `length` items: it
/// holds a comma only when the item has neighbours on both sides.
pub(crate) fn separator_fits(separator: &str, index: usize, length: usize) -> bool {
    is_trivia(
        separator,
        b',',
        usize::from(index > 0 && index + 1 < length),
    )
}

/// The region that replaces item `index` and its neighbouring regions when
/// no separator is given. The comments between the item and the commas
/// that separate it from its neighbours go with it; others stay.
fn inferred_separator(source: &str, sequence: &Sequence, index: usize) -> String {
    let length = sequence.len();
    let region = |at: usize| &source[sequence.region(at).start..sequence.region(at).end];
    let (before, after) = (region(index), region(index + 1));
    if length == 1 {
        if is_blank(before) && is_blank(after) {
            String::new()
        } else {
            format!("{before}{after}")
        }
    } else if index == 0 {
        let (_, after_comma) = around_comma(after);
        format!(
            "{before}{}",
            after_comma.trim_start_matches([' ', '\t', '\n', '\r'])
        )
    } else if index + 1 == length {
        let (before_comma, _) = around_comma(before);
        format!("{before_comma}{after}")
    } else {
        let (before_comma, _) = around_comma(before);
        let (_, after_comma) = around_comma(after);
        format!("{before_comma},{after_comma}")
    }
}

/// An item to insert. Texts are inserted exactly as given.
pub(crate) struct Insertion<'a> {
    /// For a member: its decoded key and the key's JSON string text.
    pub key: Option<(&'a str, &'a str)>,
    pub value: &'a str,
    pub layout: Option<Layout>,
    /// The text between a member's key and value.
    pub colon: Option<&'a str>,
}

/// What an insertion replaced, for its inverse.
pub(crate) struct Inserted {
    pub text: String,
    /// The region the new item and its layout replaced.
    pub region: String,
}

/// Inserts an item at `index` of the array or object `container`.
pub(crate) fn insert(
    source: &str,
    container: &Value,
    index: usize,
    insertion: &Insertion<'_>,
) -> Result<Inserted, String> {
    let sequence = Sequence::of(container).ok_or("the container is not an array or object")?;
    if index > sequence.len() {
        return Err(format!(
            "index {index} is past the end of {} item(s)",
            sequence.len()
        ));
    }
    let item = match (sequence.object, insertion.key) {
        (true, Some((key, key_text))) => {
            if container.get(key).is_some() {
                return Err(format!(
                    "the object already has the key {}",
                    crate::diagnostic::quoted(key)
                ));
            }
            let colon = insertion.colon.unwrap_or_else(|| sequence.colon(source));
            if !is_trivia(colon, b':', 1) {
                return Err("colon must be whitespace and comments around one \":\"".to_owned());
            }
            format!("{key_text}{colon}{}", insertion.value)
        }
        (false, None) => insertion.value.to_owned(),
        (true, None) => return Err("an object member needs a key".to_owned()),
        (false, Some(_)) => return Err("an array item has no key".to_owned()),
    };
    let layout = match &insertion.layout {
        Some(layout) if layout.fits(index, sequence.len()) => layout.clone(),
        Some(_) => {
            return Err(
                "before and after must be whitespace and comments, with the separating commas"
                    .to_owned(),
            );
        }
        None => inferred_layout(source, &sequence, index),
    };
    let region = sequence.region(index);
    let replacement = format!("{}{item}{}", layout.before, layout.after);
    Ok(Inserted {
        text: splice(source, region, &replacement),
        region: source[region.start..region.end].to_owned(),
    })
}

/// What a removal took out, for its inverse.
pub(crate) struct Removed {
    pub text: String,
    /// The member's key text and the text between key and value.
    pub key: Option<(String, String)>,
    pub value: String,
    /// The regions before and after the item.
    pub layout: Layout,
}

/// Removes item `index` of the array or object `container`.
pub(crate) fn remove(
    source: &str,
    container: &Value,
    index: usize,
    separator: Option<&str>,
) -> Result<Removed, String> {
    let sequence = Sequence::of(container).ok_or("the container is not an array or object")?;
    let length = sequence.len();
    if index >= length {
        return Err(format!("index {index} is past the end of {length} item(s)"));
    }
    let joined = match separator {
        Some(separator) if separator_fits(separator, index, length) => separator.to_owned(),
        Some(_) => {
            return Err(
                "separator must be whitespace and comments, with a comma only between two items"
                    .to_owned(),
            );
        }
        None => inferred_separator(source, &sequence, index),
    };
    let (before, after) = (sequence.region(index), sequence.region(index + 1));
    let value = sequence.values[index];
    let key = sequence.object.then(|| {
        let key = sequence.keys[index];
        (
            source[key.start..key.end].to_owned(),
            source[key.end..value.start].to_owned(),
        )
    });
    Ok(Removed {
        text: splice(
            source,
            Span {
                start: before.start,
                end: after.end,
            },
            &joined,
        ),
        key,
        value: source[value.start..value.end].to_owned(),
        layout: Layout {
            before: source[before.start..before.end].to_owned(),
            after: source[after.start..after.end].to_owned(),
        },
    })
}
