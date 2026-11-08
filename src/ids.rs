//! Entity identifiers.
//!
//! Things Cloud uses base58-encoded 16-byte identifiers on the wire.

/// Generate a new random base58 entity id.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system RNG unavailable");
    bs58::encode(bytes).into_string()
}

/// Generate a random RFC-4122 v4 UUID string (used for `App-Instance-Id`).
pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("operating system RNG unavailable");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;

    let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Loose validity check for a Things entity id.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && bs58::decode(id).into_vec().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_roundtrip() {
        for _ in 0..16 {
            let id = new_id();
            assert!(is_valid_id(&id), "{id} should be valid");
            assert_eq!(bs58::decode(&id).into_vec().unwrap().len(), 16);
        }
    }

    #[test]
    fn uuid_shape() {
        let id = new_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[8], b'-');
    }
}
