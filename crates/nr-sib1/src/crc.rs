#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Crc {
    Crc16,
    Crc24A,
    Crc24B,
    Crc24C,
}

impl Crc {
    #[must_use]
    pub const fn width(self) -> u8 {
        match self {
            Self::Crc16 => 16,
            Self::Crc24A | Self::Crc24B | Self::Crc24C => 24,
        }
    }

    #[must_use]
    pub fn remainder(self, bits: &[u8]) -> u32 {
        self.remainder_from(bits, 0)
    }

    #[must_use]
    pub fn check_dci(self, bits: &[u8], rnti: u16) -> bool {
        let width = usize::from(self.width());
        if bits.len() < width {
            return false;
        }
        let payload = &bits[..bits.len() - width];
        let received = bits[bits.len() - width..]
            .iter()
            .fold(0, |value, bit| 2 * value + u32::from(*bit));
        self.remainder_from(payload, (1_u32 << self.width()) - 1) ^ u32::from(rnti) == received
    }

    fn remainder_from(self, bits: &[u8], initial: u32) -> u32 {
        let width = self.width();
        let top = 1_u32 << (width - 1);
        let mask = (1_u32 << width) - 1;
        let polynomial = match self {
            Self::Crc16 => 0x1021,
            Self::Crc24A => 0x0086_4cfb,
            Self::Crc24B => 0x0080_0063,
            Self::Crc24C => 0x00b2_b117,
        };
        let mut value = initial;
        for bit in bits
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0, width.into()))
        {
            let feedback = value & top != 0;
            value = ((value << 1) | u32::from(bit & 1)) & mask;
            if feedback {
                value ^= polynomial;
            }
        }
        value
    }

    #[must_use]
    pub fn check(self, bits: &[u8]) -> bool {
        self.syndrome(bits) == 0
    }

    #[must_use]
    pub fn syndrome(self, bits: &[u8]) -> u32 {
        let width = self.width();
        let top = 1_u32 << (width - 1);
        let mask = (1_u32 << width) - 1;
        let polynomial = match self {
            Self::Crc16 => 0x1021,
            Self::Crc24A => 0x0086_4cfb,
            Self::Crc24B => 0x0080_0063,
            Self::Crc24C => 0x00b2_b117,
        };
        bits.iter().fold(0_u32, |mut value, bit| {
            let feedback = value & top != 0;
            value = ((value << 1) | u32::from(bit & 1)) & mask;
            if feedback {
                value ^= polynomial;
            }
            value
        })
    }

    #[must_use]
    pub fn append(self, bits: &[u8]) -> Vec<u8> {
        let remainder = self.remainder(bits);
        let mut encoded = Vec::with_capacity(bits.len() + usize::from(self.width()));
        encoded.extend(bits.iter().map(|bit| bit & 1));
        encoded.extend(
            (0..self.width())
                .rev()
                .map(|shift| ((remainder >> shift) & 1) as u8),
        );
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::Crc;

    #[test]
    fn each_nr_crc_accepts_its_appended_remainder() {
        let payload: Vec<u8> = (0..73)
            .map(|index| u8::from(index % 5 == 1 || index % 7 == 0))
            .collect();
        for crc in [Crc::Crc16, Crc::Crc24A, Crc::Crc24B, Crc::Crc24C] {
            let mut encoded = crc.append(&payload);
            assert!(crc.check(&encoded));
            encoded[17] ^= 1;
            assert!(!crc.check(&encoded));
        }
    }

    #[test]
    fn crc_variants_do_not_alias() {
        let payload: Vec<u8> = (0..96).map(|index| (index & 1) as u8).collect();
        assert_ne!(
            Crc::Crc24A.remainder(&payload),
            Crc::Crc24B.remainder(&payload)
        );
        assert_ne!(
            Crc::Crc24A.remainder(&payload),
            Crc::Crc24C.remainder(&payload)
        );
        assert_ne!(
            Crc::Crc24B.remainder(&payload),
            Crc::Crc24C.remainder(&payload)
        );
    }
}
