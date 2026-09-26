//! A bounded structural index. Only the current 64 input bytes are retained;
//! scanning ahead cannot allocate or change the parser's failure order.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Masks {
    punctuation: u64,
    quote: u64,
    slash: u64,
    space: u64,
    control: u64,
    high: u64,
    digit: u64,
}

#[derive(Default)]
pub(super) struct Scanner {
    start: usize,
    end: usize,
    masks: Masks,
    structural: u64,
    inside: bool,
    escaped: bool,
    #[cfg(test)]
    pub portable: bool,
}

impl Scanner {
    /// Tests a structural delimiter outside string contents.
    pub fn punctuation(&mut self, input: &[u8], pos: usize) -> bool {
        self.block(input, pos);
        self.structural & (1 << (pos - self.start)) != 0
    }

    fn block(&mut self, input: &[u8], pos: usize) {
        if pos >= self.start && pos < self.end {
            return;
        }
        // Unicode escapes can rewind inside a surrogate pair. Rebuild from
        // that position; the raw masks used by string decoding are independent
        // of quote state. Normal forward traversal preserves the carry bits.
        if pos < self.start {
            self.end = pos;
            self.inside = true;
            self.escaped = false;
        }
        while pos >= self.end {
            self.start = self.end;
            self.end = input.len().min(self.start + 64);
            self.masks = classify(&input[self.start..self.end], self.portable());
            let (quoted, inside, escaped) = strings(
                self.masks.quote,
                self.masks.slash,
                self.inside,
                self.escaped,
            );
            self.structural = self.masks.punctuation & !quoted;
            self.inside = inside;
            self.escaped = escaped;
        }
    }

    fn portable(&self) -> bool {
        #[cfg(test)]
        return self.portable;
        #[cfg(not(test))]
        false
    }

    /// Returns the ASCII string prefix, stopping before escapes or UTF-8.
    pub fn string(&mut self, input: &[u8], pos: usize, end: usize) -> usize {
        if end - pos >= 256 {
            self.block(input, pos);
            let ordinary =
                !(self.masks.quote | self.masks.slash | self.masks.control | self.masks.high);
            let available = self.end - pos;
            let n = ((ordinary >> (pos - self.start)).trailing_ones() as usize).min(available);
            if n == available {
                let n =
                    n + crate::scan::prefix(&input[pos + n..end], crate::scan::Class::JsonParse);
                self.skip_string(pos + n);
                return n;
            }
            return n;
        }
        self.prefix(input, pos, end, |m| {
            !(m.quote | m.slash | m.control | m.high)
        })
    }

    /// Advances over an already validated, unescaped string run.
    pub fn skip_string(&mut self, pos: usize) {
        if pos >= self.end {
            self.start = pos;
            self.end = pos;
            self.inside = true;
            self.escaped = false;
        }
    }

    /// Resumes structural scanning outside a string decoded without the index.
    pub fn end_string(&mut self, pos: usize) {
        if pos >= self.end {
            self.start = pos;
            self.end = pos;
            self.inside = false;
            self.escaped = false;
        }
    }

    /// Returns a whitespace prefix, with the caller retaining charge boundaries.
    pub fn space(&mut self, input: &[u8], pos: usize, end: usize) -> usize {
        self.prefix(input, pos, end, |m| m.space)
    }

    /// Returns a decimal digit prefix, without accepting signs or punctuation.
    pub fn digits(&mut self, input: &[u8], pos: usize, end: usize) -> usize {
        self.prefix(input, pos, end, |m| m.digit)
    }

    fn prefix(
        &mut self,
        input: &[u8],
        pos: usize,
        end: usize,
        mask: impl Fn(Masks) -> u64,
    ) -> usize {
        let mut at = pos;
        while at < end {
            self.block(input, at);
            let bits = mask(self.masks) >> (at - self.start);
            let n = (bits.trailing_ones() as usize).min(self.end.min(end) - at);
            at += n;
            if at < self.end.min(end) {
                break;
            }
        }
        at - pos
    }
}

