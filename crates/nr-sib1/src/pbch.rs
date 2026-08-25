use num_complex::Complex32;

use crate::gold;
use crate::polar::decode;
use crate::ssb::{Grid, Re, positions};
use crate::{Crc, Mib, SubcarrierSpacing};

const DMRS: usize = 144;

#[derive(Clone, Copy, Debug)]
pub struct Dmrs {
    pub index: u8,
    pub score: f32,
    pub margin_db: f32,
    pub channel: [Complex32; 4],
}

#[must_use]
pub fn dmrs(pci: u16, index: u8) -> [Complex32; DMRS] {
    let index = u32::from(index);
    let pci = u32::from(pci);
    let initial = (1 << 11) * (index + 1) * (pci / 4 + 1) + (1 << 6) * (index + 1) + pci % 4;
    let sequence = gold(initial, 2 * DMRS);
    let scale = std::f32::consts::FRAC_1_SQRT_2;
    std::array::from_fn(|sample| {
        Complex32::new(
            scale * (1.0 - 2.0 * f32::from(sequence[2 * sample])),
            scale * (1.0 - 2.0 * f32::from(sequence[2 * sample + 1])),
        )
    })
}

#[must_use]
pub fn detect_dmrs(grid: &Grid, pci: u16, candidates: std::ops::Range<u8>) -> Option<Dmrs> {
    let locations = positions(pci, Re::Dmrs);
    let received: Vec<Complex32> = locations
        .iter()
        .map(|(symbol, subcarrier)| grid[*symbol][*subcarrier])
        .collect();
    let energy: f32 = received.iter().map(Complex32::norm_sqr).sum();
    if energy <= f32::EPSILON {
        return None;
    }
    let mut scores: Vec<(u8, f32, [Complex32; 4])> = candidates
        .map(|index| {
            let expected = dmrs(pci, index);
            let mut correlation = [Complex32::default(); 4];
            let mut count = [0_usize; 4];
            for (((symbol, _), sample), reference) in locations.iter().zip(&received).zip(expected)
            {
                correlation[*symbol] += *sample * reference.conj();
                count[*symbol] += 1;
            }
            let score = correlation
                .iter()
                .zip(count)
                .filter(|(_, count)| *count > 0)
                .map(|(value, count)| value.norm_sqr() / count as f32)
                .sum::<f32>()
                / energy;
            let channel = std::array::from_fn(|symbol| {
                if count[symbol] == 0 {
                    Complex32::default()
                } else {
                    correlation[symbol] / count[symbol] as f32
                }
            });
            (index, score, channel)
        })
        .collect();
    if scores.len() < 2 {
        return None;
    }
    scores.sort_by(|left, right| right.1.total_cmp(&left.1));
    Some(Dmrs {
        index: scores[0].0,
        score: scores[0].1,
        margin_db: 10.0 * (scores[0].1 / scores[1].1.max(f32::EPSILON)).log10(),
        channel: scores[0].2,
    })
}

#[must_use]
pub fn decode_codeword(grid: &Grid, pci: u16, reference: &Dmrs, lmax: u8) -> Option<Vec<u8>> {
    if !matches!(lmax, 4 | 8) {
        return None;
    }
    let mut llr = Vec::with_capacity(864);
    for (symbol, subcarrier) in positions(pci, Re::Pbch) {
        let channel = reference.channel[symbol];
        if channel.norm_sqr() <= f32::EPSILON {
            return None;
        }
        let equalized = grid[symbol][subcarrier] / channel;
        llr.push(equalized.re);
        llr.push(equalized.im);
    }
    let phase = if lmax == 4 {
        reference.index % 4
    } else {
        reference.index % 8
    };
    let sequence = gold(u32::from(pci), usize::from(phase) * llr.len() + llr.len());
    for (index, value) in llr.iter_mut().enumerate() {
        if sequence[usize::from(phase) * 864 + index] == 1 {
            *value = -*value;
        }
    }
    decode(&llr, 56, 8, Some(Crc::Crc24C))
}

