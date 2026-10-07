//! Bounded validation of the backend's canonical, info-only v1 metainfo wrapper.
//! Byte slices stay private; errors never contain dictionary keys or paths.
use crate::CoreError;
use sha1::{Digest, Sha1};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

type Result<T> = std::result::Result<T, CoreError>;
enum Value<'a> {
    Bytes(&'a [u8]),
    Integer(u64),
    List(Vec<Value<'a>>),
    Dictionary(Vec<(&'a [u8], Value<'a>)>),
}
struct Parser<'a> {
    bytes: &'a [u8],
    offset: usize,
    values: usize,
}
fn invalid() -> CoreError {
    CoreError::InvalidInput
}
impl<'a> Parser<'a> {
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let start = self.offset;
        while self.bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
            self.offset += 1;
        }
        let token = self.bytes.get(start..self.offset).ok_or_else(invalid)?;
        if token.is_empty()
            || (token.len() > 1 && token[0] == b'0')
            || self.bytes.get(self.offset) != Some(&b':')
        {
            return Err(invalid());
        }
        let size: usize = std::str::from_utf8(token)
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())?;
        if size > 4_194_304 {
            return Err(invalid());
        }
        self.offset += 1;
        let end = self.offset.checked_add(size).ok_or_else(invalid)?;
        let data = self.bytes.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        Ok(data)
    }
    fn value(&mut self, depth: usize) -> Result<Value<'a>> {
        self.values += 1;
        if depth > 32 || self.values > 65_536 {
            return Err(invalid());
        }
        match self.bytes.get(self.offset).copied() {
            Some(b'0'..=b'9') => self.bytes().map(Value::Bytes),
            Some(b'i') => {
                self.offset += 1;
                let start = self.offset;
                while self.bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
                    self.offset += 1;
                }
                let token = &self.bytes[start..self.offset];
                if token.is_empty()
                    || (token.len() > 1 && token[0] == b'0')
                    || self.bytes.get(self.offset) != Some(&b'e')
                {
                    return Err(invalid());
                }
                self.offset += 1;
                Ok(Value::Integer(
                    std::str::from_utf8(token)
                        .map_err(|_| invalid())?
                        .parse()
                        .map_err(|_| invalid())?,
                ))
            }
            Some(b'l') => {
                self.offset += 1;
                let mut list = Vec::new();
                while self.bytes.get(self.offset) != Some(&b'e') {
                    list.push(self.value(depth + 1)?);
                }
                self.offset += 1;
                Ok(Value::List(list))
            }
            Some(b'd') => {
                self.offset += 1;
                let mut map = Vec::new();
                let mut previous: Option<&[u8]> = None;
                while self.bytes.get(self.offset) != Some(&b'e') {
                    self.values += 1;
                    if self.values > 65_536 {
                        return Err(invalid());
                    }
                    let key = self.bytes()?;
                    if previous.is_some_and(|p| p >= key) {
                        return Err(invalid());
                    }
                    previous = Some(key);
                    map.push((key, self.value(depth + 1)?));
                }
                self.offset += 1;
                Ok(Value::Dictionary(map))
            }
            _ => Err(invalid()),
        }
    }
}
fn dictionary<'a>(v: &'a Value<'a>) -> Result<&'a [(&'a [u8], Value<'a>)]> {
    match v {
        Value::Dictionary(m) => Ok(m),
        _ => Err(invalid()),
    }
}
fn field<'a>(map: &'a [(&'a [u8], Value<'a>)], key: &[u8]) -> Result<&'a Value<'a>> {
    map.iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
        .ok_or_else(invalid)
}
fn optional<'a>(map: &'a [(&'a [u8], Value<'a>)], key: &[u8]) -> Option<&'a Value<'a>> {
    map.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}
