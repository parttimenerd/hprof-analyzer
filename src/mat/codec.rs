// Compress a page of i32 into MAT ArrayIntCompressed bytes.
pub fn compress_int(vals: &[i32]) -> Vec<u8> {
    // ArrayIntCompressed: BIT_LENGTH = 32, mask and shifts are Java `int`.
    // For the 32-bit case `1 << x` stays in range 0..=31, so this matches the
    // mathematically-correct leading/trailing computation.
    let mut mask: i32 = 0;
    for &v in vals {
        mask |= v;
    }
    let (varying, trailing) = mat_bits_int(mask);
    compress_impl(vals.iter().map(|&v| v as u32 as u64), varying, trailing)
}
pub fn compress_long(vals: &[i64]) -> Vec<u8> {
    // ArrayLongCompressed: BIT_LENGTH = 64, but MAT uses `1 << x` with a Java
    // `int` literal 1. When the shift amount is >= 32 the shift wraps mod 32
    // (Java int shift semantics) and the result is sign-extended to long. This
    // MAT quirk often makes it pick a WIDER varying-bit width than the minimum;
    // we must replicate it byte-for-byte.
    let mut mask: i64 = 0;
    for &v in vals {
        mask |= v;
    }
    let (varying, trailing) = mat_bits_long(mask);
    compress_impl(vals.iter().map(|&v| v as u64), varying, trailing)
}

/// Replicate ArrayIntCompressed's leading/trailing-clear-bit computation
/// (BIT_LENGTH = 32). Returns (varying_bits, trailing_clear_bits).
fn mat_bits_int(mask: i32) -> (u32, u32) {
    const BIT: i32 = 32;
    let mut leading = 0i32;
    while leading < BIT {
        let bit = 1i32.wrapping_shl((BIT - leading - 1) as u32);
        if (mask & bit) != 0 {
            break;
        }
        leading += 1;
    }
    let mut trailing = 0i32;
    while trailing < BIT - leading {
        let bit = 1i32.wrapping_shl(trailing as u32);
        if (mask & bit) != 0 {
            break;
        }
        trailing += 1;
    }
    ((BIT - leading - trailing) as u32, trailing as u32)
}

/// Replicate ArrayLongCompressed's leading/trailing-clear-bit computation
/// (BIT_LENGTH = 64) INCLUDING the Java `int`-literal shift bug: `1 << x` uses
/// int shift semantics (mask x by 31) and the resulting int is sign-extended to
/// long before the AND. Returns (varying_bits, trailing_clear_bits).
fn mat_bits_long(mask: i64) -> (u32, u32) {
    const BIT: i32 = 64;
    let mut leading = 0i32;
    while leading < BIT {
        // Java: (1 << (BIT - leading - 1)) — 1 is an int, shift wraps mod 32,
        // the int result is sign-extended to long for the AND.
        let as_int = 1i32.wrapping_shl(((BIT - leading - 1) as u32) & 31);
        let bit = as_int as i64; // sign-extend int -> long
        if (mask & bit) != 0 {
            break;
        }
        leading += 1;
    }
    let mut trailing = 0i32;
    while trailing < BIT - leading {
        let as_int = 1i32.wrapping_shl((trailing as u32) & 31);
        let bit = as_int as i64;
        if (mask & bit) != 0 {
            break;
        }
        trailing += 1;
    }
    ((BIT - leading - trailing) as u32, trailing as u32)
}

fn compress_impl(iter: impl Iterator<Item = u64> + Clone, varying: u32, trailing: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(varying as u8);
    out.push(trailing as u8);
    if varying == 0 {
        return out;
    }
    // Bit-writer: emit `varying` bits per value, most-significant bit first, into
    // a byte stream. Using a small accumulator that is drained every 8 bits keeps
    // us within u64 even when `varying` is up to 64 (the MAT "wide" case). This
    // matches ArrayLongCompressed.set(): value >>= trailing, then the low
    // `varying` bits are stored MSB-first.
    let value_mask: u64 = if varying >= 64 {
        u64::MAX
    } else {
        (1u64 << varying) - 1
    };
    let mut acc: u64 = 0; // holds < 8 pending bits between values
    let mut acc_bits: u32 = 0;
    for v in iter {
        let packed = (v >> trailing) & value_mask;
        // Emit the `varying` bits of `packed`, MSB first.
        let mut remaining = varying;
        while remaining > 0 {
            // How many bits we can take now without exceeding an 8-bit boundary
            // in the accumulator (we drain to bytes as soon as we have >= 8).
            let take = remaining.min(8 - acc_bits);
            let shift = remaining - take;
            let chunk = (packed >> shift) & ((1u64 << take) - 1);
            acc = (acc << take) | chunk;
            acc_bits += take;
            remaining -= take;
            if acc_bits == 8 {
                out.push(acc as u8);
                acc = 0;
                acc_bits = 0;
            }
        }
    }
    if acc_bits > 0 {
        out.push((acc << (8 - acc_bits)) as u8);
    }
    out
}

