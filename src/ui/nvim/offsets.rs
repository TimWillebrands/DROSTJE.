//! Conversions between UTF-8 byte offsets, UTF-16 code units and buffer positions.
use std::ops::Range;

pub(super) fn byte_floor(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

pub(super) fn utf16_byte(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

pub(super) fn utf16_floor(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units + character.len_utf16() > offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

pub(super) fn utf16_ceil(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units >= offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

pub(super) fn utf16_offset(text: &str, byte: usize) -> usize {
    text[..byte_floor(text, byte)].encode_utf16().count()
}

pub(super) fn exact_utf16_range(text: &str, range: &Range<usize>) -> Option<Range<usize>> {
    if range.start > range.end {
        return None;
    }
    Some(utf16_byte(text, range.start)?..utf16_byte(text, range.end)?)
}

pub(super) fn buffer_offset(text: &str, cursor: (usize, usize)) -> usize {
    let mut start = 0;
    for _ in 0..cursor.0 {
        let Some(newline) = text[start..].find('\n') else {
            return text.len();
        };
        start += newline + 1;
    }
    let length = text[start..].find('\n').unwrap_or(text.len() - start);
    start + byte_floor(&text[start..start + length], cursor.1)
}