#[must_use]
pub fn decode_mib(codeword: &[u8], pci: u16, ssb_index: u8) -> Option<Mib> {
    let payload = codeword.get(..32)?;
    let permutation = [
        16, 23, 18, 17, 8, 30, 10, 6, 24, 7, 0, 5, 3, 2, 1, 4, 9, 11, 12, 13, 14, 15, 19, 20, 21,
        22, 25, 26, 27, 28, 29, 31,
    ];
    let protected = [permutation[7], permutation[8], permutation[10]];
    let phase = 2 * payload[permutation[7]] + payload[permutation[8]];
    let sequence = gold(u32::from(pci), 29 * (usize::from(phase) + 1));
    let mut descrambled = [0_u8; 32];
    let mut sequence_index = 29 * usize::from(phase);
    for index in 0..32 {
        descrambled[index] = payload[index];
        if !protected.contains(&index) {
            descrambled[index] ^= sequence[sequence_index];
            sequence_index += 1;
        }
    }
    let mut expanded = [0_u8; 32];
    let mut frame = 0;
    let mut location = 11;
    let mut other = 14;
    for (index, bit) in expanded.iter_mut().enumerate() {
        let source = if matches!(index, 1..=6 | 24..=27) {
            let source = frame;
            frame += 1;
            source
        } else if index == 28 {
            10
        } else if index >= 29 {
            let source = location;
            location += 1;
            source
        } else {
            let source = other;
            other += 1;
            source
        };
        *bit = descrambled[permutation[source]];
    }
    if expanded[0] != 0 {
        return None;
    }
    let system_frame = bits(&expanded[1..7]) as u16 * 16 + bits(&expanded[24..28]) as u16;
    let spacing = if expanded[7] == 0 {
        SubcarrierSpacing::Khz15
    } else {
        SubcarrierSpacing::Khz30
    };
    let offset = bits(&expanded[8..12]) as u8 + 16 * expanded[29];
    let pdcch = bits(&expanded[13..21]) as u8;
    Some(Mib {
        pci,
        system_frame,
        half_frame: expanded[28] == 1,
        subcarrier_spacing_common: spacing,
        ssb_subcarrier_offset: offset,
        dmrs_type_a_position: 2 + expanded[12],
        pdcch_config_sib1: pdcch,
        cell_barred: expanded[21] == 0,
        intra_frequency_reselection: expanded[22] == 0,
        ssb_index,
    })
}

fn bits(input: &[u8]) -> u32 {
    input
        .iter()
        .fold(0, |value, bit| 2 * value + u32::from(*bit))
}

#[cfg(test)]
mod tests {
    use super::{decode_codeword, decode_mib, detect_dmrs, dmrs};
    use crate::ssb::{Re, positions};

    #[test]
    fn each_reference_hypothesis_is_distinct_and_normalized() {
        let first = dmrs(319, 0);
        for index in 0..8 {
            let sequence = dmrs(319, index);
            assert!(
                sequence
                    .iter()
                    .all(|sample| (sample.norm_sqr() - 1.0).abs() < 1e-6)
            );
            if index > 0 {
                assert_ne!(first, sequence);
            }
        }
    }

    #[test]
    fn reference_search_finds_the_inserted_index_and_channel() {
        let pci = 319;
        let expected = dmrs(pci, 5);
        let channel = num_complex::Complex32::new(0.4, -0.7);
        let mut grid = [[num_complex::Complex32::default(); 240]; 4];
        for ((symbol, subcarrier), reference) in positions(pci, Re::Dmrs).into_iter().zip(expected)
        {
            grid[symbol][subcarrier] = reference * channel;
        }
        let found = detect_dmrs(&grid, pci, 0..8).unwrap();
        assert_eq!(found.index, 5);
        assert!((found.score - 1.0).abs() < 1e-4);
        for symbol in [1, 2, 3] {
            assert!((found.channel[symbol] - channel).norm() < 1e-4);
        }
        assert!(found.margin_db > 10.0);
    }

    #[test]
    fn invalid_ssb_cardinality_is_refused() {
        let grid = [[num_complex::Complex32::default(); 240]; 4];
        let reference = super::Dmrs {
            index: 0,
            score: 1.0,
            margin_db: 10.0,
            channel: [num_complex::Complex32::new(1.0, 0.0); 4],
        };
        assert_eq!(decode_codeword(&grid, 1, &reference, 64), None);
        assert!(decode_mib(&[], 1, 0).is_none());
    }
}
