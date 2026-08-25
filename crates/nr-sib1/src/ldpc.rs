use crate::ldpc_tables::{BG1, BG2, LIFTING};
use crate::{Crc, Reason};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaseGraph {
    One,
    Two,
}

impl BaseGraph {
    /// Systematic and transmitted column counts of TS 38.212 section 5.3.2. The first two systematic
    /// columns never reach the air, so the circular buffer is two columns shorter than the graph.
    const fn shape(self) -> (usize, usize) {
        match self {
            Self::One => (22, 66),
            Self::Two => (10, 50),
        }
    }

    /// Redundancy version starting positions of TS 38.212 Table 5.4.2.1-2, in lifting sizes.
    const fn starts(self) -> [usize; 4] {
        match self {
            Self::One => [0, 17, 33, 56],
            Self::Two => [0, 13, 25, 43],
        }
    }
}

/// Base graph and lifting size of TS 38.212 sections 7.2.2 and 5.2.2, for a transport block that
/// fits a single code block.
#[must_use]
pub fn segment(size: usize, rate: f64) -> Option<(BaseGraph, usize)> {
    let graph = if size <= 292 || rate <= 0.25 || (size <= 3824 && rate <= 0.67) {
        BaseGraph::Two
    } else {
        BaseGraph::One
    };
    let bits = size + if size > 3824 { 24 } else { 16 };
    let (systematic, limit) = match graph {
        BaseGraph::One => (22, 8448),
        BaseGraph::Two => (
            match bits {
                ..=192 => 6,
                193..=560 => 8,
                561..=640 => 9,
                _ => 10,
            },
            3840,
        ),
    };
    if bits > limit {
        return None;
    }
    LIFTING
        .iter()
        .flat_map(|set| set.iter())
        .map(|lifting| usize::from(*lifting))
        .filter(|lifting| systematic * lifting >= bits)
        .min()
        .map(|lifting| (graph, lifting))
}

#[must_use]
pub fn recover(
    input: &[f32],
    transport_bits: usize,
    lifting: usize,
    modulation: usize,
    graph: BaseGraph,
    redundancy: u8,
) -> Option<Vec<f32>> {
    let mut buffer = vec![0.0; graph.shape().1 * lifting];
    let null = spread(
        &mut buffer,
        input,
        transport_bits,
        lifting,
        modulation,
        graph,
        redundancy,
    )?;
    Some(seal(buffer, &null, lifting))
}

/// Adds one transmission's soft bits to `buffer` at the circular-buffer positions its redundancy version
/// selected, and returns which positions are filler rather than transmitted.
///
/// Separate from `recover` so that several transmissions of the same block can land in one buffer. The
/// filler positions are returned rather than written, because a known bit written once is knowledge and
/// the same bit written four times is four times the confidence in something that was never sent.
fn spread(
    buffer: &mut [f32],
    input: &[f32],
    transport_bits: usize,
    lifting: usize,
    modulation: usize,
    graph: BaseGraph,
    redundancy: u8,
) -> Option<Vec<bool>> {
    if modulation == 0 || !input.len().is_multiple_of(modulation) {
        return None;
    }
    let (systematic, columns) = graph.shape();
    let information = systematic * lifting;
    let transmitted = columns * lifting;
    if transport_bits > information || buffer.len() != transmitted {
        return None;
    }
    let mut null = vec![false; transmitted];
    null[transport_bits.saturating_sub(2 * lifting)..information - 2 * lifting].fill(true);
    let mut selected = Vec::with_capacity(input.len());
    let mut index = *graph.starts().get(usize::from(redundancy))? * lifting;
    while selected.len() < input.len() {
        let position = index % transmitted;
        if !null[position] {
            selected.push(position);
        }
        index += 1;
    }
    let chunk = input.len() / modulation;
    for (index, value) in input.iter().enumerate() {
        let selected_index = (index % modulation) * chunk + index / modulation;
        buffer[selected[selected_index]] += value;
    }
    Some(null)
}

fn seal(mut buffer: Vec<f32>, null: &[bool], lifting: usize) -> Vec<f32> {
    for (value, is_null) in buffer.iter_mut().zip(null) {
        if *is_null {
            *value = 1e20;
        }
    }
    let mut output = vec![0.0; 2 * lifting];
    output.extend(buffer);
    output
}

