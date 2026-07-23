// Compress a page of i32 into MAT ArrayIntCompressed bytes.
pub fn compress_int(vals: &[i32]) -> Vec<u8> {
    compress_impl(vals.iter().map(|&v| v as u32 as u64), 32)
}
pub fn compress_long(vals: &[i64]) -> Vec<u8> {
    compress_impl(vals.iter().map(|&v| v as u64), 64)
}

fn compress_impl(iter: impl Iterator<Item = u64> + Clone, total_bits: u32) -> Vec<u8> {
    let mut mask: u64 = 0;
    for v in iter.clone() {
        mask |= v;
    }
    let (varying, trailing) = if mask == 0 {
        (0u32, 0u32)
    } else {
        let leading = mask.leading_zeros() - (64 - total_bits);
        let trailing = mask.trailing_zeros();
        (total_bits - leading - trailing, trailing)
    };
    let mut out = Vec::new();
    out.push(varying as u8);
    out.push(trailing as u8);
    if varying == 0 {
        return out;
    }
    let value_mask: u64 = if varying >= 64 {
        u64::MAX
    } else {
        (1u64 << varying) - 1
    };
    let mut acc: u64 = 0;
    let mut acc_bits: u32 = 0;
    for v in iter {
        let packed = v >> trailing;
        acc = (acc << varying) | (packed & value_mask);
        acc_bits += varying;
        while acc_bits >= 8 {
            acc_bits -= 8;
            out.push((acc >> acc_bits) as u8);
        }
    }
    if acc_bits > 0 {
        out.push((acc << (8 - acc_bits)) as u8);
    }
    out
}

#[cfg(test)]
pub fn decode_int(bytes: &[u8], n: usize) -> Vec<i32> {
    decode_impl(bytes, n).into_iter().map(|v| v as i32).collect()
}
#[cfg(test)]
pub fn decode_long(bytes: &[u8], n: usize) -> Vec<i64> {
    decode_impl(bytes, n)
}
#[cfg(test)]
fn decode_impl(bytes: &[u8], n: usize) -> Vec<i64> {
    let varying = bytes[0] as u32;
    let trailing = bytes[1] as u32;
    if varying == 0 {
        return vec![0; n];
    }
    let mut out = Vec::with_capacity(n);
    let mut bit = 0usize;
    let data = &bytes[2..];
    for _ in 0..n {
        let mut val: u64 = 0;
        for _ in 0..varying {
            let byte = data[bit / 8];
            let b = (byte >> (7 - (bit % 8))) & 1;
            val = (val << 1) | b as u64;
            bit += 1;
        }
        out.push((val << trailing) as i64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compress_int_page_roundtrips_and_matches_layout() {
        let vals: Vec<i32> = vec![0, 8, 16, 24, 40];
        let bytes = compress_int(&vals);
        assert_eq!(bytes[1], 3, "trailing clear bits (all multiples of 8)");
        let out = decode_int(&bytes, vals.len());
        assert_eq!(out, vals);
        // length formula cross-check (Task 1 Step 5):
        let varying = bytes[0] as usize;
        if varying > 0 {
            assert_eq!(bytes.len(), 2 + ((vals.len() * varying - 1) / 8) + 1);
        }
    }
    #[test]
    fn compress_all_zero_page() {
        let vals = vec![0i32; 4];
        let bytes = compress_int(&vals);
        assert_eq!(bytes[0], 0, "varyingBits=0 for all-zero");
        assert_eq!(decode_int(&bytes, 4), vals);
    }
    #[test]
    fn compress_long_page_roundtrips() {
        let vals: Vec<i64> = vec![0, 0x1000, 0x2000, 0x1_0000_0000];
        let bytes = compress_long(&vals);
        assert_eq!(decode_long(&bytes, vals.len()), vals);
    }
}