fn strings(quotes: u64, mut slashes: u64, inside: bool, carry: bool) -> (u64, bool, bool) {
    let mut escaped = u64::from(carry);
    let mut next = false;
    while slashes != 0 {
        let start = slashes.trailing_zeros();
        let length = (slashes >> start).trailing_ones();
        let end = start + length;
        let odd = (length + u32::from(start == 0 && carry)) & 1 != 0;
        if end == 64 {
            next = odd;
            break;
        }
        escaped |= u64::from(odd) << end;
        slashes &= !((1u64 << end) - 1);
    }
    let mut parity = quotes & !escaped;
    // Inclusive prefix XOR gives quote parity in every lane.
    for shift in [1, 2, 4, 8, 16, 32] {
        parity ^= parity << shift;
    }
    parity ^= 0u64.wrapping_sub(u64::from(inside));
    (parity, parity >> 63 != 0, next)
}

fn classify(input: &[u8], portable: bool) -> Masks {
    let mut masks = Masks::default();
    let mut at = 0;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    if !portable {
        while input.len() - at >= 16 {
            // SAFETY: this slice has 16 accessible bytes; NEON and SSE2 are
            // baseline features of aarch64 and x86_64 respectively.
            let lane = unsafe { vector(&input[at..at + 16]) };
            masks.punctuation |= lane.punctuation << at;
            masks.quote |= lane.quote << at;
            masks.slash |= lane.slash << at;
            masks.space |= lane.space << at;
            masks.control |= lane.control << at;
            masks.high |= lane.high << at;
            masks.digit |= lane.digit << at;
            at += 16;
        }
    }
    let _ = portable;
    while input.len() - at >= 8 {
        let bytes = u64::from_le_bytes(input[at..at + 8].try_into().unwrap());
        let equal = |byte| zero(bytes ^ (u64::from(byte) * 0x0101010101010101));
        let pack = |bits: u64| ((bits >> 7).wrapping_mul(0x0102040810204080) >> 56) << at;
        masks.punctuation |=
            pack(equal(b'{') | equal(b'}') | equal(b'[') | equal(b']') | equal(b':') | equal(b','));
        masks.quote |= pack(equal(b'"'));
        masks.slash |= pack(equal(b'\\'));
        masks.space |= pack(equal(b' ') | equal(b'\t') | equal(b'\n') | equal(b'\r'));
        masks.high |= pack(bytes & 0x8080808080808080);
        // Set each lane's high bit independently; no cross-lane borrows.
        masks.control |= pack(
            !(bytes | ((bytes & 0x7f7f7f7f7f7f7f7f) + 0x6060606060606060)) & 0x8080808080808080,
        );
        let lower = (bytes | 0x8080808080808080).wrapping_sub(0x3030303030303030);
        let upper = (bytes & 0x7f7f7f7f7f7f7f7f) + 0x4646464646464646;
        masks.digit |= pack(lower & !upper & !bytes & 0x8080808080808080);
        at += 8;
    }
    for (offset, &byte) in input[at..].iter().enumerate() {
        let bit = 1 << (at + offset);
        masks.punctuation |=
            u64::from(matches!(byte, b'{' | b'}' | b'[' | b']' | b':' | b',')) * bit;
        masks.quote |= u64::from(byte == b'"') * bit;
        masks.slash |= u64::from(byte == b'\\') * bit;
        masks.space |= u64::from(matches!(byte, b' ' | b'\t' | b'\n' | b'\r')) * bit;
        masks.control |= u64::from(byte < 32) * bit;
        masks.high |= u64::from(byte >= 128) * bit;
        masks.digit |= u64::from(byte.is_ascii_digit()) * bit;
    }
    masks
}