/// One transport block's soft bits, accumulated across the occasions that carried it.
///
/// SIB1 repeats every twenty milliseconds with its redundancy version cycling, and those versions are
/// starting positions in one circular buffer rather than four independent codewords, so accumulating
/// them is what the rate matching is defined against. Decoding each occasion on its own throws away
/// every occasion but the one it is reading.
///
/// It does not make every window readable. RV 1 and RV 2 begin past the systematic bits, and at the code
/// rates a broadcast uses neither wraps far enough back onto them for the pair to cover the block, so a
/// window holding only those two still cannot be read. Which sets can is measured in
/// `pdsch::tests::occasions_that_cannot_be_read_alone_combine_into_one_that_can` rather than stated here,
/// because it tightens with the code rate and the obvious summaries of it are all wrong.
pub struct Combine {
    graph: BaseGraph,
    lifting: usize,
    block: usize,
    size: usize,
    buffer: Vec<f32>,
    null: Vec<bool>,
    occasions: usize,
}

impl Combine {
    pub fn new(size: usize, rate: f64) -> Result<Self, Reason> {
        if size > 3824 {
            return Err(Reason::TransportSize);
        }
        let (graph, lifting) = segment(size, rate).ok_or(Reason::TransportSize)?;
        if graph == BaseGraph::One {
            return Err(Reason::BaseGraph { size, rate });
        }
        Ok(Self {
            graph,
            lifting,
            block: size + 16,
            size,
            buffer: vec![0.0; graph.shape().1 * lifting],
            null: Vec::new(),
            occasions: 0,
        })
    }

    /// Whether this accumulator holds the same transport block a later occasion is offering. Combining
    /// soft bits from two different blocks would corrupt both.
    #[must_use]
    pub fn holds(&self, size: usize, rate: f64) -> bool {
        self.size == size && segment(size, rate) == Some((self.graph, self.lifting))
    }

    pub fn add(&mut self, input: &[f32], modulation: usize, redundancy: u8) -> Result<(), Reason> {
        self.null = spread(
            &mut self.buffer,
            input,
            self.block,
            self.lifting,
            modulation,
            self.graph,
            redundancy,
        )
        .ok_or(Reason::Undecoded)?;
        self.occasions += 1;
        Ok(())
    }

    #[must_use]
    pub fn occasions(&self) -> usize {
        self.occasions
    }

    pub fn read(&self) -> Result<Vec<u8>, Reason> {
        let sealed = seal(self.buffer.clone(), &self.null, self.lifting);
        let decoded = decode(&sealed, self.lifting, 100, self.graph).ok_or(Reason::Undecoded)?;
        let transport = decoded.get(..self.block).ok_or(Reason::Undecoded)?;
        if Crc::Crc16.check(transport) {
            Ok(bytes(&transport[..self.size]))
        } else {
            Err(Reason::Undecoded)
        }
    }
}

/// Packs a transport block's bits into the octets the RRC parser reads.
#[must_use]
pub fn bytes(bits: &[u8]) -> Vec<u8> {
    bits.as_chunks::<8>()
        .0
        .iter()
        .map(|byte| byte.iter().fold(0, |value, bit| 2 * value + *bit))
        .collect()
}

