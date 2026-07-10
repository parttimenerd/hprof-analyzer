/// Variable-length integer encoding (7 bits per byte, MSB = continuation flag).
/// Used for delta-encoding sorted inbound edge lists to reduce memory ~4x.

pub fn encode(mut v: u32, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

pub fn decode_one(buf: &[u8]) -> (u32, usize) {
    let mut val = 0u32;
    let mut shift = 0u32;
    for (i, &b) in buf.iter().enumerate() {
        val |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 == 0 {
            return (val, i + 1);
        }
        shift += 7;
    }
    (val, buf.len())
}

pub fn decode_all(buf: &[u8]) -> Vec<u32> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let (v, consumed) = decode_one(&buf[i..]);
        result.push(v);
        i += consumed;
    }
    result
}

/// Encode a sorted slice as delta-from-previous values.
pub fn encode_delta(sorted: &[u32], out: &mut Vec<u8>) {
    let mut prev = 0u32;
    for &v in sorted {
        encode(v - prev, out);
        prev = v;
    }
}

/// Decode delta-encoded slice, returning original sorted values.
pub fn decode_delta(buf: &[u8], count: usize) -> Vec<u32> {
    let mut result = Vec::with_capacity(count);
    let mut prev = 0u32;
    let mut i = 0;
    while i < buf.len() && result.len() < count {
        let (delta, consumed) = decode_one(&buf[i..]);
        prev += delta;
        result.push(prev);
        i += consumed;
    }
    result
}

/// Encoded byte length for a u32 value.
pub fn encoded_len(mut v: u32) -> usize {
    let mut n = 1;
    v >>= 7;
    while v > 0 { n += 1; v >>= 7; }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_values() {
        let vals: Vec<u32> = vec![0, 1, 63, 64, 127, 128, 255, 300, 16383, 16384, u32::MAX];
        let mut buf = Vec::new();
        for &v in &vals { encode(v, &mut buf); }
        assert_eq!(decode_all(&buf), vals);
    }

    #[test]
    fn delta_roundtrip() {
        let sorted = vec![5u32, 10, 15, 100, 200, 201, 1000, 100_000];
        let mut buf = Vec::new();
        encode_delta(&sorted, &mut buf);
        assert_eq!(decode_delta(&buf, sorted.len()), sorted);
    }

    #[test]
    fn single_byte_values() {
        // values 0..127 should encode in exactly 1 byte each
        for v in 0u32..128 {
            let mut buf = Vec::new();
            encode(v, &mut buf);
            assert_eq!(buf.len(), 1, "v={v}");
            assert_eq!(decode_one(&buf), (v, 1));
        }
    }

    #[test]
    fn two_byte_boundary() {
        // 128 needs 2 bytes
        let mut buf = Vec::new();
        encode(128, &mut buf);
        assert_eq!(buf.len(), 2);
        assert_eq!(decode_one(&buf), (128, 2));
    }

    #[test]
    fn encoded_len_check() {
        assert_eq!(encoded_len(0), 1);
        assert_eq!(encoded_len(127), 1);
        assert_eq!(encoded_len(128), 2);
        assert_eq!(encoded_len(16383), 2);
        assert_eq!(encoded_len(16384), 3);
    }

    #[test]
    fn empty_delta() {
        let mut buf = Vec::new();
        encode_delta(&[], &mut buf);
        assert!(buf.is_empty());
        assert_eq!(decode_delta(&buf, 0), vec![]);
    }
}
