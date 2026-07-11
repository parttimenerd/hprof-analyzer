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

/// Variable-length encode a u64 (7 bits per byte, MSB = continuation).
pub fn encode_u64(mut v: u64, out: &mut Vec<u8>) {
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

/// Decode one vbyte u64 from the front of `buf`, returning (value, bytes_read).
pub fn decode_one_u64(buf: &[u8]) -> (u64, usize) {
    let mut val = 0u64;
    let mut shift = 0u32;
    for (i, &b) in buf.iter().enumerate() {
        val |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return (val, i + 1);
        }
        shift += 7;
    }
    (val, buf.len())
}

/// Encode a sorted u64 slice as vbyte delta-from-previous values.
pub fn encode_delta_u64(sorted: &[u64], out: &mut Vec<u8>) {
    let mut prev = 0u64;
    for &v in sorted {
        encode_u64(v - prev, out);
        prev = v;
    }
}

/// Decode a vbyte delta-encoded u64 stream back into the original sorted values.
pub fn decode_delta_u64(buf: &[u8], count: usize) -> Vec<u64> {
    let mut result = Vec::with_capacity(count);
    let mut prev = 0u64;
    let mut i = 0;
    while i < buf.len() && result.len() < count {
        let (delta, consumed) = decode_one_u64(&buf[i..]);
        prev += delta;
        result.push(prev);
        i += consumed;
    }
    result
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

    #[test]
    fn u64_roundtrip_values() {
        let vals: Vec<u64> = vec![
            0, 1, 127, 128, 255, 16383, 16384, u32::MAX as u64,
            1u64 << 40, u64::MAX,
        ];
        let mut buf = Vec::new();
        for &v in &vals {
            encode_u64(v, &mut buf);
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            let (v, c) = decode_one_u64(&buf[i..]);
            out.push(v);
            i += c;
        }
        assert_eq!(out, vals);
    }

    #[test]
    fn u64_delta_roundtrip() {
        let sorted: Vec<u64> = vec![
            0x1000, 0x1010, 0x1028, 0x1040, 0x5000, 0x5000_0000, 0x5000_0018,
        ];
        let mut buf = Vec::new();
        encode_delta_u64(&sorted, &mut buf);
        assert_eq!(decode_delta_u64(&buf, sorted.len()), sorted);
    }

    #[test]
    fn u64_empty_delta() {
        let mut buf = Vec::new();
        encode_delta_u64(&[], &mut buf);
        assert!(buf.is_empty());
        assert_eq!(decode_delta_u64(&buf, 0), Vec::<u64>::new());
    }
}