#[must_use]
pub fn decode(
    input: &[f32],
    lifting: usize,
    iterations: usize,
    graph: BaseGraph,
) -> Option<Vec<u8>> {
    let (base, row_blocks, column_blocks, information_blocks) = match graph {
        BaseGraph::One => (BG1, 46, 68, 22),
        BaseGraph::Two => (BG2, 42, 52, 10),
    };
    if input.len() != column_blocks * lifting {
        return None;
    }
    let set = LIFTING
        .iter()
        .position(|values| values.contains(&(lifting as u16)))?;
    let checks = row_blocks * lifting;
    let mut rows = vec![Vec::new(); checks];
    let mut variables = Vec::with_capacity(base.len() * lifting);
    for (row, column, shifts) in base {
        let shift = usize::from(shifts[set]) % lifting;
        for index in 0..lifting {
            let edge = variables.len();
            rows[usize::from(*row) * lifting + index].push(edge);
            variables.push(usize::from(*column) * lifting + (index + shift) % lifting);
        }
    }
    let mut belief = input.to_vec();
    let mut message = vec![0.0_f32; variables.len()];
    for _ in 0..iterations {
        for edges in &rows {
            let mut sign = false;
            let mut minimum = f32::INFINITY;
            let mut second = f32::INFINITY;
            let mut minimum_edge = usize::MAX;
            for edge in edges {
                let value = belief[variables[*edge]] - message[*edge];
                sign ^= value.is_sign_negative();
                let magnitude = value.abs();
                if magnitude < minimum {
                    second = minimum;
                    minimum = magnitude;
                    minimum_edge = *edge;
                } else if magnitude < second {
                    second = magnitude;
                }
            }
            for edge in edges {
                let value = belief[variables[*edge]] - message[*edge];
                let magnitude = if *edge == minimum_edge {
                    second
                } else {
                    minimum
                };
                let negative = sign ^ value.is_sign_negative();
                let next = if negative {
                    -0.8 * magnitude
                } else {
                    0.8 * magnitude
                };
                belief[variables[*edge]] += next - message[*edge];
                message[*edge] = next;
            }
        }
        let hard: Vec<u8> = belief.iter().map(|value| u8::from(*value < 0.0)).collect();
        if rows.iter().all(|edges| {
            edges
                .iter()
                .fold(0, |parity, edge| parity ^ hard[variables[*edge]])
                == 0
        }) {
            return Some(hard[..information_blocks * lifting].to_vec());
        }
    }
    None
}

/// Recovers a transport block of `size` bits, whose 16-bit CRC is the only evidence that any of the
/// hypotheses that led here were right.
///
/// SIB1 always lands on base graph two, but that is a consequence rather than a rule and the margin is
/// under one percent: SI-RNTI forces QPSK, the qam64 table's QPSK rows top out at MCS 9 and 679/1024,
/// and SIB1 is capped at 2976 bits, so `size <= 3824 && rate <= 0.67` holds by 0.4% of rate. `segment`
/// therefore computes the selection of TS 38.212 section 7.2.2 and this refuses base graph one by name
/// rather than assuming it away — if it ever fires, one of those three premises has moved.
pub fn transport(
    input: &[f32],
    size: usize,
    rate: f64,
    modulation: usize,
    redundancy: u8,
) -> Result<Vec<u8>, Reason> {
    if size > 3824 {
        return Err(Reason::TransportSize);
    }
    let (graph, lifting) = segment(size, rate).ok_or(Reason::TransportSize)?;
    if graph == BaseGraph::One {
        return Err(Reason::BaseGraph { size, rate });
    }
    let block = size + 16;
    let recovered =
        recover(input, block, lifting, modulation, graph, redundancy).ok_or(Reason::Undecoded)?;
    let decoded = decode(&recovered, lifting, 100, graph).ok_or(Reason::Undecoded)?;
    let transport = decoded.get(..block).ok_or(Reason::Undecoded)?;
    Crc::Crc16
        .check(transport)
        .then(|| transport[..size].to_vec())
        .ok_or(Reason::Undecoded)
}

/// An encoder for base graph two, and the only independent statement in this crate of what the
/// transport chain is supposed to carry.
///
/// It exists so that a failure to recover SIB1 can be attributed. Without it a bad channel estimate and
/// a bad rate-matching index look identical from outside, and the temptation is to adjust the estimator
/// until something decodes. The lifting convention here is the one `decode` uses, which an independent
/// MATLAB codeword pins, so a round trip measures the rate matching rather than agreeing with itself
/// about how a base graph lifts.
#[cfg(test)]
pub(crate) mod oracle {
    use super::{BaseGraph, LIFTING};
    use crate::ldpc_tables::BG2;

    /// Every lifted parity check as the variable indices it sums, built exactly as `decode` builds it.
    fn checks(lifting: usize) -> Vec<Vec<usize>> {
        let set = LIFTING
            .iter()
            .position(|values| values.contains(&(lifting as u16)))
            .expect("lifting size is not in any normative set");
        let mut rows = vec![Vec::new(); 42 * lifting];
        for (row, column, shifts) in BG2 {
            let shift = usize::from(shifts[set]) % lifting;
            for index in 0..lifting {
                rows[usize::from(*row) * lifting + index]
                    .push(usize::from(*column) * lifting + (index + shift) % lifting);
            }
        }
        rows
    }

