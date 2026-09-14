//! CSI parameter accumulator.
//!
//! Fixed-capacity and allocation-free: this is on the per-byte path, and
//! CLAUDE.md §5.2 requires "no string matching, no allocation" there.

/// xterm's limit. A sequence with more parameters than this is malformed, and
/// the parser marks the dispatch `ignore` rather than growing.
pub const MAX_PARAMS: usize = 32;

#[derive(Clone, Debug)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    len: usize,
    /// True once a digit has been seen for the parameter being built, so that
    /// `CSI ;5H` yields [0, 5] — an omitted parameter is 0, which is what the
    /// "default" rules in ECMA-48 rest on.
    started: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            values: [0; MAX_PARAMS],
            len: 0,
            started: false,
        }
    }
}

impl Params {
    pub fn clear(&mut self) {
        self.len = 0;
        self.started = false;
    }

    /// Feed one byte in `0-9`, `;` or `:`. Returns false if the parameter list
    /// is full, which the caller turns into `ignore = true`.
    ///
    /// `:` is treated as a separator — see the parser header for why.
    pub fn push_byte(&mut self, b: u8) -> bool {
        if b == b';' || b == b':' {
            if !self.started && self.len < MAX_PARAMS {
                // An empty slot before a separator is a real, zero-valued param.
                self.values[self.len] = 0;
                self.len += 1;
            }
            self.started = false;
            return self.len < MAX_PARAMS;
        }

        let digit = (b - b'0') as u16;
        if !self.started {
            if self.len >= MAX_PARAMS {
                return false;
            }
            self.values[self.len] = 0;
            self.len += 1;
            self.started = true;
        }
        let slot = &mut self.values[self.len - 1];
        // Saturate rather than wrap: `CSI 99999999999m` must not become a small
        // number that happens to be a valid attribute.
        *slot = slot.saturating_mul(10).saturating_add(digit);
        true
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Parameter at `i`, or `default` when absent or zero.
    ///
    /// ECMA-48 treats an omitted or zero parameter as "use the default", and
    /// nearly every CSI handler wants that rule rather than the raw value.
    #[inline]
    pub fn get_or(&self, i: usize, default: u16) -> u16 {
        match self.values.get(i) {
            Some(&v) if i < self.len && v != 0 => v,
            _ => default,
        }
    }

    /// Raw value at `i`, zero if absent. Use for SGR, where 0 is meaningful.
    #[inline]
    pub fn raw(&self, i: usize) -> u16 {
        if i < self.len {
            self.values[i]
        } else {
            0
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.values[..self.len].iter().copied()
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.values[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<u16> {
        let mut p = Params::default();
        for b in s.bytes() {
            p.push_byte(b);
        }
        p.as_slice().to_vec()
    }

    #[test]
    fn simple_list() {
        assert_eq!(parse("1;31"), vec![1, 31]);
    }

    #[test]
    fn leading_empty_param_is_zero() {
        assert_eq!(parse(";5"), vec![0, 5]);
    }

    #[test]
    fn interior_empty_params_are_zero() {
        assert_eq!(parse("38:2::255:0:0"), vec![38, 2, 0, 255, 0, 0]);
    }

    #[test]
    fn defaults_apply_to_zero_and_missing() {
        let mut p = Params::default();
        for b in b"0;7" {
            p.push_byte(*b);
        }
        assert_eq!(p.get_or(0, 1), 1); // zero -> default
        assert_eq!(p.get_or(1, 1), 7);
        assert_eq!(p.get_or(9, 1), 1); // missing -> default
        assert_eq!(p.raw(0), 0); // but SGR still sees the real 0
    }

    #[test]
    fn huge_values_saturate() {
        assert_eq!(parse("99999999999"), vec![u16::MAX]);
    }

    #[test]
    fn overflowing_the_list_is_reported() {
        let mut p = Params::default();
        let mut ok = true;
        for _ in 0..MAX_PARAMS + 5 {
            ok &= p.push_byte(b'1');
            ok &= p.push_byte(b';');
        }
        assert!(!ok);
        assert!(p.len() <= MAX_PARAMS);
    }
}
