use num_complex::Complex32;

#[must_use]
pub fn pss(nid2: u8) -> Option<[i8; 127]> {
    if nid2 > 2 {
        return None;
    }
    let mut state = [0_u8; 134];
    state[..7].copy_from_slice(&[0, 1, 1, 0, 1, 1, 1]);
    for index in 0..127 {
        state[index + 7] = state[index + 4] ^ state[index];
    }
    Some(std::array::from_fn(|index| {
        1 - 2 * state[(index + 43 * usize::from(nid2)) % 127] as i8
    }))
}

#[must_use]
pub fn sss(nid1: u16, nid2: u8) -> Option<[i8; 127]> {
    if nid1 > 335 || nid2 > 2 {
        return None;
    }
    let mut first = [0_u8; 134];
    let mut second = [0_u8; 134];
    first[0] = 1;
    second[0] = 1;
    for index in 0..127 {
        first[index + 7] = first[index + 4] ^ first[index];
        second[index + 7] = second[index + 1] ^ second[index];
    }
    let m0 = 15 * usize::from(nid1 / 112) + 5 * usize::from(nid2);
    let m1 = usize::from(nid1 % 112);
    Some(std::array::from_fn(|index| {
        (1 - 2 * first[(index + m0) % 127] as i8) * (1 - 2 * second[(index + m1) % 127] as i8)
    }))
}

#[must_use]
pub const fn pci(nid1: u16, nid2: u8) -> Option<u16> {
    if nid1 > 335 || nid2 > 2 {
        None
    } else {
        Some(3 * nid1 + nid2 as u16)
    }
}

#[must_use]
pub const fn identity(pci: u16) -> Option<(u16, u8)> {
    if pci > 1007 {
        None
    } else {
        Some((pci / 3, (pci % 3) as u8))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Pss {
    pub nid2: u8,
    pub score: f32,
    pub margin_db: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Sss {
    pub nid1: u16,
    pub pci: u16,
    pub score: f32,
    pub margin_db: f32,
}

#[must_use]
pub fn detect_pss(symbol: &[Complex32], ssb_center: isize) -> Option<Pss> {
    let centre = isize::try_from(symbol.len() / 2).ok()? + ssb_center;
    let start = usize::try_from(centre - 64).ok()?;
    let received = symbol.get(start..start + 127)?;
    let energy: f32 = received.iter().map(Complex32::norm_sqr).sum();
    if energy <= f32::EPSILON {
        return None;
    }
    let mut candidates: Vec<(u8, f32)> = (0..3)
        .map(|nid2| {
            let expected = pss(nid2).unwrap();
            let correlation: Complex32 = received
                .iter()
                .zip(expected)
                .map(|(sample, bit)| *sample * f32::from(bit))
                .sum();
            (nid2, correlation.norm_sqr() / (127.0 * energy))
        })
        .collect();
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    let margin_db = 10.0 * (candidates[0].1 / candidates[1].1.max(f32::EPSILON)).log10();
    Some(Pss {
        nid2: candidates[0].0,
        score: candidates[0].1,
        margin_db,
    })
}

#[must_use]
pub fn detect_sss(symbol: &[Complex32], ssb_center: isize, nid2: u8) -> Option<Sss> {
    if nid2 > 2 {
        return None;
    }
    let centre = isize::try_from(symbol.len() / 2).ok()? + ssb_center;
    let start = usize::try_from(centre - 64).ok()?;
    let received = symbol.get(start..start + 127)?;
    let energy: f32 = received.iter().map(Complex32::norm_sqr).sum();
    if energy <= f32::EPSILON {
        return None;
    }
    let mut candidates: Vec<(u16, f32)> = (0..=335)
        .map(|nid1| {
            let expected = sss(nid1, nid2).unwrap();
            let correlation: Complex32 = received
                .iter()
                .zip(expected)
                .map(|(sample, bit)| *sample * f32::from(bit))
                .sum();
            (nid1, correlation.norm_sqr() / (127.0 * energy))
        })
        .collect();
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    let margin_db = 10.0 * (candidates[0].1 / candidates[1].1.max(f32::EPSILON)).log10();
    Some(Sss {
        nid1: candidates[0].0,
        pci: pci(candidates[0].0, nid2).unwrap(),
        score: candidates[0].1,
        margin_db,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use num_complex::Complex32;

    use super::{detect_pss, detect_sss, identity, pci, pss, sss};

    #[test]
    fn physical_cell_identity_round_trips() {
        for expected in 0..=1007 {
            let (nid1, nid2) = identity(expected).unwrap();
            assert_eq!(pci(nid1, nid2), Some(expected));
        }
        assert_eq!(identity(1008), None);
    }

    #[test]
    fn all_pss_sequences_are_distinct_binary_bipolar_codes() {
        let codes: BTreeSet<_> = (0..3).map(|nid2| pss(nid2).unwrap()).collect();
        assert_eq!(codes.len(), 3);
        assert!(codes.iter().flatten().all(|value| matches!(value, -1 | 1)));
        assert_eq!(pss(3), None);
    }

    #[test]
    fn all_sss_sequences_address_the_full_pci_space() {
        let codes: BTreeSet<_> = (0..=335)
            .flat_map(|nid1| (0..3).map(move |nid2| sss(nid1, nid2).unwrap()))
            .collect();
        assert_eq!(codes.len(), 1008);
        assert!(codes.iter().flatten().all(|value| matches!(value, -1 | 1)));
        assert_eq!(sss(336, 0), None);
    }

    #[test]
    fn frequency_domain_search_finds_the_inserted_pss() {
        let mut symbol = vec![Complex32::default(); 512];
        for (index, bit) in pss(2).unwrap().iter().enumerate() {
            symbol[256 - 64 + index] =
                Complex32::new(0.3 * f32::from(*bit), -0.7 * f32::from(*bit));
        }
        let found = detect_pss(&symbol, 0).unwrap();
        assert_eq!(found.nid2, 2);
        assert!(
            (found.score - 1.0).abs() < 1e-4,
            "normalized score was {}",
            found.score
        );
        assert!(found.margin_db > 10.0);
    }

    #[test]
    fn frequency_domain_search_finds_the_inserted_sss() {
        let mut symbol = vec![Complex32::default(); 512];
        for (index, bit) in sss(271, 1).unwrap().iter().enumerate() {
            symbol[256 - 64 + index] =
                Complex32::new(-0.2 * f32::from(*bit), 0.8 * f32::from(*bit));
        }
        let found = detect_sss(&symbol, 0, 1).unwrap();
        assert_eq!((found.nid1, found.pci), (271, 814));
        assert!((found.score - 1.0).abs() < 1e-4);
        assert!(found.margin_db > 10.0);
    }
}
