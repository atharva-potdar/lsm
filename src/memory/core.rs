#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueResult {
    Found(Vec<u8>),
    Deleted,
    NotFound,
}

// op 0 - delete, op 1 - put
pub fn encode_internal_key(user_key: &[u8], lsn: u64, op_type: u8) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(user_key.len() + 8 + 1);

    encoded.extend_from_slice(user_key);

    encoded.extend_from_slice(&(u64::MAX - lsn).to_be_bytes());

    encoded.push(op_type);

    encoded
}

/// Custom internal key comparator:
/// Compares user keys first, and if identical, compares the 9-byte trailer
/// (8-byte inverted LSN followed by 1-byte `op_type`).
pub fn cmp_internal_key(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    if a.len() < 9 || b.len() < 9 {
        return a.cmp(b);
    }
    let user_key_a = &a[..a.len() - 9];
    let user_key_b = &b[..b.len() - 9];
    match user_key_a.cmp(user_key_b) {
        std::cmp::Ordering::Equal => a[a.len() - 9..].cmp(&b[b.len() - 9..]),
        ord => ord,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmp_internal_key_prefix() {
        // Key A: [0x01] at LSN 0 (trailer is all 0xFF)
        let key_a = encode_internal_key(&[0x01], 0, 1);
        // Key B: [0x01, 0x02] at LSN 100
        let key_b = encode_internal_key(&[0x01, 0x02], 100, 1);

        // Native slice cmp would erroneously say key_a > key_b because 0xFF > 0x02:
        assert!(key_a.as_slice() > key_b.as_slice());

        // cmp_internal_key correctly identifies that [0x01] < [0x01, 0x02]:
        assert_eq!(cmp_internal_key(&key_a, &key_b), std::cmp::Ordering::Less);
        assert_eq!(cmp_internal_key(&key_b, &key_a), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_cmp_internal_key_same_user_key_lsn_order() {
        // Same user key: higher LSN must sort before lower LSN (smaller inverted trailer)
        let newer = encode_internal_key(b"apple", 100, 1);
        let older = encode_internal_key(b"apple", 50, 1);
        assert_eq!(cmp_internal_key(&newer, &older), std::cmp::Ordering::Less);
        assert_eq!(cmp_internal_key(&older, &newer), std::cmp::Ordering::Greater);

        // Put vs Delete on identical user key and LSN: op 0 (delete) sorts before op 1 (put)
        let delete = encode_internal_key(b"apple", 50, 0);
        assert_eq!(cmp_internal_key(&delete, &older), std::cmp::Ordering::Less);
    }
}