    /// The 52-column codeword for `information`, of which `recover` sees columns 2 onward.
    ///
    /// The core is solved rather than assumed dual-diagonal. Writing out the closed form would bake the
    /// structure of the graph into the oracle, and then the oracle could only ever confirm the reading
    /// of the tables that produced it.
    pub fn encode(information: &[u8], lifting: usize) -> Vec<u8> {
        let rows = checks(lifting);
        let mut codeword = vec![0u8; 52 * lifting];
        codeword[..information.len()].copy_from_slice(information);
        let core = 4 * lifting;
        let base = 10 * lifting;
        let words = core / 64 + 1;
        let mut matrix: Vec<Vec<u64>> = rows[..core]
            .iter()
            .map(|row| {
                let mut equation = vec![0u64; words];
                for variable in row {
                    if (base..base + core).contains(variable) {
                        let bit = variable - base;
                        equation[bit / 64] ^= 1 << (bit % 64);
                    } else if codeword[*variable] == 1 {
                        equation[core / 64] ^= 1 << (core % 64);
                    }
                }
                equation
            })
            .collect();
        let mut pivots = vec![usize::MAX; core];
        let mut rank = 0;
        for column in 0..core {
            let Some(found) = (rank..core)
                .find(|candidate| matrix[*candidate][column / 64] >> (column % 64) & 1 == 1)
            else {
                continue;
            };
            matrix.swap(rank, found);
            let pivot = matrix[rank].clone();
            for (other, equation) in matrix.iter_mut().enumerate() {
                if other != rank && equation[column / 64] >> (column % 64) & 1 == 1 {
                    for (word, value) in equation.iter_mut().zip(&pivot) {
                        *word ^= value;
                    }
                }
            }
            pivots[column] = rank;
            rank += 1;
        }
        for (column, row) in pivots.iter().enumerate() {
            assert_ne!(
                *row,
                usize::MAX,
                "the core is singular at lifting {lifting}"
            );
            codeword[base + column] = u8::from(matrix[*row][core / 64] >> (core % 64) & 1 == 1);
        }
        for (offset, row) in rows[core..].iter().enumerate() {
            let unknown = (14 + offset / lifting) * lifting + offset % lifting;
            codeword[unknown] = row
                .iter()
                .filter(|variable| **variable != unknown)
                .fold(0, |parity, variable| parity ^ codeword[*variable]);
        }
        for row in &rows {
            assert_eq!(
                row.iter()
                    .fold(0, |parity, variable| parity ^ codeword[*variable]),
                0,
                "the encoded word does not satisfy its own parity checks"
            );
        }
        codeword
    }

