use crate::crc::Crc;
use crate::polar_tables::{INPUT, RELIABILITY, SUBBLOCK};

#[derive(Clone, Debug)]
struct Path {
    llr: Vec<f32>,
    bits: Vec<i8>,
    metric: f32,
    size: usize,
    depth: usize,
}

impl Path {
    fn new(channel: &[f32]) -> Self {
        let size = channel.len();
        let depth = size.ilog2() as usize;
        let mut llr = vec![0.0; (depth + 1) * size];
        for index in 0..size {
            llr[index] = channel[reverse(index, depth)];
        }
        Self {
            llr,
            bits: vec![-1; (depth + 1) * size],
            metric: 0.0,
            size,
            depth,
        }
    }

    fn offset(&self, layer: usize, branch: usize, phase: usize) -> usize {
        layer * self.size + branch * (1 << layer) + phase
    }

    fn llr(&self, layer: usize, branch: usize, phase: usize) -> f32 {
        self.llr[self.offset(layer, branch, phase)]
    }

    fn set_llr(&mut self, value: f32, layer: usize, branch: usize, phase: usize) {
        let offset = self.offset(layer, branch, phase);
        self.llr[offset] = value;
    }

    fn bit(&self, layer: usize, branch: usize, phase: usize) -> u8 {
        self.bits[self.offset(layer, branch, phase)] as u8
    }

    fn set_bit(&mut self, value: u8, layer: usize, branch: usize, phase: usize) {
        let offset = self.offset(layer, branch, phase);
        self.bits[offset] = value as i8;
    }

    fn decide(&mut self, phase: usize, bit: u8) {
        let value = self.llr(self.depth, 0, phase);
        if bit != u8::from(value < 0.0) {
            self.metric += value.abs();
        }
        self.set_bit(bit, self.depth, 0, phase);
    }

    fn information(&self, positions: &[usize]) -> Vec<u8> {
        positions
            .iter()
            .map(|phase| self.bit(self.depth, 0, *phase))
            .collect()
    }
}

#[must_use]
pub fn mother_length(k: usize, e: usize) -> Option<usize> {
    if k == 0 || e == 0 || k > e {
        return None;
    }
    let ceiling = e.next_power_of_two().ilog2() as usize;
    let first = if e <= 9 * (1 << ceiling.saturating_sub(1)) / 8 && 16 * k < 9 * e {
        ceiling.saturating_sub(1)
    } else {
        ceiling
    };
    let second = (8 * k).next_power_of_two().ilog2() as usize;
    Some(1 << first.min(second).clamp(5, 9))
}

#[must_use]
pub fn input_pattern(k: usize) -> Option<Vec<usize>> {
    if k > INPUT.len() {
        return None;
    }
    let skipped = INPUT.len() - k;
    Some(
        INPUT
            .iter()
            .copied()
            .filter(|position| usize::from(*position) >= skipped)
            .map(|position| usize::from(position) - skipped)
            .collect(),
    )
}

fn positions(k: usize, e: usize, size: usize) -> Option<Vec<usize>> {
    let block = size / SUBBLOCK.len();
    let mapped: Vec<usize> = (0..size)
        .map(|index| usize::from(SUBBLOCK[32 * index / size]) * block + index % block)
        .collect();
    let mut barred = vec![false; size];
    if e < size {
        if 16 * k <= 7 * e {
            for position in &mapped[..size - e] {
                barred[*position] = true;
            }
            let limit = if 4 * e >= 3 * size {
                (3 * size).div_ceil(4).saturating_sub(e / 2)
            } else {
                (9 * size).div_ceil(16).saturating_sub(e / 4)
            };
            barred[..limit].fill(true);
        } else {
            for position in &mapped[e..] {
                barred[*position] = true;
            }
        }
    }
    let mut selected = Vec::with_capacity(k);
    for position in RELIABILITY
        .iter()
        .copied()
        .filter(|position| usize::from(*position) < size)
        .rev()
    {
        let position = usize::from(position);
        if !barred[position] {
            selected.push(position);
            if selected.len() == k {
                selected.sort_unstable();
                return Some(selected);
            }
        }
    }
    None
}

