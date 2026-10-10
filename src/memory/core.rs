// op 0 - delete, op 1 - put
pub fn encode_internal_key(user_key: &[u8], lsn: u64, op_type: u8) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(user_key.len() + 8 + 1);

    encoded.extend_from_slice(user_key);

    encoded.extend_from_slice(&(u64::MAX - lsn).to_be_bytes());

    encoded.push(op_type);

    encoded
}
