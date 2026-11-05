#[derive(Clone, Copy)]
pub(crate) enum Class {
    Ascii,
    JsonParse,
    JsonStringify,
}

/// Skips ASCII bytes unequal to `needle`, stopping before any non-ASCII byte.
#[inline(never)]
pub(crate) fn ascii_mismatch(bytes: &[u8], needle: u8) -> usize {
    // The loops carry the unscanned tail rather than a running position, so
    // no counter needs an overflow check on every block.
    let mut rest = bytes;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    while let Some((chunk, tail)) = rest.split_first_chunk::<16>() {
        let found = vector_ascii_mismatch(chunk, needle);
        if found < 16 {
            return bytes.len() - rest.len() + found;
        }
        rest = tail;
    }
    const LOW: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
        let word = u64::from_le_bytes(*chunk);
        let diff = word ^ (u64::from(needle) * 0x0101_0101_0101_0101);
        let found = (!(((diff & LOW) + LOW) | diff) | word) & HIGH;
        if found != 0 {
            return bytes.len() - rest.len() + found.trailing_zeros() as usize / 8;
        }
        rest = tail;
    }
    bytes.len() - rest.len()
        + rest
            .iter()
            .position(|&byte| byte == needle || !byte.is_ascii())
            .unwrap_or(rest.len())
}

#[cfg(test)]
mod byte_search_tests {
    use super::*;

    #[test]
    fn ascii_search_stops_before_utf8_at_every_lane_and_alignment() {
        for offset in 0..16 {
            for position in 0..64 {
                for byte in 128..=255 {
                    let mut bytes = [b'a'; 80];
                    bytes[offset + position] = byte;
                    assert_eq!(ascii_mismatch(&bytes[offset..], b'z'), position);
                }
            }
        }
    }