#[must_use]
pub fn encode(bits: &[u8], e: usize) -> Option<Vec<u8>> {
    let k = bits.len();
    let size = mother_length(k, e)?;
    let information = positions(k, e, size)?;
    let pattern = input_pattern(k)?;
    let interleaved: Vec<u8> = pattern.iter().map(|position| bits[*position] & 1).collect();
    let mut codeword = vec![0_u8; size];
    for (position, bit) in information.iter().zip(interleaved) {
        codeword[*position] = bit;
    }
    let mut stride = 1;
    while stride < size {
        for start in (0..size).step_by(2 * stride) {
            for index in start..start + stride {
                codeword[index] ^= codeword[index + stride];
            }
        }
        stride *= 2;
    }
    Some(codeword)
}

#[must_use]
pub fn rate_match(codeword: &[u8], k: usize, e: usize) -> Option<Vec<u8>> {
    let size = codeword.len();
    if size < 32 || !size.is_power_of_two() || k > e {
        return None;
    }
    let block = size / SUBBLOCK.len();
    let interleaved: Vec<u8> = (0..size)
        .map(|index| codeword[usize::from(SUBBLOCK[32 * index / size]) * block + index % block])
        .collect();
    Some(if e >= size {
        (0..e).map(|index| interleaved[index % size]).collect()
    } else if 16 * k <= 7 * e {
        interleaved[size - e..].to_vec()
    } else {
        interleaved[..e].to_vec()
    })
}

#[must_use]
pub fn rate_recover(input: &[f32], k: usize, size: usize) -> Option<Vec<f32>> {
    let e = input.len();
    if size < 32 || !size.is_power_of_two() || k > e {
        return None;
    }
    let mut selected = vec![0.0; size];
    if e >= size {
        for (index, value) in input.iter().enumerate() {
            selected[index % size] += value;
        }
    } else if 16 * k <= 7 * e {
        selected[size - e..].copy_from_slice(input);
    } else {
        selected[..e].copy_from_slice(input);
        selected[e..].fill(f32::INFINITY);
    }
    let block = size / SUBBLOCK.len();
    let mut recovered = vec![0.0; size];
    for (index, value) in selected.into_iter().enumerate() {
        let position = usize::from(SUBBLOCK[32 * index / size]) * block + index % block;
        recovered[position] = value;
    }
    Some(recovered)
}

#[must_use]
pub fn decode(input: &[f32], k: usize, list: usize, crc: Option<Crc>) -> Option<Vec<u8>> {
    decode_with(input, k, list, |bits| {
        crc.is_none_or(|check| check.check(bits))
    })
}

#[must_use]
pub fn decode_masked(input: &[f32], k: usize, list: usize, crc: Crc, mask: u16) -> Option<Vec<u8>> {
    decode_with(input, k, list, |bits| crc.check_dci(bits, mask))
}

fn decode_with(
    input: &[f32],
    k: usize,
    list: usize,
    valid: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let e = input.len();
    let size = mother_length(k, e)?;
    let channel = rate_recover(input, k, size)?;
    let information = positions(k, e, size)?;
    let mut frozen = vec![true; size];
    for position in &information {
        frozen[*position] = false;
    }
    let mut paths = vec![Path::new(&channel)];
    for (phase, is_frozen) in frozen.into_iter().enumerate() {
        for path in &mut paths {
            calculate(path, path.depth, phase);
        }
        if is_frozen {
            for path in &mut paths {
                path.decide(phase, 0);
            }
        } else {
            let mut branches = Vec::with_capacity(paths.len() * 2);
            for path in paths {
                let mut one = path.clone();
                let mut zero = path;
                zero.decide(phase, 0);
                one.decide(phase, 1);
                branches.push(zero);
                branches.push(one);
            }
            branches.sort_by(|left, right| left.metric.total_cmp(&right.metric));
            branches.truncate(list.max(1));
            paths = branches;
        }
        if phase % 2 == 1 {
            for path in &mut paths {
                update(path, path.depth, phase);
            }
        }
    }
    paths.sort_by(|left, right| left.metric.total_cmp(&right.metric));
    let inverse = inverse(&input_pattern(k)?);
    paths.into_iter().find_map(|path| {
        let interleaved = path.information(&information);
        let decoded: Vec<u8> = inverse
            .iter()
            .map(|position| interleaved[*position])
            .collect();
        if valid(&decoded) { Some(decoded) } else { None }
    })
}

