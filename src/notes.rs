//! Notes (`nt`) decoding.
//!
//! A note is either a full-text object:
//!
//! ```json
//! {"_t":"tx","ch":75990805,"v":"正文","t":1}
//! ```
//!
//! or a byte-offset patch against the previous text:
//!
//! ```json
//! {"t":2,"ps":[{"p":0,"l":53,"r":"新文"}]}
//! ```
//!
//! `p`/`l` are offsets into the *UTF-8 bytes* of the old text, not characters.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Patch {
    #[serde(default)]
    pub p: i64,
    #[serde(default)]
    pub l: i64,
    #[serde(default)]
    pub r: String,
}

/// Decode an `nt` value. `previous` is the text currently known locally and is
/// required to apply patches; when a patch cannot be applied we return `None`
/// rather than writing a truncated note.
pub fn decode(value: &Value, previous: &str) -> Option<String> {
    // Defensive: a bare string is invalid on the wire (it crashes the Mac app),
    // but we should still not lose data when reading it.
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }

    let obj = value.as_object()?;

    if let Some(patches) = obj.get("ps").and_then(Value::as_array) {
        let parsed: Vec<Patch> = patches
            .iter()
            .filter_map(|p| serde_json::from_value(p.clone()).ok())
            .collect();
        if parsed.len() != patches.len() {
            return None;
        }
        return apply_patches(previous, &parsed);
    }

    obj.get("v").and_then(Value::as_str).map(str::to_string)
}

/// Apply patches in order, each against the current text, using byte offsets.
pub fn apply_patches(base: &str, patches: &[Patch]) -> Option<String> {
    let mut current = base.to_string();

    for patch in patches {
        if patch.p < 0 || patch.l < 0 {
            return None;
        }
        let start = patch.p as usize;
        let end = start.checked_add(patch.l as usize)?;
        let bytes = current.as_bytes();
        if end > bytes.len() {
            return None;
        }
        if !current.is_char_boundary(start) || !current.is_char_boundary(end) {
            // Byte offset lands inside a multi-byte character: refuse to guess.
            return None;
        }

        let mut next = String::with_capacity(bytes.len() - (end - start) + patch.r.len());
        next.push_str(&current[..start]);
        next.push_str(&patch.r);
        next.push_str(&current[end..]);
        current = next;
    }

    Some(current)
}

/// CRC32 of the UTF-8 bytes of `text`, as used by the `ch` field.
pub fn checksum(text: &str) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(text.as_bytes());
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_full_text() {
        let v = json!({"_t":"tx","ch":75990805,"v":"Mac端捕获测试\n第二行","t":1});
        assert_eq!(decode(&v, "").unwrap(), "Mac端捕获测试\n第二行");
        assert_eq!(checksum("Mac端捕获测试\n第二行"), 75990805);
    }

    #[test]
    fn applies_byte_offset_patch() {
        // "第一行正文\n第二行 emoji 😀 中文\n第三行" is 24 chars / 53 bytes.
        let old = "第一行正文\n第二行 emoji 😀 中文\n第三行";
        assert_eq!(old.chars().count(), 24);
        assert_eq!(old.len(), 53);

        let v = json!({"t":2,"ps":[{"p":0,"l":53,"r":"新文"}]});
        assert_eq!(decode(&v, old).unwrap(), "新文");
    }

    #[test]
    fn refuses_patch_inside_multibyte_char() {
        let v = json!({"t":2,"ps":[{"p":1,"l":1,"r":"x"}]});
        assert!(decode(&v, "中").is_none());
    }

    #[test]
    fn refuses_patch_without_base() {
        let v = json!({"t":2,"ps":[{"p":0,"l":5,"r":"x"}]});
        assert!(decode(&v, "").is_none());
    }
}