    #[test]
    fn byte_search_matches_scalar_at_every_lane_and_alignment() {
        for len in [0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 127] {
            for offset in 0..16 {
                for needle in 0..=255u8 {
                    let mut bytes = vec![needle.wrapping_add(1); len + offset];
                    for position in 0..=len {
                        if position < len {
                            bytes[offset + position] = needle;
                        }
                        let input = &bytes[offset..];
                        assert_eq!(
                            ascii_mismatch(input, needle),
                            input
                                .iter()
                                .position(|&b| b == needle || b >= 128)
                                .unwrap_or(len)
                        );
                        if position < len {
                            bytes[offset + position] = needle.wrapping_add(1);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
fn vector_ascii_mismatch(bytes: &[u8; 16], needle: u8) -> usize {
    use std::arch::aarch64::*;
    // SAFETY: the load reads exactly the sixteen bytes of `bytes`; NEON is baseline.
    unsafe {
        let input = vld1q_u8(bytes.as_ptr());
        let mask = vorrq_u8(vceqq_u8(input, vdupq_n_u8(needle)), input);
        if vmaxvq_u8(mask) < 128 {
            return 16;
        }
        let lanes = vreinterpretq_u64_u8(mask);
        let low = u64::from_le(vgetq_lane_u64::<0>(lanes)) & 0x8080_8080_8080_8080;
        if low != 0 {
            low.trailing_zeros() as usize / 8
        } else {
            let high = u64::from_le(vgetq_lane_u64::<1>(lanes)) & 0x8080_8080_8080_8080;
            8 + high.trailing_zeros() as usize / 8
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn vector_ascii_mismatch(bytes: &[u8; 16], needle: u8) -> usize {
    use std::arch::x86_64::*;
    // SAFETY: the load reads exactly the sixteen bytes of `bytes`; SSE2 is baseline.
    unsafe {
        let input = _mm_loadu_si128(bytes.as_ptr().cast());
        let mask = _mm_or_si128(
            _mm_cmpeq_epi8(input, _mm_set1_epi8(i8::from_ne_bytes([needle]))),
            input,
        );
        (u32::from_ne_bytes(_mm_movemask_epi8(mask).to_ne_bytes()) | 1 << 16).trailing_zeros()
            as usize
    }
}

pub(crate) fn ordinary(b: u8, class: Class) -> bool {
    match class {
        Class::Ascii => b < 128,
        Class::JsonParse => (32..128).contains(&b) && b != b'"' && b != b'\\',
        Class::JsonStringify => {
            (32..128).contains(&b) && b != b'"' && b != b'\\' && b != b'<' && b != b'>' && b != b'&'
        }
    }
}

/// Bytes tested at once by [`block_clean`].
#[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
const BLOCK: usize = 16;
#[cfg(not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))))]
const BLOCK: usize = 8;

/// Tests one block for `class` membership.
#[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(always)]
fn block_clean(chunk: &[u8; BLOCK], class: Class) -> bool {
    vector_clean(chunk, class)
}

/// Tests one block for `class` membership with SWAR arithmetic.
#[cfg(not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))))]
fn block_clean(chunk: &[u8; BLOCK], class: Class) -> bool {
    let v = u64::from_le_bytes(*chunk);
    let equal = |byte: u8| {
        let x = v ^ (u64::from(byte) * 0x0101010101010101);
        !(((x & 0x7f7f7f7f7f7f7f7f) + 0x7f7f7f7f7f7f7f7f) | x | 0x7f7f7f7f7f7f7f7f)
    };
    let mut bad = v & 0x8080808080808080;
    if !matches!(class, Class::Ascii) {
        bad |= !(v | ((v & 0x7f7f7f7f7f7f7f7f) + 0x6060606060606060)) & 0x8080808080808080;
        bad |= equal(b'"') | equal(b'\\');
    }
    if matches!(class, Class::JsonStringify) {
        bad |= equal(b'<') | equal(b'>') | equal(b'&');
    }
    bad == 0
}

/// Counts the leading bytes of `s` that are `class` bytes.
pub(crate) fn prefix(s: &[u8], class: Class) -> usize {
    // The first block is tested here, so a short string or a short run never
    // calls into the block loop, and a clean block skips the scalar peel. The
    // loop retests it, which keeps its wide blocks aligned to the start.
    let mut blocks = 0;
    if let Some(block) = s.first_chunk::<BLOCK>() {
        if block_clean(block, class) {
            blocks = if s.len() >= 2 * BLOCK {
                clean_blocks(s, class)
            } else {
                BLOCK
            };
        }
    }
    let tail = &s[blocks..];
    blocks
        + tail
            .iter()
            .position(|&byte| !ordinary(byte, class))
            .unwrap_or(tail.len())
}

/// Counts the bytes at the start of `s` that whole blocks of `class` bytes
/// cover. A function of its own, so that the block loop's constants stay in
/// registers whatever the register pressure of the scan around it; each class
/// gets a copy of the loop with its tests resolved.
#[inline(never)]
fn clean_blocks(s: &[u8], class: Class) -> usize {
    match class {
        Class::Ascii => clean_blocks_of(s, Class::Ascii),
        Class::JsonParse => clean_blocks_of(s, Class::JsonParse),
        Class::JsonStringify => clean_blocks_of(s, Class::JsonStringify),
    }
}

// The loops carry the unscanned tail rather than a running offset, so no
// counter needs an overflow check on every block.
#[inline(always)]
fn clean_blocks_of(s: &[u8], class: Class) -> usize {
    let mut rest = s;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    if matches!(class, Class::Ascii) {
        while let Some((chunk, tail)) = rest.split_first_chunk::<64>() {
            if !vector_ascii64(chunk) {
                break;
            }
            rest = tail;
        }
    }
    while let Some((chunk, tail)) = rest.split_first_chunk::<BLOCK>() {
        if !block_clean(chunk, class) {
            break;
        }
        rest = tail;
    }
    s.len() - rest.len()
}

#[derive(Default)]
pub(crate) struct TextSpan {
    pub len: usize,
    pub runes: usize,
    pub steps: u64,
}

/// Scans valid UTF-8 until an escape, invalid byte, or incomplete trailing rune.
// Inlined into the string scanners even under overflow checks, which made it
// too large for the inliner's taste: the JSON writer's escape loop runs a
// fifth slower around an out-of-line call it never takes.
#[inline(always)]
pub(crate) fn text_span(s: &[u8], class: Class) -> TextSpan {
    let mut span = TextSpan::default();
    while span.len < s.len() {
        if s[span.len] < 128 {
            let n = prefix(&s[span.len..], class);
            if n == 0 {
                break;
            }
            span.len += n;
            span.runes += n;
            span.steps += (n as u64).div_ceil(64);
            if span.len < s.len() && s[span.len] < 128 {
                break;
            }
        } else {
            let tail = &s[span.len..];
            if matches!(class, Class::JsonParse) {
                let unicode = unicode_span(tail);
                if unicode.len == 0 {
                    break;
                }
                span.len += unicode.len;
                span.runes += unicode.runes;
                span.steps += unicode.steps;
                continue;
            }
            let (n, valid) = rune_width(tail);
            if !valid
                || (matches!(class, Class::JsonStringify)
                    && n == 3
                    && tail[0] == 0xe2
                    && tail[1] == 0x80
                    && matches!(tail[2], 0xa8 | 0xa9))
            {
                break;
            }
            span.len += n;
            span.runes += 1;
            span.steps += 1;
        }
    }
    span
}

/// Counts a run of valid non-ASCII characters without constructing code points.
#[inline(always)]
pub(crate) fn unicode_span(s: &[u8]) -> TextSpan {
    // Most runs are a character or a word between ASCII. Those stay in the
    // caller: a call costs them up to a tenth, and a block scan that ends in
    // its first or second block costs more than it saves.
    const HIGH: u128 = 0x8080_8080_8080_8080_8080_8080_8080_8080;
    let (start, mut runes) = match s.first_chunk::<16>() {
        Some(head) if u128::from_le_bytes(*head) & HIGH == HIGH => unicode_blocks(s),
        _ => (0, 0),
    };
    let mut rest = &s[start..];
    let mut state = UTF8_ACCEPT;
    let mut bytes = rest.iter();
    while let Some(&byte) = bytes.next() {
        state = utf8_step(state, byte);
        match state & 63 {
            UTF8_REJECT => break,
            UTF8_ACCEPT => {
                rest = bytes.as_slice();
                // A rune takes at least a byte, so the count stays below
                // `s.len()` and cannot wrap; a checked add here is a branch
                // on every rune.
                runes = runes.wrapping_add(1);
            }
            _ => {}
        }
    }
    TextSpan {
        len: s.len() - rest.len(),
        runes,
        steps: runes as u64,
    }
}

/// Validates `s` in whole blocks from a rune boundary, until one holds ASCII,
/// an invalid byte or too few bytes. Returns the start of the last rune a
/// valid block began, which may continue past it and so is left to the
/// caller, and the number of complete runes before that point.
#[inline(never)]
fn unicode_blocks(s: &[u8]) -> (usize, usize) {
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    let (start, mut runes) = vector_unicode(s);
    #[cfg(not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))))]
    let (start, mut runes) = (0, 0usize);
    // Decoding a rune at a time makes each lead's address wait on the width
    // of the rune before it, and branches on that width. Here the state is
    // all a byte waits on, and nothing branches until the block is done.
    let mut rest = &s[start..];
    let mut state = UTF8_ACCEPT;
    while let Some(block) = rest.first_chunk::<8>() {
        for &byte in block {
            state = utf8_step(state, byte);
        }
        if state & 63 == UTF8_REJECT {
            break;
        }
        // No byte of a valid block is ASCII, so bit 6 alone marks its leads.
        let leads = u64::from_le_bytes(*block) & 0x4040_4040_4040_4040;
        runes = runes.wrapping_add(leads.count_ones() as usize);
        rest = &rest[8..];
    }
    let valid = &s[start..s.len() - rest.len()];
    match valid.iter().rposition(|&byte| byte & 0xc0 != 0x80) {
        Some(lead) => (start + lead, runes - 1),
        None => (start, runes),
    }
}

pub(crate) fn ascii_case(s: &mut [u8], upper: bool) {
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    let tail = {
        let mut chunks = s.chunks_exact_mut(16);
        for chunk in &mut chunks {
            vector_case(chunk.try_into().unwrap(), upper);
        }
        chunks.into_remainder()
    };
    #[cfg(not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))))]
    let tail = s;
    for b in tail {
        *b = if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        };
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
fn vector_ascii64(s: &[u8; 64]) -> bool {
    use std::arch::aarch64::*;
    // SAFETY: the four loads read the 64 bytes of `s`; NEON is baseline.
    unsafe {
        let p = s.as_ptr();
        let a = vorrq_u8(vld1q_u8(p), vld1q_u8(p.add(16)));
        let b = vorrq_u8(vld1q_u8(p.add(32)), vld1q_u8(p.add(48)));
        vmaxvq_u8(vorrq_u8(a, b)) < 128
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn vector_ascii64(s: &[u8; 64]) -> bool {
    use std::arch::x86_64::*;
    // SAFETY: the four loads read the 64 bytes of `s`; SSE2 is baseline.
    unsafe {
        let p = s.as_ptr();
        let a = _mm_or_si128(_mm_loadu_si128(p.cast()), _mm_loadu_si128(p.add(16).cast()));
        let b = _mm_or_si128(
            _mm_loadu_si128(p.add(32).cast()),
            _mm_loadu_si128(p.add(48).cast()),
        );
        _mm_movemask_epi8(_mm_or_si128(a, b)) == 0
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
fn vector_clean(s: &[u8; 16], class: Class) -> bool {
    use std::arch::aarch64::*;
    // SAFETY: the load reads exactly the sixteen bytes of `s`; NEON is baseline.
    unsafe {
        let v = vld1q_u8(s.as_ptr());
        if matches!(class, Class::Ascii) {
            return vmaxvq_u8(v) < 128;
        }
        let mut bad = vcgeq_u8(v, vdupq_n_u8(128));
        if !matches!(class, Class::Ascii) {
            bad = vorrq_u8(bad, vcltq_u8(v, vdupq_n_u8(32)));
            bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b'"')));
            bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b'\\')));
        }
        if matches!(class, Class::JsonStringify) {
            for b in *b"<>&" {
                bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b)));
            }
        }
        vmaxvq_u8(bad) == 0
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn vector_clean(s: &[u8; 16], class: Class) -> bool {
    use std::arch::x86_64::*;
    // SAFETY: the load reads exactly the sixteen bytes of `s`; SSE2 is baseline.
    unsafe {
        let v = _mm_loadu_si128(s.as_ptr().cast());
        if matches!(class, Class::Ascii) {
            return _mm_movemask_epi8(v) == 0;
        }
        let mut bad = _mm_cmplt_epi8(v, _mm_set1_epi8(32));
        for b in *b"\"\\" {
            bad = _mm_or_si128(
                bad,
                _mm_cmpeq_epi8(v, _mm_set1_epi8(i8::from_ne_bytes([b]))),
            );
        }
        if matches!(class, Class::JsonStringify) {
            for b in *b"<>&" {
                bad = _mm_or_si128(
                    bad,
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(i8::from_ne_bytes([b]))),
                );
            }
        }
        _mm_movemask_epi8(bad) == 0
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn vector_unicode(s: &[u8]) -> (usize, usize) {
    use std::arch::x86_64::*;
    // SAFETY: the loop bounds every unaligned 16-byte load; SSE2 is baseline.
    unsafe {
        let at_least = |v, b: u8| {
            _mm_cmpgt_epi8(
                _mm_xor_si128(v, _mm_set1_epi8(-128)),
                _mm_set1_epi8(i8::from_ne_bytes([(b ^ 128).wrapping_sub(1)])),
            )
        };
        let below = |v, b| _mm_andnot_si128(at_least(v, b), _mm_set1_epi8(-1));
        let equal = |v, b: u8| _mm_cmpeq_epi8(v, _mm_set1_epi8(i8::from_ne_bytes([b])));
        let mut previous = _mm_setzero_si128();
        let (mut start, mut leads, mut boundary) = (0, 0, 0);
        while s.len() - start >= 16 {
            let v = _mm_loadu_si128(s.as_ptr().add(start).cast());
            let prev1 = _mm_or_si128(_mm_slli_si128::<1>(v), _mm_srli_si128::<15>(previous));
            let prev2 = _mm_or_si128(_mm_slli_si128::<2>(v), _mm_srli_si128::<14>(previous));
            let prev3 = _mm_or_si128(_mm_slli_si128::<3>(v), _mm_srli_si128::<13>(previous));
            let continuation = below(v, 0xc0);
            let expected = _mm_or_si128(
                _mm_or_si128(at_least(prev1, 0xc0), at_least(prev2, 0xe0)),
                at_least(prev3, 0xf0),
            );
            let mut bad = _mm_or_si128(_mm_xor_si128(continuation, expected), below(v, 0x80));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xe0), below(v, 0xa0)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xed), at_least(v, 0xa0)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xf0), below(v, 0x90)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xf4), at_least(v, 0x90)));
            bad = _mm_or_si128(
                bad,
                _mm_andnot_si128(
                    continuation,
                    _mm_or_si128(below(v, 0xc2), at_least(v, 0xf5)),
                ),
            );
            if _mm_movemask_epi8(bad) != 0 {
                break;
            }
            let lead = !u32::from_ne_bytes(_mm_movemask_epi8(continuation).to_ne_bytes()) & 0xffff;
            leads += lead.count_ones() as usize;
            boundary = start + (31 - lead.leading_zeros()) as usize;
            previous = v;
            start += 16;
        }
        (boundary, leads.saturating_sub(1))
    }
}

/// Validates `s` 16 bytes at a time as `rune_width` would decode it, from a
/// rune boundary until a block holds ASCII, an invalid byte or too few bytes.
/// Returns the start of the last rune a valid block began, which may continue
/// past it and so is left to the scalar decoder, and the number of complete
/// runes before that point.
#[cfg(all(feature = "simd", target_arch = "aarch64"))]
fn vector_unicode(s: &[u8]) -> (usize, usize) {
    use std::arch::aarch64::*;
    // SAFETY: every load reads the 16 bytes at `start`, which the loop keeps
    // within `s`; NEON is baseline.
    unsafe {
        let at_least = |v, b| vcgeq_u8(v, vdupq_n_u8(b));
        let below = |v, b| vcltq_u8(v, vdupq_n_u8(b));
        let equal = |v, b| vceqq_u8(v, vdupq_n_u8(b));
        // Nothing before a rune boundary expects a continuation byte.
        let mut previous = vdupq_n_u8(0);
        // Lead bytes are counted per lane and summed every 255 blocks, before
        // a lane can overflow; the last valid block's leads give the boundary.
        let (mut leads, mut pending, mut blocks) = (0, vdupq_n_u8(0), 0);
        let mut last = None;
        let mut start = 0;
        while s.len() - start >= 16 {
            let v = vld1q_u8(s.as_ptr().add(start));
            let prev1 = vextq_u8::<15>(previous, v);
            let prev2 = vextq_u8::<14>(previous, v);
            let prev3 = vextq_u8::<13>(previous, v);
            let continuation = below(v, 0xc0);
            let expected = vorrq_u8(
                vorrq_u8(at_least(prev1, 0xc0), at_least(prev2, 0xe0)),
                at_least(prev3, 0xf0),
            );
            let mut bad = vorrq_u8(veorq_u8(continuation, expected), below(v, 0x80));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xe0), below(v, 0xa0)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xed), at_least(v, 0xa0)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xf0), below(v, 0x90)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xf4), at_least(v, 0x90)));
            bad = vorrq_u8(
                bad,
                vbicq_u8(vorrq_u8(below(v, 0xc2), at_least(v, 0xf5)), continuation),
            );
            if vmaxvq_u8(bad) != 0 {
                break;
            }
            let lead = vmvnq_u8(continuation);
            // A lead mask lane is 0xff, so subtracting it counts one.
            pending = vsubq_u8(pending, lead);
            blocks += 1;
            if blocks == 255 {
                leads += usize::from(vaddlvq_u8(pending));
                (pending, blocks) = (vdupq_n_u8(0), 0);
            }
            last = Some((start, lead));
            previous = v;
            start += 16;
        }
        let Some((start, lead)) = last else {
            return (0, 0);
        };
        leads += usize::from(vaddlvq_u8(pending));
        // Four mask bits per byte, so lane i owns bits 4i..4i+3. A valid
        // block starts a rune in every four bytes, so `bits` is nonzero.
        let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(
            lead,
        ))));
        (start + (63 - bits.leading_zeros() as usize) / 4, leads - 1)
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
fn vector_case(s: &mut [u8; 16], upper: bool) {
    use std::arch::aarch64::*;
    // SAFETY: the load and store touch exactly the sixteen bytes of `s`, which
    // the exclusive borrow owns; NEON is baseline.
    unsafe {
        let v = vld1q_u8(s.as_ptr());
        let lo = if upper { b'a' } else { b'A' };
        let hi = lo + 25;
        let mask = vandq_u8(vcgeq_u8(v, vdupq_n_u8(lo)), vcleq_u8(v, vdupq_n_u8(hi)));
        let flipped = veorq_u8(v, vandq_u8(mask, vdupq_n_u8(32)));
        vst1q_u8(s.as_mut_ptr(), flipped);
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
fn vector_case(s: &mut [u8; 16], upper: bool) {
    use std::arch::x86_64::*;
    // SAFETY: the load and store touch exactly the sixteen bytes of `s`, which
    // the exclusive borrow owns; SSE2 is baseline.
    unsafe {
        let v = _mm_loadu_si128(s.as_ptr().cast());
        let lo = if upper { b'a' } else { b'A' };
        let mask = _mm_and_si128(
            _mm_cmpgt_epi8(v, _mm_set1_epi8(i8::from_ne_bytes([lo - 1]))),
            _mm_cmplt_epi8(v, _mm_set1_epi8(i8::from_ne_bytes([lo + 26]))),
        );
        _mm_storeu_si128(
            s.as_mut_ptr().cast(),
            _mm_xor_si128(v, _mm_and_si128(mask, _mm_set1_epi8(32))),
        );
    }
}

// Low bits give sequence width; high bits select the legal second-byte range.
const UTF8_LEAD: [u8; 256] = {
    let mut table = [0; 256];
    let mut b = 0;
    while b < 256 {
        table[b] = match b {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0 => 0x13,
            0xe1..=0xec | 0xee..=0xef => 3,
            0xed => 0x23,
            0xf0 => 0x34,
            0xf1..=0xf3 => 4,
            0xf4 => 0x44,
            _ => 0,
        };
        b += 1;
    }
    table
};

/// The legal second bytes for each range `UTF8_LEAD` selects: the lowest, and
/// how many more follow it.
static UTF8_SECOND: [(u8, u8); 5] = [
    (0x80, 0x3f),
    (0xa0, 0x1f),
    (0x80, 0x1f),
    (0x90, 0x2f),
    (0x80, 0x0f),
];

// A recognizer for non-ASCII UTF-8 whose step is one shift (a shift-based
// DFA). A state is the bit offset of a six-bit field, and a byte's entry
// holds, in each state's field, the state that byte leads to from there.
// Rejection is field zero: ASCII, like any byte a state does not expect,
// leads to it and none leave.
const UTF8_REJECT: u64 = 0;
const UTF8_ACCEPT: u64 = 6;

static UTF8_STEP: [u64; 256] = {
    // Continuation bytes still owed, then the leads whose second byte has a
    // narrower range than the others.
    const ONE: u64 = 12;
    const TWO: u64 = 18;
    const THREE: u64 = 24;
    const E0: u64 = 30;
    const ED: u64 = 36;
    const F0: u64 = 42;
    const F4: u64 = 48;
    let mut table = [0; 256];
    let mut b = 0;
    while b < 256 {
        let lead = match b {
            0xc2..=0xdf => ONE,
            0xe0 => E0,
            0xe1..=0xec | 0xee..=0xef => TWO,
            0xed => ED,
            0xf0 => F0,
            0xf1..=0xf3 => THREE,
            0xf4 => F4,
            _ => UTF8_REJECT,
        };
        let mut entry = lead << UTF8_ACCEPT;
        if matches!(b, 0x80..=0xbf) {
            entry |= UTF8_ACCEPT << ONE | ONE << TWO | TWO << THREE;
        }
        if matches!(b, 0xa0..=0xbf) {
            entry |= ONE << E0;
        }
        if matches!(b, 0x80..=0x9f) {
            entry |= ONE << ED;
        }
        if matches!(b, 0x90..=0xbf) {
            entry |= TWO << F0;
        }
        if matches!(b, 0x80..=0x8f) {
            entry |= TWO << F4;
        }
        table[b] = entry;
        b += 1;
    }
    table
};

/// Steps the recognizer. Only the low six bits of a state are the state; the
/// shift leaves other fields above them, and masking them off the amount
/// rather than the result costs nothing, since a shift ignores them anyway.
#[inline(always)]
fn utf8_step(state: u64, byte: u8) -> u64 {
    UTF8_STEP[usize::from(byte)] >> (state & 63)
}

fn rune_width(s: &[u8]) -> (usize, bool) {
    sequence(s)
}

/// The body of [`rune_width`], for a decoder that has to stay a leaf.
#[inline(always)]
fn sequence(s: &[u8]) -> (usize, bool) {
    let tag = UTF8_LEAD[usize::from(s[0])];
    let n = usize::from(tag & 7);
    if n <= 1 {
        return (1, n == 1);
    }
    if s.len() < n {
        return (1, false);
    }
    let (low, span) = UTF8_SECOND[usize::from(tag >> 4)];
    if s[1].wrapping_sub(low) > span
        || (n >= 3 && s[2] & 0xc0 != 0x80)
        || (n == 4 && s[3] & 0xc0 != 0x80)
    {
        return (1, false);
    }
    (n, true)
}

/// Decodes one rune, replacing each invalid byte as Go's UTF-8 decoder does.
pub(crate) fn rune(s: &[u8]) -> (char, usize, bool) {
    if s[0] < 128 {
        return (s[0] as char, 1, true);
    }
    let (n, valid) = rune_width(s);
    if !valid {
        return ('\u{fffd}', 1, false);
    }
    let mut cp = u32::from(s[0] & (0x7f >> n));
    for &byte in &s[1..n] {
        cp = (cp << 6) | u32::from(byte & 0x3f);
    }
    (char::from_u32(cp).unwrap(), n, true)
}

/// Decodes one rune as [`rune`] does, to its code point and width.
///
/// Never inlined and calling nothing, for a loop that keeps only its ASCII
/// step inline: one call per rune, with no registers to save around it.
#[inline(never)]
pub(crate) fn code_point(s: &[u8]) -> (u32, usize) {
    if s[0] < 128 {
        return (u32::from(s[0]), 1);
    }
    let (n, valid) = sequence(s);
    if !valid {
        return (u32::from(char::REPLACEMENT_CHARACTER), 1);
    }
    let mut cp = u32::from(s[0] & (0x7f >> n));
    for &byte in &s[1..n] {
        cp = (cp << 6) | u32::from(byte & 0x3f);
    }
    (cp, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_rune(bytes: &[u8]) -> (char, usize, bool) {
        let valid = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
        };
        match valid.chars().next() {
            Some(ch) => (ch, ch.len_utf8(), true),
            None => ('\u{fffd}', 1, false),
        }
    }

    #[test]
    fn decoder_matches_every_unicode_scalar_and_invalid_prefixes() {
        for cp in 0..=0x10ffff {
            if let Some(ch) = char::from_u32(cp) {
                let mut buffer = [0; 4];
                let bytes = ch.encode_utf8(&mut buffer).as_bytes();
                assert_eq!(rune(bytes), (ch, bytes.len(), true), "U+{cp:04X}");
                assert_eq!(code_point(bytes), (cp, bytes.len()), "U+{cp:04X}");
                for len in 1..bytes.len() {
                    assert_eq!(rune(&bytes[..len]), ('\u{fffd}', 1, false));
                    assert_eq!(code_point(&bytes[..len]), (0xfffd, 1));
                }
            }
        }
        for a in 0..=255 {
            for b in 0..=255 {
                for tail in [[0, 0], [0x80, 0x80], [0xbf, 0xbf], [0xff, 0xff]] {
                    let bytes = [a, b, tail[0], tail[1]];
                    for len in 1..=bytes.len() {
                        assert_eq!(
                            rune(&bytes[..len]),
                            reference_rune(&bytes[..len]),
                            "{bytes:02x?}/{len}"
                        );
                        let (ch, width, _) = reference_rune(&bytes[..len]);
                        assert_eq!(
                            code_point(&bytes[..len]),
                            (ch as u32, width),
                            "{bytes:02x?}/{len}"
                        );
                    }
                }
            }
        }
    }

    /// The rune-at-a-time span the vector and wide paths must reproduce.
    fn reference_unicode_span(s: &[u8]) -> (usize, usize) {
        let (mut len, mut runes) = (0, 0);
        while len < s.len() && s[len] >= 128 {
            let (n, valid) = rune_width(&s[len..]);
            if !valid {
                break;
            }
            len += n;
            runes += 1;
        }
        (len, runes)
    }

    #[test]
    fn unicode_spans_match_rune_at_a_time_decoding() {
        let pieces: [&[u8]; 24] = [
            b"\xc2\x80",
            b"\xdf\xbf",
            "é".as_bytes(),
            b"\xe0\xa0\x80",
            b"\xe0\x9f\xbf",
            b"\xed\x9f\xbf",
            b"\xed\xa0\x80",
            b"\xef\xbf\xbf",
            "界".as_bytes(),
            b"\xf0\x90\x80\x80",
            b"\xf0\x8f\xbf\xbf",
            b"\xf4\x8f\xbf\xbf",
            b"\xf4\x90\x80\x80",
            "🙂".as_bytes(),
            b"\xc0\x80",
            b"\xc1\xbf",
            b"\xf5\x80\x80\x80",
            b"\xff",
            b"\x80",
            b"\xbf",
            b"\xe7\x95",
            b"\xf0\x9f\x99",
            b"a",
            b"\xe1\x80\x80",
        ];
        // Uniform text makes the same lanes lead in every block, the case
        // where per-lane counts come closest to overflowing.
        for text in ["é", "界", "🙂", "é界🙂"] {
            for tail in [&b""[..], b"a", b"\xff", b"\xe7\x95"] {
                let mut input = text.repeat(8000 / text.len()).into_bytes();
                input.extend_from_slice(tail);
                let span = unicode_span(&input);
                let (len, runes) = reference_unicode_span(&input);
                assert_eq!((span.len, span.runes), (len, runes), "{text} {tail:02x?}");
            }
        }
        // Every lead and second byte at each offset in a block, between text
        // that keeps the blocks around them whole and valid.
        let text = "界é".repeat(8).into_bytes();
        let mut input = Vec::new();
        for a in 0..=255 {
            for b in 0..=255 {
                for tail in [[0, 0], [0x80, 0x80], [0xbf, 0xbf], [0xff, 0xff]] {
                    for before in [0, 3, 5, 10, 15, 20, 25, 30] {
                        input.clear();
                        input.extend_from_slice(&text[..before]);
                        input.extend_from_slice(&[a, b, tail[0], tail[1]]);
                        input.extend_from_slice(&text);
                        let span = unicode_span(&input);
                        let (len, runes) = reference_unicode_span(&input);
                        assert_eq!((span.len, span.runes), (len, runes), "{input:02x?}");
                    }
                }
            }
        }
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for case in 0..20000 {
            let mut input = Vec::new();
            // Mostly valid text, so spans run long enough to cross blocks.
            let valid = next(4) != 0;
            // Some inputs outgrow the 255 blocks a vector lane can count.
            let target = if case % 50 == 0 {
                4000 + next(4000)
            } else {
                next(200) + 1
            };
            while input.len() < target {
                let piece = if valid && next(40 + input.len() / 8) != 0 {
                    pieces[[2, 8, 13, 0, 1, 3, 5, 7, 9, 11, 23][next(11)]]
                } else {
                    pieces[next(pieces.len())]
                };
                input.extend_from_slice(piece);
            }
            for start in 0..input.len().min(5) {
                for end in [input.len(), input.len().saturating_sub(1).max(start)] {
                    let slice = &input[start..end];
                    let span = unicode_span(slice);
                    assert_eq!(
                        (span.len, span.runes, span.steps),
                        {
                            let (len, runes) = reference_unicode_span(slice);
                            (len, runes, runes as u64)
                        },
                        "case {case}: {slice:02x?}"
                    );
                }
            }
        }
    }

    #[test]
    fn classifiers_match_scalar_at_every_lane() {
        for class in [Class::Ascii, Class::JsonParse, Class::JsonStringify] {
            for len in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127] {
                for pos in 0..len {
                    for byte in 0..=255 {
                        let mut data = vec![b'a'; len];
                        data[pos] = byte;
                        assert_eq!(
                            prefix(&data, class),
                            data.iter()
                                .position(|&b| !ordinary(b, class))
                                .unwrap_or(len)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn case_matches_scalar_including_unaligned_slices() {
        for upper in [false, true] {
            for offset in 0..16 {
                for len in 0..80 {
                    let mut data: Vec<u8> = (0..offset + len).map(|i| (i * 31) as u8).collect();
                    let expected: Vec<_> = data[offset..]
                        .iter()
                        .map(|b| {
                            if upper {
                                b.to_ascii_uppercase()
                            } else {
                                b.to_ascii_lowercase()
                            }
                        })
                        .collect();
                    ascii_case(&mut data[offset..], upper);
                    assert_eq!(&data[offset..], expected);
                }
            }
        }
    }
}