fn calculate(path: &mut Path, layer: usize, phase: usize) {
    if layer == 0 {
        return;
    }
    let next = phase / 2;
    if phase.is_multiple_of(2) {
        calculate(path, layer - 1, next);
    }
    for branch in 0..1 << (path.depth - layer) {
        let left = path.llr(layer - 1, 2 * branch, next);
        let right = path.llr(layer - 1, 2 * branch + 1, next);
        let value = if phase.is_multiple_of(2) {
            left.signum() * right.signum() * left.abs().min(right.abs())
        } else {
            let bit = path.bit(layer, branch, phase - 1);
            right + if bit == 0 { left } else { -left }
        };
        path.set_llr(value, layer, branch, phase);
    }
}

fn update(path: &mut Path, layer: usize, phase: usize) {
    let next = phase / 2;
    for branch in 0..1 << (path.depth - layer) {
        let left = path.bit(layer, branch, phase - 1);
        let right = path.bit(layer, branch, phase);
        path.set_bit(left ^ right, layer - 1, 2 * branch, next);
        path.set_bit(right, layer - 1, 2 * branch + 1, next);
    }
    if next % 2 == 1 {
        update(path, layer - 1, next);
    }
}

fn inverse(pattern: &[usize]) -> Vec<usize> {
    let mut inverse = vec![0; pattern.len()];
    for (index, position) in pattern.iter().enumerate() {
        inverse[*position] = index;
    }
    inverse
}

fn reverse(value: usize, width: usize) -> usize {
    value.reverse_bits() >> (usize::BITS as usize - width)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, input_pattern, mother_length, rate_match};
    use crate::polar_tables::{INPUT, RELIABILITY, SUBBLOCK};

    #[test]
    fn normative_tables_are_complete_permutations() {
        for (values, length) in [
            (&RELIABILITY[..], 1024),
            (&INPUT[..], 164),
            (&SUBBLOCK[..], 32),
        ] {
            let mut sorted = values.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..length).collect::<Vec<_>>());
        }
    }

    #[test]
    fn input_interleaving_is_invertible_for_every_supported_length() {
        for length in 1..=164 {
            let pattern = input_pattern(length).unwrap();
            let mut sorted = pattern;
            sorted.sort_unstable();
            assert_eq!(sorted, (0..length).collect::<Vec<_>>());
        }
    }

    #[test]
    fn mother_lengths_match_reference_cases() {
        assert_eq!(mother_length(20, 300), Some(256));
        assert_eq!(mother_length(200, 400), Some(512));
        assert_eq!(mother_length(456, 1200), Some(512));
        assert_eq!(mother_length(56, 864), Some(512));
    }

    #[test]
    fn encoder_matches_independent_matlab_vector() {
        let input = include_bytes!("../tests/fixtures/polar/k40-e64-input.bin");
        let expected = include_bytes!("../tests/fixtures/polar/k40-e64-codeword.bin");
        assert_eq!(encode(input, 64).unwrap(), expected.as_slice());
    }

    /// A DCI on an aggregation level of 8 has more coded bits than the mother code has room for, so
    /// rate matching repeats it and the decoder has to fold the repeats back together. An aggregation
    /// level of 4 shortens instead, which is a different branch, so exercise both.
    #[test]
    fn every_rate_matching_branch_round_trips_a_control_payload() {
        for coded in [432, 864, 1728] {
            let bits: Vec<u8> = (0..63).map(|index| u8::from(index % 3 == 0)).collect();
            let codeword = encode(&bits, coded).unwrap();
            let matched = rate_match(&codeword, 63, coded).unwrap();
            assert_eq!(matched.len(), coded);
            let llr: Vec<f32> = matched
                .iter()
                .map(|bit| if *bit == 0 { 20.0 } else { -20.0 })
                .collect();
            assert_eq!(decode(&llr, 63, 8, None).as_deref(), Some(bits.as_slice()));
        }
    }

    #[test]
    fn list_decoder_recovers_a_noise_free_matlab_vector() {
        let input = include_bytes!("../tests/fixtures/polar/k40-e64-input.bin");
        let codeword = include_bytes!("../tests/fixtures/polar/k40-e64-codeword.bin");
        let matched = rate_match(codeword, 40, 64).unwrap();
        let llr: Vec<f32> = matched
            .iter()
            .map(|bit| if *bit == 0 { 20.0 } else { -20.0 })
            .collect();
        assert_eq!(decode(&llr, 40, 8, None).unwrap(), input.as_slice());
    }
}