#[cfg(test)]
pub fn decode_int(bytes: &[u8], n: usize) -> Vec<i32> {
    decode_impl(bytes, n)
        .into_iter()
        .map(|v| v as i32)
        .collect()
}
#[cfg(test)]
pub(crate) fn decode_long(bytes: &[u8], n: usize) -> Vec<i64> {
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

    // --- mat_bits_long Java int-shift-wrap edge cases ---

    /// When shift amount >= 32 Java int-literal `1 << x` wraps mod 32.
    /// Shift 32 wraps to shift 0 → bit = 1 (not 0x1_0000_0000). This means
    /// bit 32 of the mask is tested using int-bit 0, so the sign-extended value
    /// equals 1i64, which is ALSO testing bit 0. The result: MAT "sees" bit 32
    /// as set only when int-bit 0 is set, creating a false positive that widens
    /// varying_bits to cover both bit 32 and bit 0 together.
    #[test]
    fn mat_bits_long_shift_wraps_at_32() {
        // mask = 0x1_0000_0000 (only bit 32 set).
        // Java: 1 << (64 - 0 - 1) = 1 << 63 wraps to 1 << 31 = -2147483648 → sign-ext = -2147483648i64 = 0xFFFF_FFFF_8000_0000.
        // (mask & 0xFFFF_FFFF_8000_0000) != 0 only if bit 31 or bits 32-63 set — bit 32 IS set → leading stays 0.
        let mask: i64 = 0x1_0000_0000i64;
        let (varying, trailing) = mat_bits_long(mask);
        // Trailing: shift 0 → int 1 → i64 1. bit 0 of mask = 0 → trailing increments.
        // Continues until a shift where sign-extended int overlaps with set bit.
        // bit 32 is set; shift=31 → 1<<(31&31)=2^31 sign-ext=0xFFFF_FFFF_8000_0000, (mask&that)!=0 → stop.
        // trailing = 31, leading = 0, varying = 64 - 0 - 31 = 33.
        assert_eq!(trailing, 31, "trailing for mask=0x1_0000_0000");
        assert_eq!(varying, 33, "varying for mask=0x1_0000_0000");
        // Roundtrip must still be exact.
        let vals = vec![0i64, 0x1_0000_0000i64, 0x2_0000_0000i64];
        let bytes = compress_long(&vals);
        assert_eq!(decode_long(&bytes, vals.len()), vals);
    }

    /// Bit 63 of the mask: shift amount = 0 (leading=0, 64-0-1=63, 63&31=31).
    /// Java: 1<<31 = -2147483648 sign-ext = 0xFFFF_FFFF_8000_0000.
    /// (mask & 0xFFFF_FFFF_8000_0000) != 0 when bit 63 or bits 31-63 set.
    #[test]
    fn mat_bits_long_bit63_set() {
        let mask: i64 = i64::MIN; // only bit 63
        let (varying, trailing) = mat_bits_long(mask);
        // leading=0: shift=63, 63&31=31 → 1<<31=-2^31 sign-ext=0xFFFF_FFFF_8000_0000,
        // (mask & that) = (i64::MIN & 0xFFFF_FFFF_8000_0000) = i64::MIN ≠ 0 → stop, leading=0.
        // trailing: shift=0 → 1<<0=1, mask&1=0 → trailing++; continue while no overlap.
        // shift=31 → 0xFFFF_FFFF_8000_0000 & i64::MIN ≠ 0 → stop at trailing=31.
        assert_eq!(varying + trailing, 64, "varying+trailing <= BIT");
        let vals = vec![i64::MIN, 0i64, i64::MIN >> 1];
        let bytes = compress_long(&vals);
        assert_eq!(decode_long(&bytes, vals.len()), vals, "bit63 roundtrip");
    }

    /// varying=64 boundary: when every bit contributes, value_mask = u64::MAX
    /// (the `if varying >= 64 { u64::MAX }` guard in compress_impl).
    #[test]
    fn mat_bits_long_varying64_guard() {
        // mask = -1 (all bits set). leading=0 (any shift → bit set immediately).
        // trailing=0 (shift=0 → 1 sign-ext=1, mask&1!=0 → stop). varying=64.
        let mask: i64 = -1;
        let (varying, trailing) = mat_bits_long(mask);
        assert_eq!(varying, 64);
        assert_eq!(trailing, 0);
        // compress_impl must not panic on varying=64 (value_mask = u64::MAX).
        let vals = vec![-1i64, 0i64, i64::MAX, i64::MIN];
        let bytes = compress_long(&vals);
        assert_eq!(
            decode_long(&bytes, vals.len()),
            vals,
            "varying=64 roundtrip"
        );
    }

    /// A mask where only bits in the range [16, 47] are set — straddles the
    /// int-shift wraparound at 32. Tests that both leading and trailing are
    /// correctly computed using wrapped-shift semantics.
    #[test]
    fn mat_bits_long_straddle_32() {
        // set bits 16..=47 → mask has 32 varying bits, 16 trailing zeros.
        let mask: i64 = 0x0000_FFFF_FFFF_0000u64 as i64;
        let (varying, trailing) = mat_bits_long(mask);
        // trailing: bits 0-15 are 0, bit 16 is set.
        //   shift 0..15: 1<<(k&31)=1<<k → sign-ext positive → mask&1<<k = 0 for k<16.
        //   shift 16: 1<<16 → sign-ext = 65536 → mask & 65536 ≠ 0 → stop. trailing=16.
        assert_eq!(trailing, 16);
        // Must roundtrip.
        let vals = vec![0x0000_FFFF_0000_0000i64, 0x0000_0001_0000_0000i64, 0i64];
        let bytes = compress_long(&vals);
        assert_eq!(
            decode_long(&bytes, vals.len()),
            vals,
            "straddle32 roundtrip"
        );
    }

    /// Confirm that compress_int handles all-negative input (all bits set → mask=-1)
    /// producing varying=32, trailing=0.
    #[test]
    fn compress_int_all_negative() {
        let vals = vec![-1i32, -2i32, i32::MIN];
        let bytes = compress_int(&vals);
        assert_eq!(bytes[0], 32, "varying=32 for negative ints");
        assert_eq!(bytes[1], 0, "trailing=0 for mask=-1");
        assert_eq!(decode_int(&bytes, vals.len()), vals);
    }
}