fn zero(bytes: u64) -> u64 {
    !(((bytes & 0x7f7f7f7f7f7f7f7f) + 0x7f7f7f7f7f7f7f7f) | bytes | 0x7f7f7f7f7f7f7f7f)
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
unsafe fn vector(input: &[u8]) -> Masks {
    use std::arch::aarch64::*;
    // SAFETY: caller supplies a full vector on a NEON target.
    unsafe {
        let v = vld1q_u8(input.as_ptr());
        let eq = |b| vceqq_u8(v, vdupq_n_u8(b));
        let bits = |v| {
            let weights =
                vld1q_u8([1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128].as_ptr());
            let masked = vandq_u8(v, weights);
            u64::from(vaddv_u8(vget_low_u8(masked)))
                | (u64::from(vaddv_u8(vget_high_u8(masked))) << 8)
        };
        Masks {
            punctuation: bits(vorrq_u8(
                vorrq_u8(vorrq_u8(eq(b'{'), eq(b'}')), vorrq_u8(eq(b'['), eq(b']'))),
                vorrq_u8(eq(b':'), eq(b',')),
            )),
            quote: bits(eq(b'"')),
            slash: bits(eq(b'\\')),
            space: bits(vorrq_u8(
                vorrq_u8(eq(b' '), eq(b'\t')),
                vorrq_u8(eq(b'\n'), eq(b'\r')),
            )),
            control: bits(vcltq_u8(v, vdupq_n_u8(32))),
            high: bits(vcgeq_u8(v, vdupq_n_u8(128))),
            digit: bits(vcleq_u8(vsubq_u8(v, vdupq_n_u8(b'0')), vdupq_n_u8(9))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_match_every_byte_in_every_lane() {
        for len in [1, 7, 8, 15, 16, 17, 31, 32, 63, 64] {
            for lane in 0..len {
                for byte in 0..=255 {
                    let mut text = vec![b'a'; len];
                    text[lane] = byte;
                    let mut expected = Masks::default();
                    for (at, b) in text.iter().enumerate() {
                        let one = classify(std::slice::from_ref(b), true);
                        expected.punctuation |= one.punctuation << at;
                        expected.quote |= one.quote << at;
                        expected.slash |= one.slash << at;
                        expected.space |= one.space << at;
                        expected.control |= one.control << at;
                        expected.high |= one.high << at;
                        expected.digit |= one.digit << at;
                    }
                    assert_eq!(classify(&text, false), expected, "SIMD {lane} {byte}");
                    assert_eq!(classify(&text, true), expected, "SWAR {lane} {byte}");
                }
            }
        }
    }

    #[test]
    fn quote_parity_and_escape_carries_match_byte_scanning() {
        for padding in 0..128 {
            for slashes in 0..132 {
                let text = format!(
                    "[\"{}{}\",{{\"k\":\"v\"}}]",
                    "a".repeat(padding),
                    "\\".repeat(slashes)
                );
                let mut scanner = Scanner::default();
                let (mut quoted, mut escaped) = (false, false);
                for (pos, byte) in text.bytes().enumerate() {
                    if byte == b'"' && !escaped {
                        quoted = !quoted;
                    }
                    let structural =
                        matches!(byte, b'[' | b']' | b'{' | b'}' | b',' | b':') && !quoted;
                    assert_eq!(
                        scanner.punctuation(text.as_bytes(), pos),
                        structural,
                        "{padding} {slashes} {pos}"
                    );
                    escaped = byte == b'\\' && !escaped;
                }
            }
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
unsafe fn vector(input: &[u8]) -> Masks {
    use std::arch::x86_64::*;
    // SAFETY: caller supplies a full vector; SSE2 is baseline on x86_64.
    unsafe {
        let v = _mm_loadu_si128(input.as_ptr().cast());
        let eq = |b| _mm_cmpeq_epi8(v, _mm_set1_epi8(b as i8));
        let bits = |v| _mm_movemask_epi8(v) as u64;
        let high = bits(v);
        Masks {
            punctuation: bits(_mm_or_si128(
                _mm_or_si128(
                    _mm_or_si128(eq(b'{'), eq(b'}')),
                    _mm_or_si128(eq(b'['), eq(b']')),
                ),
                _mm_or_si128(eq(b':'), eq(b',')),
            )),
            quote: bits(eq(b'"')),
            slash: bits(eq(b'\\')),
            space: bits(_mm_or_si128(
                _mm_or_si128(eq(b' '), eq(b'\t')),
                _mm_or_si128(eq(b'\n'), eq(b'\r')),
            )),
            control: bits(_mm_cmplt_epi8(v, _mm_set1_epi8(32))) & !high,
            high,
            digit: bits(_mm_and_si128(
                _mm_cmpgt_epi8(v, _mm_set1_epi8(47)),
                _mm_cmplt_epi8(v, _mm_set1_epi8(58)),
            )),
        }
    }
}