    /// The `length` transmitted bits of `codeword` from redundancy version `redundancy`, interleaved for
    /// `modulation`, which is the exact inverse of the selection `recover` undoes.
    pub fn rate_match(
        codeword: &[u8],
        transport_bits: usize,
        lifting: usize,
        modulation: usize,
        redundancy: u8,
        length: usize,
    ) -> Vec<f32> {
        let transmitted = 50 * lifting;
        let information = 10 * lifting;
        let mut null = vec![false; transmitted];
        null[transport_bits.saturating_sub(2 * lifting)..information - 2 * lifting].fill(true);
        let mut selected = Vec::with_capacity(length);
        let mut index = BaseGraph::Two.starts()[usize::from(redundancy)] * lifting;
        while selected.len() < length {
            let position = index % transmitted;
            if !null[position] {
                selected.push(position);
            }
            index += 1;
        }
        let chunk = length / modulation;
        (0..length)
            .map(|bit| {
                let position = selected[(bit % modulation) * chunk + bit / modulation];
                if codeword[position + 2 * lifting] == 0 {
                    20.0
                } else {
                    -20.0
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseGraph, decode, oracle, recover, segment, transport};
    use crate::ldpc_tables::{BG1, BG2, LIFTING};
    use crate::{Crc, Reason};

    #[test]
    fn normative_graphs_and_lifting_sets_have_expected_shapes() {
        assert_eq!((BG1.len(), BG2.len()), (316, 197));
        assert_eq!(LIFTING.iter().map(|set| set.len()).sum::<usize>(), 51);
        assert!(LIFTING[6].contains(&104));
        assert!(
            !LIFTING
                .iter()
                .flat_map(|set| set.iter())
                .any(|value| *value == 105)
        );
    }

    #[test]
    fn recovery_restores_punctures_and_fillers() {
        let input = vec![1.0; 1_728];
        let recovered = recover(&input, 656, 72, 2, BaseGraph::Two, 0).unwrap();
        assert_eq!(recovered.len(), 3_744);
        assert!(recovered[..144].iter().all(|value| *value == 0.0));
        assert!(recovered[656..720].iter().all(|value| *value > 1e19));
    }

    /// The reference capture broadcasts a 640-bit block at rate 379/1024, which segments onto base
    /// graph two with a lifting size of 72; both were hand-checked before the fixture was trusted.
    #[test]
    fn the_reference_block_segments_onto_the_hand_checked_graph() {
        assert_eq!(segment(640, 379.0 / 1024.0), Some((BaseGraph::Two, 72)));
        assert_eq!(segment(4_000, 0.8), Some((BaseGraph::One, 192)));
        assert_eq!(segment(9_000, 0.9), None);
    }

    /// The four information-block widths TS 38.212 section 5.2.2 selects a lifting size with. Every
    /// broadcast this decoder met on air was wide enough for the last of them, so the narrow three were
    /// never exercised until a synthetic allocation reached them.
    #[test]
    fn each_information_width_round_trips() {
        for (size, rate, expected) in [
            (176, 193.0 / 1024.0, (BaseGraph::Two, 32)),
            (320, 193.0 / 1024.0, (BaseGraph::Two, 44)),
            (600, 379.0 / 1024.0, (BaseGraph::Two, 72)),
            (768, 120.0 / 1024.0, (BaseGraph::Two, 80)),
        ] {
            let (graph, lifting) = segment(size, rate).unwrap();
            assert_eq!((graph, lifting), expected);
            let transport_block = block(size, 377);
            let codeword = oracle::encode(&transport_block, lifting);
            let llr = oracle::rate_match(&codeword, size + 16, lifting, 2, 0, 40 * lifting);
            assert_eq!(
                transport(&llr, size, rate, 2, 0).as_deref(),
                Ok(&transport_block[..size]),
                "size {size} on {graph:?} at lifting {lifting}"
            );
        }
    }

    /// A deterministic transport block of `size` bits with its 16-bit CRC attached, which is what the
    /// DL-SCH carries and what a round trip has to return unchanged.
    fn block(size: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let payload: Vec<u8> = (0..size)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                u8::from(state >> 63 == 1)
            })
            .collect();
        Crc::Crc16.append(&payload)
    }

    /// The whole transport chain, end to end, against an encoder that shares none of its code.
    ///
    /// Rate matching, the redundancy-version starts, the bit interleaver, the filler positions, the
    /// scrambling-free path into the decoder and the CRC all have to agree at once for this to pass, and
    /// each is an index computation with an off-by-one that a live capture reports as a weak signal.
    #[test]
    fn the_transport_chain_round_trips_every_size_rate_and_lifting_it_will_meet() {
        for size in [40, 208, 640, 1_192, 2_976] {
            for rate in [251.0, 379.0, 490.0, 616.0, 679.0] {
                let rate = rate / 1024.0;
                let (graph, lifting) = segment(size, rate).unwrap();
                assert_eq!(graph, BaseGraph::Two);
                let transport_block = block(size, size as u64 * 1_000 + rate as u64);
                let codeword = oracle::encode(&transport_block, lifting);
                for modulation in [2, 4] {
                    let wanted = ((size + 16) as f64 / rate).ceil() as usize;
                    let length = wanted.div_ceil(modulation) * modulation;
                    let llr =
                        oracle::rate_match(&codeword, size + 16, lifting, modulation, 0, length);
                    assert_eq!(
                        transport(&llr, size, rate, modulation, 0).as_deref(),
                        Ok(&transport_block[..size]),
                        "size {size} rate {rate} modulation {modulation}"
                    );
                }
            }
        }
    }

    /// The Rogers window that reached a CRC-valid RV 0 DCI and no transport block, at exactly the
    /// parameters it reported: 34 resource blocks over 11 symbols with three of them reference, MCS 0,
    /// which sizes a 768-bit block and then asks for 6528 soft bits from a 4000-bit codeword.
    ///
    /// MCS 0 is the rate SIB1 is most often sent at and the least covered by a grid that starts at
    /// 251/1024, and asking for more bits than the codeword holds is the only case that wraps the
    /// circular buffer more than once. Both had to be ruled out before the estimate could be blamed.
    /// The lifting is 80 rather than the 104 that reading `K_b` off the rate suggests: `K_b` is 10 for
    /// every block over 640 bits regardless of rate (TS 38.212 section 5.2.2), and only the base graph
    /// depends on the rate.
    #[test]
    fn the_lowest_broadcast_rate_round_trips_when_it_asks_for_more_bits_than_the_codeword_holds() {
        let (size, rate) = (768, 120.0 / 1024.0);
        let (graph, lifting) = segment(size, rate).unwrap();
        assert_eq!((graph, lifting), (BaseGraph::Two, 80));
        let transport_block = block(size, 377);
        let codeword = oracle::encode(&transport_block, lifting);
        let llr = oracle::rate_match(&codeword, size + 16, lifting, 2, 0, 6_528);
        // The circular buffer is 50 lifted columns, the two punctured systematic ones having been
        // dropped, so this asks for a sixth more bits than the buffer holds.
        assert!(llr.len() > 50 * lifting);
        assert_eq!(
            transport(&llr, size, rate, 2, 0).as_deref(),
            Ok(&transport_block[..size])
        );
    }

    /// A whole circular buffer decodes from every redundancy version, which is what pins the starts of
    /// TS 38.212 Table 5.4.2.1-2 and the wrap around the end of the buffer.
    #[test]
    fn every_redundancy_version_recovers_the_block_it_started_from() {
        let (size, rate) = (640, 379.0 / 1024.0);
        let (_, lifting) = segment(size, rate).unwrap();
        let transport_block = block(size, 7);
        let codeword = oracle::encode(&transport_block, lifting);
        for redundancy in 0..4 {
            let llr =
                oracle::rate_match(&codeword, size + 16, lifting, 2, redundancy, 50 * lifting);
            assert_eq!(
                transport(&llr, size, rate, 2, redundancy).as_deref(),
                Ok(&transport_block[..size]),
                "redundancy version {redundancy}"
            );
        }
    }

    /// DANGER: redundancy versions 1 and 2 start past every systematic bit, so a single occasion
    /// carrying one of them cannot be decoded however many times it is retried. A live n5 cell offered
    /// three occasions in a half-second window carrying RV 2, RV 1 and RV 1, and raising the attempt
    /// budget from 4 to 32 changed nothing — the fix was to walk forward to an occasion carrying RV 0.
    #[test]
    fn a_lone_puncturing_redundancy_version_cannot_be_decoded_by_retrying_it() {
        let (size, rate) = (640, 379.0 / 1024.0);
        let (_, lifting) = segment(size, rate).unwrap();
        let codeword = oracle::encode(&block(size, 11), lifting);
        let length = 25 * lifting;
        for redundancy in [1, 2] {
            let llr = oracle::rate_match(&codeword, size + 16, lifting, 2, redundancy, length);
            assert!(transport(&llr, size, rate, 2, redundancy).is_err());
        }
        let llr = oracle::rate_match(&codeword, size + 16, lifting, 2, 0, length);
        assert!(transport(&llr, size, rate, 2, 0).is_ok());
    }

    /// Base graph one is refused by name rather than attempted. SIB1 cannot reach it, and a decode that
    /// silently tried would report a weak signal instead of the premise that moved.
    #[test]
    fn base_graph_one_is_refused_by_name() {
        assert_eq!(
            transport(&[0.0; 64], 3_800, 0.9, 2, 0),
            Err(Reason::BaseGraph {
                size: 3_800,
                rate: 0.9
            })
        );
    }

    #[test]
    fn decoder_matches_independent_bg2_z72_vector() {
        let input = include_bytes!("../tests/fixtures/ldpc/bg2-z72-input.bin");
        let codeword = include_bytes!("../tests/fixtures/ldpc/bg2-z72-codeword.bin");
        let mut llr = vec![0.0; 144];
        llr.extend(
            codeword
                .iter()
                .map(|bit| if *bit == 0 { 20.0 } else { -20.0 }),
        );
        assert_eq!(
            decode(&llr, 72, 100, BaseGraph::Two).unwrap(),
            input.as_slice()
        );
    }
}