fn integer(v: &Value<'_>) -> Result<u64> {
    match v {
        Value::Integer(n) => Ok(*n),
        _ => Err(invalid()),
    }
}
fn bytes<'a>(v: &'a Value<'a>) -> Result<&'a [u8]> {
    match v {
        Value::Bytes(b) => Ok(b),
        _ => Err(invalid()),
    }
}
fn component(v: &Value<'_>) -> Result<String> {
    let s = std::str::from_utf8(bytes(v)?).map_err(|_| invalid())?;
    if s.is_empty()
        || matches!(s, "." | "..")
        || s.ends_with('.')
        || s.ends_with(' ')
        || s.chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(invalid());
    }
    Ok(s.nfc().collect())
}
fn path(v: &Value<'_>) -> Result<Vec<String>> {
    match v {
        Value::List(list) if !list.is_empty() => list.iter().map(component).collect(),
        _ => Err(invalid()),
    }
}
fn equivalent_component(map: &[(&[u8], Value<'_>)], key: &[u8], utf8: &[u8]) -> Result<String> {
    let name = component(field(map, key)?)?;
    if optional(map, utf8)
        .map(component)
        .transpose()?
        .is_some_and(|s| s != name)
    {
        return Err(invalid());
    }
    Ok(name)
}
pub(crate) fn validate(data: &[u8], hash: &str, index: u32, expected: Option<u64>) -> Result<()> {
    let mut parser = Parser {
        bytes: data,
        offset: 0,
        values: 0,
    };
    // Canonical backend emission strips every outer annotation/tracker field.
    if !data.starts_with(b"d4:info") {
        return Err(invalid());
    }
    parser.offset = 7;
    let start = parser.offset;
    let info = parser.value(1)?;
    let end = parser.offset;
    if data.get(end..) != Some(b"e") || format!("{:x}", Sha1::digest(&data[start..end])) != hash {
        return Err(invalid());
    }
    let map = dictionary(&info)?;
    let keys: &[&[u8]] = &[
        b"name",
        b"name.utf-8",
        b"piece length",
        b"pieces",
        b"length",
        b"files",
    ];
    if map.iter().any(|(k, _)| !keys.contains(k)) {
        return Err(invalid());
    }
    equivalent_component(map, b"name", b"name.utf-8")?;
    let piece_length = integer(field(map, b"piece length")?)?;
    if !(16_384..=16_777_216).contains(&piece_length) || !piece_length.is_power_of_two() {
        return Err(invalid());
    }
    let pieces = bytes(field(map, b"pieces")?)?;
    let mut lengths = Vec::new();
    match (optional(map, b"length"), optional(map, b"files")) {
        (Some(length), None) => lengths.push(integer(length)?),
        (None, Some(Value::List(files))) if !files.is_empty() && files.len() <= 4096 => {
            let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
            for file in files {
                let file = dictionary(file)?;
                if file
                    .iter()
                    .any(|(k, _)| ![b"length".as_slice(), b"path", b"path.utf-8"].contains(k))
                {
                    return Err(invalid());
                }
                let name = path(field(file, b"path")?)?;
                let collision_name: Vec<String> = name
                    .iter()
                    .map(|part| part.to_lowercase().nfc().collect())
                    .collect();
                if optional(file, b"path.utf-8")
                    .map(path)
                    .transpose()?
                    .is_some_and(|p| p != name)
                    || paths
                        .iter()
                        .any(|p| p.starts_with(&collision_name) || collision_name.starts_with(p))
                {
                    return Err(invalid());
                }
                paths.insert(collision_name);
                lengths.push(integer(field(file, b"length")?)?);
            }
        }
        _ => return Err(invalid()),
    }
    if lengths.contains(&0) {
        return Err(invalid());
    }
    let total = lengths
        .iter()
        .try_fold(0u64, |sum, n| sum.checked_add(*n))
        .ok_or_else(invalid)?;
    // Full payload must fit the fixed native reservation, including unselected files.
    if total > 2_147_483_648 {
        return Err(invalid());
    }
    let count = total.checked_add(piece_length - 1).ok_or_else(invalid)? / piece_length;
    if count.checked_mul(20) != Some(pieces.len() as u64) {
        return Err(invalid());
    }
    let selected = *lengths.get(index as usize).ok_or_else(invalid)?;
    if expected.is_some_and(|n| n != selected) {
        return Err(invalid());
    }
    Ok(())
}
