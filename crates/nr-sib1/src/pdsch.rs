use num_complex::Complex32;

use crate::{Dci, Mib, Numerology, Reason, decode_transport, gold};

/// Where the initial bandwidth part sits in this capture.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    /// Subcarrier of its first resource block, which is both the origin of a resource assignment and
    /// the reference point of every demodulation sequence (TS 38.211 section 7.4.1.1.2).
    pub start: usize,
    /// Its width, which for a SIB1 is CORESET#0 and is the span the VRB-to-PRB interleaver permutes
    /// across — so it changes the mapping, not just the bounds.
    pub resource_blocks: usize,
}

/// Which end of the allocation the reference symbols are counted from. Mapping type A counts from the
/// start of the slot and type B from the start of the allocation, so reading one on the other's timing
/// measures the channel at symbols that carry data and reports a CRC failure for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mapping {
    A,
    B,
}

/// Time-domain resource allocation of TS 38.214 Table 5.1.2.1.1-2, the default table that a DCI in
/// the Type0-PDCCH common search space indexes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Allocation {
    pub mapping: Mapping,
    pub slots: u8,
    pub first_symbol: u8,
    pub symbols: u8,
}

/// Modulation order and code rate, in 1024ths, of TS 38.214 Table 5.1.3.1-1, which DCI format `1_0`
/// always indexes.
const MODULATION: [(u8, u16); 29] = [
    (2, 120),
    (2, 157),
    (2, 193),
    (2, 251),
    (2, 308),
    (2, 379),
    (2, 449),
    (2, 526),
    (2, 602),
    (2, 679),
    (4, 340),
    (4, 378),
    (4, 434),
    (4, 490),
    (4, 553),
    (4, 616),
    (4, 658),
    (6, 438),
    (6, 466),
    (6, 517),
    (6, 567),
    (6, 616),
    (6, 666),
    (6, 719),
    (6, 772),
    (6, 822),
    (6, 873),
    (6, 910),
    (6, 948),
];

/// Modulation order and code rate of `mcs`, absent for the reserved indices.
#[must_use]
pub fn modulation(mcs: u8) -> Option<(u8, u16)> {
    MODULATION.get(usize::from(mcs)).copied()
}

pub fn allocation(dci: &Dci, dmrs_type_a_position: u8) -> Result<Allocation, Reason> {
    /// Rows of TS 38.214 Table 5.1.2.1.1-2 as (mapping type B, K0, S, L), for `dmrs-TypeA-Position`
    /// pos2 then pos3.
    const TABLE: [[(bool, u8, u8, u8); 2]; 16] = [
        [(false, 0, 2, 12), (false, 0, 3, 11)],
        [(false, 0, 2, 10), (false, 0, 3, 9)],
        [(false, 0, 2, 9), (false, 0, 3, 8)],
        [(false, 0, 2, 7), (false, 0, 3, 6)],
        [(false, 0, 2, 5), (false, 0, 3, 4)],
        [(true, 0, 9, 4), (true, 0, 10, 4)],
        [(true, 0, 4, 4), (true, 0, 6, 4)],
        [(true, 0, 5, 7), (true, 0, 5, 7)],
        [(true, 0, 5, 2), (true, 0, 5, 2)],
        [(true, 0, 9, 2), (true, 0, 9, 2)],
        [(true, 0, 12, 2), (true, 0, 12, 2)],
        [(false, 0, 1, 13), (false, 0, 1, 13)],
        [(false, 0, 1, 6), (false, 0, 1, 6)],
        [(false, 0, 2, 4), (false, 0, 2, 4)],
        [(true, 0, 4, 7), (true, 0, 4, 7)],
        [(true, 0, 8, 4), (true, 0, 8, 4)],
    ];
    let unmapped = || Reason::Unmapped {
        row: dci.time,
        dmrs_type_a_position,
    };
    let position = usize::from(dmrs_type_a_position.checked_sub(2).ok_or_else(unmapped)?);
    let &(mapping_b, slots, first_symbol, symbols) = TABLE
        .get(usize::from(dci.time))
        .and_then(|row| row.get(position))
        .ok_or_else(unmapped)?;
    Ok(Allocation {
        mapping: if mapping_b { Mapping::B } else { Mapping::A },
        slots,
        first_symbol,
        symbols,
    })
}

/// Bundle size of the virtual-to-physical mapping for a DCI format `1_0` in the Type0-PDCCH common
/// search space (TS 38.211 section 7.3.1.6).
const BUNDLE: usize = 2;

/// Physical resource block each virtual one of a `width`-block bandwidth part maps to.
///
/// Interleaving permutes whole bundles across the bandwidth part, so a contiguous assignment lands
/// scattered, and reading it contiguously gathers the right number of samples from the wrong
/// subcarriers — which a receiver reports as a CRC failure and reads as a weak signal. The bandwidth
/// part starts at the lowest CORESET#0 resource block, so its offset within the bundle grid is zero and
/// only the last bundle can be short.
fn interleave(width: usize) -> Vec<usize> {
    let bundles = width.div_ceil(BUNDLE);
    let columns = bundles / 2;
    let mut blocks = Vec::with_capacity(width);
    for bundle in 0..bundles {
        let mapped = if bundle + 1 == bundles {
            bundle
        } else {
            (bundle % 2) * columns + bundle / 2
        };
        let size = (width - bundle * BUNDLE).min(BUNDLE);
        blocks.extend((0..size).map(|offset| mapped * BUNDLE + offset));
    }
    blocks
}

/// What the demodulation reference symbols of an allocation measured, each keyed by the symbol's index
/// within it. The phases are kept apart from the channel because `rotation` has to solve for the slot's
/// carrier phase before the channel can be corrected for it.
struct Estimates {
    channel: Vec<(usize, Vec<Complex32>)>,
    phase: Vec<(usize, f32)>,
}

/// Channel estimate and measured reference phase of each symbol of `region` that carries a
/// demodulation sequence.
///
/// `assigned` is the physical resource block behind each block of the region, which is the identity
/// unless the DCI interleaved the mapping. It selects the sequence elements as well as the subcarriers:
/// the sequence is laid over the bandwidth part's physical blocks, so a reference in a permuted block
/// must be despread against the element that block holds and not against the one its virtual position
/// would suggest.
fn estimate(
    region: &[Vec<Complex32>],
    assigned: &[usize],
    allocation: Allocation,
    mib: &Mib,
    slot: u16,
    bwp: usize,
) -> Result<Estimates, Reason> {
    let width = 12 * assigned.len();
    let mut channel: Vec<(usize, Vec<Complex32>)> = Vec::new();
    let mut phase: Vec<(usize, f32)> = Vec::new();
    for symbol in references(allocation, mib.dmrs_type_a_position) {
        let index = usize::from(
            symbol
                .checked_sub(allocation.first_symbol)
                .ok_or(Reason::Undecoded)?,
        );
        let samples = region.get(index).ok_or(Reason::Undecoded)?;
        let sequence = sequence(mib.pci, slot, symbol, 6 * bwp);
        // Configuration type 1 places the reference on every other subcarrier of the resource block,
        // taking sequence element k/2 for subcarrier k counted from the reference point.
        let pilots: Vec<Complex32> = assigned
            .iter()
            .enumerate()
            .flat_map(|(block, physical)| (0..6).map(move |pilot| (block, *physical, pilot)))
            .map(|(block, physical, pilot)| {
                samples[12 * block + 2 * pilot] * sequence[6 * physical + pilot].conj()
            })
            .collect();
        let mean: Complex32 = pilots.iter().sum();
        if mean.norm_sqr() == 0.0 {
            return Err(Reason::Undecoded);
        }
        phase.push((index, mean.arg()));
        channel.push((
            index,
            (0..width)
                .map(|subcarrier| {
                    let lower = pilots[subcarrier / 2];
                    let upper = pilots[(subcarrier / 2 + 1).min(pilots.len() - 1)];
                    if subcarrier % 2 == 0 {
                        lower
                    } else {
                        (lower + upper) / 2.0
                    }
                })
                .collect(),
        ));
    }
    Ok(Estimates { channel, phase })
}

/// Soft bits of one PDSCH occasion, with what the coding chain needs to read them.
///
/// Handed out before decoding so that a caller walking a cell's repeats can combine occasions carrying
/// different redundancy versions of one block. `ldpc::Combine` is what consumes it.
pub struct Soft {
    pub llr: Vec<f32>,
    pub size: usize,
    pub rate: f64,
    pub order: usize,
    pub redundancy: u8,
}

/// Recovers the SIB1 transport block from the PDSCH symbols the DCI scheduled.
pub fn decode_sib1(
    symbols: &[Vec<Complex32>],
    mib: &Mib,
    dci: &Dci,
    numerology: Numerology,
    center_hz: f64,
    part: Region,
    slot: u16,
) -> Result<Vec<u8>, Reason> {
    let candidates = soft(symbols, mib, dci, numerology, center_hz, part, slot)?;
    let mut furthest = Reason::Undecoded;
    for soft in candidates {
        match decode_transport(&soft.llr, soft.size, soft.rate, soft.order, soft.redundancy) {
            Ok(bits) => return Ok(crate::ldpc::bytes(&bits)),
            Err(reason) => furthest = reason,
        }
    }
    Err(furthest)
}

/// Demodulates and equalizes one PDSCH occasion, stopping short of the coding chain.
///
/// Every way this can fail carries its own reason. Collapsing them onto one reported a missing mapping
/// as a failed CRC, which sends whoever reads it looking for a weak signal.
#[allow(clippy::too_many_lines)]
pub fn soft(
    symbols: &[Vec<Complex32>],
    mib: &Mib,
    dci: &Dci,
    numerology: Numerology,
    center_hz: f64,
    part: Region,
    slot: u16,
) -> Result<Vec<Soft>, Reason> {
    let allocation = allocation(dci, mib.dmrs_type_a_position)?;
    let (order, rate) = modulation(dci.mcs).ok_or(Reason::Unmodulated { mcs: dci.mcs })?;
    // A UE is not expected to decode an SI-RNTI PDSCH above QPSK (TS 38.214 section 5.1.3.1), so this
    // is evidence the DCI was misparsed rather than a demapper worth writing. `pdcch` invalidates such
    // a candidate before it reaches here; this covers a caller driving the chain directly.
    if order != 2 {
        return Err(Reason::UnexpectedSib1Modulation { qm: order });
    }
    if symbols.len() != usize::from(allocation.symbols) {
        return Err(Reason::Symbols {
            wanted: usize::from(allocation.symbols),
            got: symbols.len(),
        });
    }
    let blocks = usize::from(dci.resource_blocks);
    let offset = usize::from(dci.first_resource_block);
    let width = 12 * blocks;
    let mapped = if dci.interleaved {
        interleave(part.resource_blocks)
    } else {
        (0..part.resource_blocks).collect()
    };
    let assigned = mapped
        .get(offset..offset + blocks)
        .ok_or(Reason::Assigned {
            first_resource_block: offset,
            resource_blocks: blocks,
            bwp: part.resource_blocks,
        })?;
    let region: Vec<Vec<Complex32>> = symbols
        .iter()
        .map(|symbol| {
            assigned
                .iter()
                .map(|block| symbol.get(part.start + 12 * block..part.start + 12 * (block + 1)))
                .collect::<Option<Vec<_>>>()
                .map(|blocks| blocks.concat())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Reason::Undecoded)?;
    let Estimates { channel, phase } = estimate(
        &region,
        assigned,
        allocation,
        mib,
        slot,
        part.resource_blocks,
    )?;
    let size = transport_size(blocks, allocation.symbols, channel.len(), order, rate)
        .ok_or(Reason::TransportSize)?;
    let initial = (u32::from(u16::MAX) << 15) + u32::from(mib.pci);
    // DANGER: combine rather than divide, for the reasons given on `equalize` in `pdcch`. Turning an
    // estimate leaves its magnitude alone, so this is the same for every carrier-phase candidate below.
    let power = channel
        .iter()
        .flat_map(|(_, estimates)| estimates)
        .map(Complex32::norm_sqr)
        .sum::<f32>()
        / (channel.len() * width) as f32;
    if power == 0.0 {
        return Err(Reason::Undecoded);
    }
    let candidates = rotation(
        numerology,
        slot,
        allocation,
        region.len(),
        &phase,
        center_hz,
    );
    if candidates.is_empty() {
        return Err(Reason::Rotation);
    }
    candidates
        .into_iter()
        .map(|rotation| {
            let mut channel = channel.clone();
            // Align every reference to a common phase so that interpolating across them measures the
            // channel rather than where in the slot each one happened to sit.
            for (index, estimates) in &mut channel {
                let level = Complex32::from_polar(1.0, -rotation[*index]);
                for estimate in estimates {
                    *estimate *= level;
                }
            }
            let mut llr = Vec::with_capacity(2 * width * (region.len() - channel.len()));
            for (index, samples) in region.iter().enumerate() {
                if channel.iter().any(|(reference, _)| *reference == index) {
                    continue;
                }
                let estimate = interpolate(&channel, index).ok_or(Reason::Undecoded)?;
                let level = Complex32::from_polar(1.0, rotation[index]);
                for (sample, estimate) in samples.iter().zip(estimate) {
                    let value = sample * (estimate * level).conj() / power;
                    llr.push(value.re);
                    llr.push(value.im);
                }
            }
            let scrambling = gold(initial, llr.len());
            for (value, bit) in llr.iter_mut().zip(scrambling) {
                if bit == 1 {
                    *value = -*value;
                }
            }
            Ok(Soft {
                llr,
                size,
                rate: f64::from(rate) / 1024.0,
                order: usize::from(order),
                redundancy: dci.redundancy,
            })
        })
        .collect()
}

/// Distinct carrier-phase readings carried forward when the references cannot separate them.
///
/// DANGER: this is a bound on work and has to stay above the set rather than inside it. Two readings
/// are only distinguishable if they differ by more than the dedup's threshold somewhere in the
/// allocation, so the set cannot be larger than the search range times the allocation's duration —
/// about thirty over ±7.5 kHz and a 15 kHz slot, and fewer at every other numerology. A cap of three
/// sat well inside that. A live Rogers cell offered twenty readings and had transmitted the eleventh,
/// so the truncated set never held it: a broadcast whose references were 0.87 coherent, whose DCI was
/// CRC-valid and whose transport size was right reported as a DL-SCH that would not decode through
/// thirteen accumulated occasions. It reads on its first occasion once the set is carried whole, and
/// `rogers-n105-dlsch` is what pins the number: that capture fails at three and reads from eight.
const ALIASES: usize = 32;

/// Every constant rotation the cell may have left on the symbols of the allocation, in radians.
///
/// TS 38.211 section 5.4 has the cell restart the carrier phase at the beginning of every symbol, so a
/// receiver sees each symbol turned by its own start time against the frequency the cell compensated at.
/// Without undoing that, the sparse references of a PDSCH cannot be told apart from the channel and every
/// symbol between them decodes as noise.
///
/// There is more than one answer. The compensating frequency is either the tuner's or nothing at all, and
/// within each the residual offset the acquisition did not remove aliases across an allocation this
/// short. So this returns candidates in an order that does not change between occasions, and the DL-SCH
/// CRC settles them like every other ambiguity that arrives before the decode.
fn rotation(
    numerology: Numerology,
    slot: u16,
    allocation: Allocation,
    symbols: usize,
    measured: &[(usize, f32)],
    center_hz: f64,
) -> Vec<Vec<f32>> {
    if measured.is_empty() {
        return Vec::new();
    }
    let mut clock = 0;
    let times: Vec<f64> = (0..symbols)
        .map(|index| {
            let position = 14 * usize::from(slot) + usize::from(allocation.first_symbol) + index;
            clock += numerology.prefix(position);
            let body = clock;
            clock += numerology.size;
            body as f64
        })
        .collect();
    let rate = numerology.sample_rate_hz as f64;
    let shape = |slope: f64| -> Vec<f32> {
        times
            .iter()
            .map(|time| (slope * time).rem_euclid(std::f64::consts::TAU) as f32)
            .collect()
    };
    // One reference fits every slope exactly, so nothing in the allocation can choose between them —
    // and rows 5, 6 and 7 of the default A table carry exactly one, which TS 38.214 Table 5.1.2.1.1-1
    // lets a cell schedule SIB1 on. The slope is not unknown though, only unmeasurable here: TS 38.211
    // section 5.4 has the transmitter compensate at its own carrier, so the two ends of that are the
    // only candidates worth carrying, and the DL-SCH CRC settles which one this chain presents.
    if measured.len() < 2 {
        return [center_hz, 0.0]
            .into_iter()
            .map(|tuned| shape(-std::f64::consts::TAU * tuned / rate))
            .collect();
    }
    // DANGER: walk the residual outward from zero rather than from one end. Three references spread
    // over less than a millisecond alias badly — a residual of a few kilohertz turns the fitted phase
    // through many whole turns, so dozens of slopes explain them exactly and the search cannot tell
    // those apart. Sweeping from -7.5 kHz upward therefore settled on the largest residual that fit,
    // which is the one furthest from the offset the acquisition had already corrected, and it chose a
    // different one on each occasion of the same cell. Data symbols came out rotated by up to half a
    // turn, so the soft bits of consecutive occasions of one broadcast disagreed on 60% of their signs
    // and combining them cancelled rather than accumulated.
    let residuals = (0..=1500).flat_map(|step| {
        if step == 0 {
            vec![0]
        } else {
            vec![step, -step]
        }
    });
    let score = |slope: f64| -> f32 {
        measured
            .iter()
            .map(|(index, phase)| {
                Complex32::from_polar(
                    1.0,
                    phase - (slope * times[*index]).rem_euclid(std::f64::consts::TAU) as f32,
                )
            })
            .sum::<Complex32>()
            .norm()
    };
    let mut fitted: Vec<(f32, f64, f64)> = Vec::new();
    for tuned in [center_hz, 0.0] {
        for step in residuals.clone() {
            let residual = f64::from(step) * 5.0;
            let slope = -std::f64::consts::TAU * (tuned - residual) / rate;
            fitted.push((score(slope), residual.abs(), slope));
        }
    }
    // A model that does not explain the references it was fitted to cannot be trusted to predict the
    // symbols between them.
    let ceiling = fitted
        .iter()
        .fold(0.0_f32, |best, (score, ..)| best.max(*score));
    if ceiling <= 0.9 * measured.len() as f32 {
        return Vec::new();
    }
    // DANGER: three references over less than a millisecond alias, and the aliases fit exactly. On a
    // live Rogers cell two of them scored 3.000 and 2.999 out of three references, about ten kilohertz
    // apart, and which one won was decided by noise — a different one on each occasion of the same
    // broadcast. Their predicted phases differ by around half a turn at the data symbols, so the soft
    // bits of consecutive occasions disagreed on sixty per cent of their signs and accumulating them
    // cancelled instead of combining.
    //
    // Taking the argmax is therefore choosing at random between readings that the references cannot
    // separate, and ordering the rest behind it inherits that: which alias won decided which of the
    // others survived the dedup, so a buffer's index named a different reading on each occasion and
    // accumulating them cancelled exactly as taking the argmax alone had.
    //
    // DANGER: order these by how far the residual sits from the offset the acquisition already
    // resolved, never by score. The ordering is the whole mechanism — a buffer accumulates the
    // candidate at its own index across every occasion of the broadcast, so that index has to name the
    // same reading each time whatever the noise did to the scores. On the live n105 cell the winner
    // moved between +6.9 kHz, -7.1 kHz and -55 Hz across thirteen occasions of one RV 0 broadcast,
    // which read as an undecodable cell and was neither weak nor impaired.
    let apart = |left: &f32, right: &f32| {
        let gap = (left - right).abs();
        gap.min(std::f32::consts::TAU - gap) > 0.3
    };
    let mut near: Vec<&(f32, f64, f64)> = fitted
        .iter()
        .filter(|(score, ..)| *score > ceiling - 0.02 * measured.len() as f32)
        .collect();
    near.sort_by(|(_, left, _), (_, right, _)| left.total_cmp(right));
    let mut candidates: Vec<Vec<f32>> = Vec::new();
    for (.., slope) in near {
        let shaped = shape(*slope);
        if candidates.iter().all(|kept| {
            kept.iter()
                .zip(&shaped)
                .any(|(kept, value)| apart(kept, value))
        }) {
            candidates.push(shaped);
        }
        if candidates.len() == ALIASES {
            break;
        }
    }
    candidates
}

/// Reference symbols of the allocation as slot indices, for single-symbol reference signals with
/// `dmrs-AdditionalPosition` at `pos2`, which is what a UE assumes for the PDSCH that carries SIB1.
///
/// Type A is TS 38.211 Table 7.4.1.1.2-3 and measures its duration from the start of the slot; type B is
/// Table 7.4.1.1.2-4, measures its duration from the start of the allocation, and always puts its first
/// reference on the allocation's own first symbol rather than at `dmrs-TypeA-Position`.
fn references(allocation: Allocation, dmrs_type_a_position: u8) -> Vec<u8> {
    match allocation.mapping {
        Mapping::A => match allocation.first_symbol + allocation.symbols {
            ..8 => vec![dmrs_type_a_position],
            8..10 => vec![dmrs_type_a_position, 7],
            10..13 => vec![dmrs_type_a_position, 6, 9],
            _ => vec![dmrs_type_a_position, 7, 11],
        },
        Mapping::B => match allocation.symbols {
            ..7 => vec![allocation.first_symbol],
            _ => vec![allocation.first_symbol, allocation.first_symbol + 4],
        },
    }
}

fn sequence(pci: u16, slot: u16, symbol: u8, length: usize) -> Vec<Complex32> {
    let initial =
        ((1 << 17) * (14 * u64::from(slot) + u64::from(symbol) + 1) * (2 * u64::from(pci) + 1)
            + 2 * u64::from(pci))
            % (1 << 31);
    let bits = gold(initial as u32, 2 * length);
    (0..length)
        .map(|index| {
            Complex32::new(
                std::f32::consts::FRAC_1_SQRT_2 * (1.0 - 2.0 * f32::from(bits[2 * index])),
                std::f32::consts::FRAC_1_SQRT_2 * (1.0 - 2.0 * f32::from(bits[2 * index + 1])),
            )
        })
        .collect()
}

/// Channel of a symbol that carries no reference, interpolated between the symbols that do. This is
/// only sound because the carrier offset is removed against a single anchor, so a symbol's phase is
/// the channel rather than an artefact of where its own demodulation window started.
fn interpolate(channel: &[(usize, Vec<Complex32>)], symbol: usize) -> Option<Vec<Complex32>> {
    let below = channel.iter().rfind(|(index, _)| *index < symbol);
    let above = channel.iter().find(|(index, _)| *index > symbol);
    match (below, above) {
        (Some((lower, before)), Some((upper, after))) => {
            let weight = (symbol - lower) as f32 / (upper - lower) as f32;
            Some(
                before
                    .iter()
                    .zip(after)
                    .map(|(before, after)| before * (1.0 - weight) + after * weight)
                    .collect(),
            )
        }
        (Some((_, nearest)), None) | (None, Some((_, nearest))) => Some(nearest.clone()),
        (None, None) => None,
    }
}

/// Transport block size of TS 38.214 section 5.1.3.2, for one layer and no configured overhead.
fn transport_size(
    blocks: usize,
    symbols: u8,
    references: usize,
    order: u8,
    rate: u16,
) -> Option<usize> {
    /// TS 38.214 Table 5.1.3.2-1.
    const SIZES: [u16; 93] = [
        24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 128, 136, 144, 152, 160, 168, 176,
        184, 192, 208, 224, 240, 256, 272, 288, 304, 320, 336, 352, 368, 384, 408, 432, 456, 480,
        504, 528, 552, 576, 608, 640, 672, 704, 736, 768, 808, 848, 888, 928, 984, 1032, 1064,
        1128, 1160, 1192, 1224, 1256, 1288, 1320, 1352, 1416, 1480, 1544, 1608, 1672, 1736, 1800,
        1864, 1928, 2024, 2088, 2152, 2216, 2280, 2408, 2472, 2536, 2600, 2664, 2728, 2792, 2856,
        2976, 3104, 3240, 3368, 3496, 3624, 3752, 3824,
    ];
    // A reference symbol carries no data at all: DCI format 1_0 leaves both demodulation groups
    // without data for every allocation longer than two symbols.
    let per_block = 12 * usize::from(symbols).checked_sub(references)?;
    let elements = per_block.min(156) * blocks;
    let information = elements as f64 * f64::from(rate) / 1024.0 * f64::from(order);
    // Above this a transport block segments into several code blocks, which broadcast never does.
    if information < 24.0 || information > 3824.0 {
        return None;
    }
    let shift = (information.log2().floor() as u32).saturating_sub(6).max(3);
    let quantized = (((information as usize) >> shift) << shift).max(24);
    SIZES
        .iter()
        .find(|size| usize::from(**size) >= quantized)
        .map(|size| usize::from(*size))
}

/// A SIB1 PDSCH transmitter, for covering configurations no local cell happens to broadcast.
///
/// Retained real captures are the only honest evidence about acquisition and impairment, and they say
/// nothing at all about a legal configuration nobody near Hamilton transmits — every cell within reach
/// broadcasts direct mapping at MCS 0 on mapping type A. Interleaved mapping, type B, and every MCS and
/// redundancy version above the first are exercised here or not at all.
///
/// It shares the reference sequence, the scrambling sequence and the resource-block permutation with the
/// decoder, so it cannot catch an error in those. What it does catch is the whole chain around them:
/// segmentation, rate matching, redundancy selection, modulation, the placement of data around the
/// reference symbols, and the arithmetic that sizes the transport block.
#[cfg(test)]
mod vector {
    use super::gold;
    use super::{
        Estimates, allocation, estimate, interleave, modulation, references, transport_size,
    };
    use crate::ldpc::{oracle, segment};
    use crate::{Crc, Dci, Mib, Region};
    use num_complex::Complex32;

    /// The resource grid of one slot carrying `payload` bits of SIB1, and the transport block a decoder
    /// has to return unchanged.
    pub fn broadcast(
        mib: &Mib,
        dci: &Dci,
        part: Region,
        slot: u16,
        subcarriers: usize,
    ) -> (Vec<Vec<Complex32>>, Vec<u8>) {
        let allocation = allocation(dci, mib.dmrs_type_a_position).unwrap();
        let (order, rate) = modulation(dci.mcs).unwrap();
        let blocks = usize::from(dci.resource_blocks);
        let offset = usize::from(dci.first_resource_block);
        let width = 12 * blocks;
        let mapped: Vec<usize> = if dci.interleaved {
            interleave(part.resource_blocks)
        } else {
            (0..part.resource_blocks).collect()
        };
        let assigned = &mapped[offset..offset + blocks];
        let carrying: Vec<u8> = references(allocation, mib.dmrs_type_a_position)
            .into_iter()
            .map(|symbol| symbol - allocation.first_symbol)
            .collect();
        let size = transport_size(blocks, allocation.symbols, carrying.len(), order, rate).unwrap();

        let mut state = u64::from(mib.pci) << 32 | u64::from(size as u32) | 1;
        let payload: Vec<u8> = (0..size)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                u8::from(state >> 63 == 1)
            })
            .collect();
        let transport = Crc::Crc16.append(&payload);
        let (_, lifting) = segment(size, f64::from(rate) / 1024.0).unwrap();
        let coded = 2 * width * (usize::from(allocation.symbols) - carrying.len());
        let codeword = oracle::encode(&transport, lifting);
        let selected = oracle::rate_match(
            &codeword,
            size + 16,
            lifting,
            usize::from(order),
            dci.redundancy,
            coded,
        );
        // `rate_match` speaks in the log-likelihoods a decoder consumes; a transmitter needs the bits it
        // decided, and a positive likelihood is a zero.
        let scrambling = gold((u32::from(u16::MAX) << 15) + u32::from(mib.pci), coded);
        let bits: Vec<u8> = selected
            .iter()
            .zip(scrambling)
            .map(|(value, mask)| u8::from(*value < 0.0) ^ mask)
            .collect();

        let mut grid =
            vec![vec![Complex32::default(); subcarriers]; usize::from(allocation.symbols)];
        let mut pairs = bits.as_chunks::<2>().0.iter();
        let amplitude = 1.0 / 2.0_f32.sqrt();
        for (index, symbol) in grid.iter_mut().enumerate() {
            let carried = u8::try_from(index).unwrap();
            // The region is laid out in the allocation's own block order and then scattered to the
            // physical blocks behind it, which is the one order the sequence and the data agree in.
            // DANGER: a permuted block's reference element is chosen by where the block sits, not by
            // where the allocation put it, so the two indices cannot be collapsed.
            let region: Vec<Complex32> = if carrying.contains(&carried) {
                let sequence = &super::sequence(
                    mib.pci,
                    slot,
                    allocation.first_symbol + carried,
                    6 * part.resource_blocks,
                );
                assigned
                    .iter()
                    .flat_map(|physical| {
                        (0..12).map(move |subcarrier| match subcarrier % 2 {
                            0 => sequence[6 * *physical + subcarrier / 2],
                            _ => Complex32::default(),
                        })
                    })
                    .collect()
            } else {
                (0..width)
                    .map(|_| {
                        let pair = pairs.next().unwrap();
                        Complex32::new(
                            amplitude * (1.0 - 2.0 * f32::from(pair[0])),
                            amplitude * (1.0 - 2.0 * f32::from(pair[1])),
                        )
                    })
                    .collect()
            };
            for (block, physical) in assigned.iter().enumerate() {
                let to = part.start + 12 * physical;
                symbol[to..to + 12].copy_from_slice(&region[12 * block..12 * (block + 1)]);
            }
        }
        (grid, payload)
    }

    /// The channel estimate a decoder recovers from a grid this module built, which has to be flat and
    /// unity for the round trips to mean what they claim. A transmitter that quietly scaled or turned the
    /// references would make every one of them a test of the estimator instead.
    pub fn flat(grid: &[Vec<Complex32>], mib: &Mib, dci: &Dci, part: Region, slot: u16) -> bool {
        let allocation = allocation(dci, mib.dmrs_type_a_position).unwrap();
        let blocks = usize::from(dci.resource_blocks);
        let offset = usize::from(dci.first_resource_block);
        let mapped: Vec<usize> = if dci.interleaved {
            interleave(part.resource_blocks)
        } else {
            (0..part.resource_blocks).collect()
        };
        let assigned = &mapped[offset..offset + blocks];
        let region: Vec<Vec<Complex32>> = grid
            .iter()
            .map(|symbol| {
                assigned
                    .iter()
                    .flat_map(|block| {
                        symbol[part.start + 12 * block..part.start + 12 * (block + 1)].to_vec()
                    })
                    .collect()
            })
            .collect();
        let Ok(Estimates { channel, phase }) = estimate(
            &region,
            assigned,
            allocation,
            mib,
            slot,
            part.resource_blocks,
        ) else {
            return false;
        };
        phase.iter().all(|(_, value)| value.abs() < 1e-3)
            && channel
                .iter()
                .flat_map(|(_, estimates)| estimates)
                .all(|estimate| (estimate.norm() - 1.0).abs() < 1e-3)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Allocation, Mapping, allocation, decode_sib1, interleave, references, transport_size,
        vector,
    };
    use crate::{Combine, Dci, Mib, Numerology, Reason, Region, SubcarrierSpacing};
    use num_complex::Complex32;

    fn mib() -> Mib {
        Mib {
            pci: 377,
            system_frame: 0,
            half_frame: false,
            subcarrier_spacing_common: SubcarrierSpacing::Khz15,
            ssb_subcarrier_offset: 0,
            dmrs_type_a_position: 2,
            pdcch_config_sib1: 0,
            cell_barred: false,
            intra_frequency_reselection: false,
            ssb_index: 0,
        }
    }

    fn dci(time: u8, mcs: u8) -> Dci {
        Dci {
            first_resource_block: 0,
            resource_blocks: 8,
            time,
            interleaved: false,
            mcs,
            redundancy: 0,
            system_information: false,
        }
    }

    #[test]
    fn the_default_table_fills_the_slot_behind_the_control_region() {
        assert_eq!(
            allocation(&dci(0, 5), 2),
            Ok(Allocation {
                mapping: Mapping::A,
                slots: 0,
                first_symbol: 2,
                symbols: 12,
            })
        );
        assert_eq!(
            allocation(&dci(0, 5), 3),
            Ok(Allocation {
                mapping: Mapping::A,
                slots: 0,
                first_symbol: 3,
                symbols: 11,
            })
        );
    }

    /// A row it cannot index is not a CRC failure. Reporting the two as one sends whoever reads it after
    /// a signal problem that is not there.
    #[test]
    fn each_refusal_names_itself_rather_than_reading_as_a_failed_decode() {
        assert!(matches!(
            allocation(&dci(0, 5), 4),
            Err(Reason::Unmapped {
                row: 0,
                dmrs_type_a_position: 4
            })
        ));
    }

    #[test]
    fn reference_symbols_follow_the_duration_from_the_start_of_the_slot() {
        let full = allocation(&dci(0, 5), 2).unwrap();
        assert_eq!(references(full, 2), vec![2, 7, 11]);
        let short = allocation(&dci(3, 5), 2).unwrap();
        assert_eq!(references(short, 2), vec![2, 7]);
        let shortest = allocation(&dci(4, 5), 2).unwrap();
        assert_eq!(references(shortest, 2), vec![2]);
    }

    /// Mapping type B counts its duration and its references from the allocation, not from the slot, so
    /// a four-symbol allocation opening at symbol 9 has its reference at 9 and never at
    /// `dmrs-TypeA-Position`. Measuring one on type A timing reads symbols that carry data.
    #[test]
    fn mapping_type_b_counts_its_references_from_the_allocation() {
        let late = allocation(&dci(5, 5), 2).unwrap();
        assert_eq!(
            (late.mapping, late.first_symbol, late.symbols),
            (Mapping::B, 9, 4)
        );
        assert_eq!(references(late, 2), vec![9]);
        let long = allocation(&dci(7, 5), 2).unwrap();
        assert_eq!((long.first_symbol, long.symbols), (5, 7));
        assert_eq!(references(long, 2), vec![5, 9]);
    }

    /// The interleaver is a permutation of whole bundles, so every block appears once and the last
    /// bundle stays where it is. A 48-block CORESET#0 gives 24 bundles in 12 columns, and its second
    /// virtual bundle sits halfway up the physical grid.
    #[test]
    fn interleaving_permutes_bundles_and_leaves_the_last_one_alone() {
        let wide = interleave(48);
        assert_eq!(wide.len(), 48);
        assert_eq!(&wide[..6], &[0, 1, 24, 25, 2, 3]);
        assert_eq!(&wide[46..], &[46, 47]);
        let mut sorted = wide.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..48).collect::<Vec<_>>());
        // An odd width leaves a one-block bundle, which maps to itself rather than off the end.
        let odd = interleave(25);
        assert_eq!(odd.len(), 25);
        assert_eq!(odd[24], 24);
        let mut sorted = odd.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..25).collect::<Vec<_>>());
    }

    /// Eight resource blocks over twelve symbols at MCS 5 is what the reference capture broadcasts,
    /// and its 640-bit block is the value the hand-checked fixture was built against.
    #[test]
    fn the_reference_allocation_sizes_its_transport_block_to_640_bits() {
        assert_eq!(transport_size(8, 12, 3, 2, 379), Some(640));
    }

    /// Every legal SIB1 transport configuration, against a transmitter that no local cell resembles.
    ///
    /// Each axis is swept on its own against a fixed remainder, because a grid sweep of all of them is
    /// thousands of LDPC decodes for no more information: what a failure has to name is the one setting
    /// that broke, and a combined sweep names the tuple.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_legal_broadcast_configuration_round_trips() {
        let part = Region {
            start: 96,
            resource_blocks: 48,
        };
        let mib = mib();
        let round_trip = |dci: &Dci, part: Region, slot: u16| {
            let (grid, payload) = vector::broadcast(&mib, dci, part, slot, 1024);
            assert!(
                vector::flat(&grid, &mib, dci, part, slot),
                "the transmitter did not present a unity channel for {dci:?}"
            );
            let decoded = decode_sib1(
                &grid,
                &mib,
                dci,
                Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap(),
                881.5e6,
                part,
                slot,
            );
            let expected: Vec<u8> = payload
                .as_chunks::<8>()
                .0
                .iter()
                .map(|byte| byte.iter().fold(0, |value, bit| 2 * value + *bit))
                .collect();
            assert_eq!(decoded.as_deref(), Ok(&expected[..]), "{dci:?} at {part:?}");
        };

        // Every MCS a UE may see on an SI-RNTI PDSCH. Above MCS 9 the qam64 table leaves QPSK, which
        // `decode_sib1` refuses as a misparsed DCI rather than a demapper to write.
        for mcs in 0..=9 {
            round_trip(&dci(0, mcs), part, 2);
        }
        // A SIB1 occasion cycles its redundancy version, and which of them read alone is arithmetic
        // rather than signal quality. RV 0 and RV 3 do: 3 starts far enough around the circular buffer
        // that this allocation wraps back onto the systematic bits. RV 1 and RV 2 start past them and do
        // not wrap far enough, so what they carry is parity for a block the decoder has never seen, and
        // no amount of signal fixes that. It is why `system_information` walks occasions looking for
        // RV 0 rather than retrying the occasion in front of it — and it is the same two versions a live
        // n5 cell offered three times in half a second while 32 retries changed nothing.
        for redundancy in [0, 3] {
            round_trip(
                &Dci {
                    redundancy,
                    ..dci(0, 4)
                },
                part,
                2,
            );
        }
        for redundancy in [1, 2] {
            let dci = Dci {
                redundancy,
                ..dci(0, 4)
            };
            let (grid, _) = vector::broadcast(&mib, &dci, part, 2, 1024);
            assert_eq!(
                decode_sib1(
                    &grid,
                    &mib,
                    &dci,
                    Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap(),
                    881.5e6,
                    part,
                    2,
                ),
                Err(Reason::Undecoded),
                "RV {redundancy} decoded alone, which would mean the occasion walk is unnecessary"
            );
        }
        // Mapping type A at each of its durations, and type B, which no cell in reach transmits.
        for time in [0, 1, 2, 7] {
            round_trip(&dci(time, 2), part, 2);
        }
        // Rows 4, 5 and 6 carry a single reference symbol, and a UE has to read them: TS 38.214 Table
        // 5.1.2.1.1-1 puts SI-RNTI in the Type0 common search space on the default A table, whose every
        // row a cell may schedule. They used to refuse as `Rotation`, because TS 38.211 section 5.4 has
        // the cell restart its carrier phase each symbol and one measurement cannot separate a slope
        // from an offset.
        //
        // It never needed to be measured. Section 5.4 makes the slope a function of the carrier, so the
        // two ends of it — compensated at the tuner's own frequency, or not compensated at all — are the
        // only readings there are, and the DL-SCH CRC says which one the cell transmitted.
        for time in [4, 5, 6] {
            round_trip(&dci(time, 2), part, 2);
        }
        // Interleaved VRB-to-PRB, which permutes the blocks the allocation reads and the sequence
        // elements it despreads them against, and which every cell in reach transmits without.
        for interleaved in [false, true] {
            for first_resource_block in [0, 5, 40] {
                round_trip(
                    &Dci {
                        interleaved,
                        first_resource_block,
                        resource_blocks: 8,
                        ..dci(0, 3)
                    },
                    part,
                    2,
                );
            }
        }
        // Allocation widths, which move the transport block size and with it the lifting size and which
        // of the four information-block widths TS 38.212 section 5.2.2 selects it by. MCS 0 throughout:
        // a wide allocation at a higher rate asks for a block past the 3840-bit ceiling of base graph
        // two, which SIB1 cannot reach at 2976 bits and which `segment` refuses rather than silently
        // moving to base graph one.
        for resource_blocks in [2, 4, 12, 24, 48] {
            round_trip(
                &Dci {
                    resource_blocks,
                    ..dci(0, 0)
                },
                Region {
                    start: 96,
                    resource_blocks: 48,
                },
                2,
            );
        }
        // A region against each edge of the captured window, which is where a bandwidth part that runs
        // off the transform reads as a failed CRC instead of as the geometry it is.
        for start in [0, 1024 - 12 * 48] {
            round_trip(
                &dci(0, 4),
                Region {
                    start,
                    resource_blocks: 48,
                },
                2,
            );
        }
        // The slot seeds every reference sequence, so a decoder reading the wrong one measures noise.
        for slot in [0, 7, 9] {
            round_trip(&dci(0, 4), part, slot);
        }
    }

    /// Two occasions that cannot be read alone, read together.
    ///
    /// This is the whole reason a redundancy version that punctures the systematic bits is not a dead
    /// end. The four versions are positions in one circular buffer rather than four codewords, so a
    /// receiver that accumulates them decodes what neither transmission carries by itself.
    ///
    /// Which sets read is measured here rather than reasoned about, because the rule is not the tidy one
    /// it looks like. Coverage of the buffer is what decides it, so it tightens with the code rate: at
    /// MCS 0 every version reads alone, by MCS 4 only RV 0 and RV 3 do, and at MCS 9 on four blocks only
    /// RV 0 does and the sole pair that reads is RV 2 with RV 3. Combining strictly helps at every rate
    /// and rescues no window at all at the highest — a capture that saw only RV 1 and RV 2 has to keep
    /// looking, which is why `system_information` walks to the end of the window rather than stopping
    /// once it has two occasions.
    #[test]
    fn occasions_that_cannot_be_read_alone_combine_into_one_that_can() {
        let part = Region {
            start: 96,
            resource_blocks: 48,
        };
        let mib = mib();
        // Accumulating only means anything if the occasions carry one transport block between them.
        let payload = |redundancy| {
            let dci = Dci {
                redundancy,
                ..dci(0, 4)
            };
            crate::ldpc::bytes(&vector::broadcast(&mib, &dci, part, 2, 1024).1)
        };
        assert_eq!(payload(1), payload(2));

        // Which redundancy versions read alone is a property of the code rate, not of the receiver.
        // Each version is a starting position in one circular buffer; a transmission long enough wraps
        // back onto the systematic bits and reads by itself, and a shorter one does not reach them.
        //
        // MCS 0 is low enough that every version wraps. MCS 4 is not, and MCS 9 on four blocks reaches
        // the case that makes combining worth having: no single occasion decodes and two together do.
        for (mcs, blocks, size, alone, pairs) in [
            (0, 4, 96, [true, true, true, true], [true, true, true]),
            (3, 8, 432, [true, false, true, true], [true, true, true]),
            (4, 8, 528, [true, false, false, true], [false, true, true]),
            (9, 4, 576, [true, false, false, false], [false, false, true]),
        ] {
            let read = |set: &[u8]| {
                let mut combined = None;
                for redundancy in set {
                    let dci = Dci {
                        redundancy: *redundancy,
                        mcs,
                        resource_blocks: blocks,
                        ..dci(0, 4)
                    };
                    let (grid, _) = vector::broadcast(&mib, &dci, part, 2, 1024);
                    let candidates = super::soft(
                        &grid,
                        &mib,
                        &dci,
                        Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap(),
                        881.5e6,
                        part,
                        2,
                    )
                    .unwrap();
                    let soft = &candidates[0];
                    assert_eq!(soft.size, size, "mcs {mcs} on {blocks} blocks");
                    let mut buffer = combined
                        .take()
                        .unwrap_or_else(|| Combine::new(soft.size, soft.rate).unwrap());
                    buffer.add(&soft.llr, soft.order, soft.redundancy).unwrap();
                    combined = Some(buffer);
                }
                let combined = combined.unwrap();
                assert_eq!(combined.occasions(), set.len());
                combined.read().is_ok()
            };
            for (redundancy, expected) in alone.iter().enumerate() {
                assert_eq!(
                    read(&[u8::try_from(redundancy).unwrap()]),
                    *expected,
                    "RV {redundancy} alone at MCS {mcs} on {blocks} blocks"
                );
            }
            for (set, expected) in [([1, 2], pairs[0]), ([1, 3], pairs[1]), ([2, 3], pairs[2])] {
                assert_eq!(
                    read(&set),
                    expected,
                    "RV {set:?} combined at MCS {mcs} on {blocks} blocks"
                );
            }
            // RV 1 and RV 2 both begin past the systematic bits and neither wraps back onto them, so
            // their union carries no systematic bit at any rate where either fails alone. A third
            // occasion is what fixes that, and the walk keeps accumulating until one arrives.
            assert!(
                read(&[1, 2, 3]),
                "three occasions at MCS {mcs} on {blocks} blocks"
            );
        }
    }

    /// The same transmitter with the channel spoiled, bounded so a failure is a regression and not a
    /// dice roll. Noise is seeded, and each level is stated as the thing it does to the constellation.
    #[test]
    fn a_bounded_impairment_envelope_still_reads() {
        let part = Region {
            start: 96,
            resource_blocks: 48,
        };
        let mib = mib();
        let dci = dci(0, 4);
        let (clean, payload) = vector::broadcast(&mib, &dci, part, 2, 1024);
        let expected: Vec<u8> = payload
            .as_chunks::<8>()
            .0
            .iter()
            .map(|byte| byte.iter().fold(0, |value, bit| 2 * value + *bit))
            .collect();
        let read = |grid: &[Vec<Complex32>]| {
            decode_sib1(
                grid,
                &mib,
                &dci,
                Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap(),
                881.5e6,
                part,
                2,
            )
            .as_deref()
                == Ok(&expected[..])
        };
        let spoil = |gain: Complex32, noise: f32, dc: f32| {
            let mut state = 0x2545_F491_4F6C_DD1D_u64;
            let mut random = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / 8_388_608.0 - 1.0
            };
            let mut grid = clean.clone();
            for symbol in &mut grid {
                for (subcarrier, sample) in symbol.iter_mut().enumerate() {
                    *sample = *sample * gain + Complex32::new(random(), random()) * noise;
                    if subcarrier == 512 {
                        *sample += Complex32::new(dc, 0.0);
                    }
                }
            }
            grid
        };
        // A flat channel of any gain and phase, which the estimator is meant to remove entirely.
        assert!(read(&spoil(Complex32::from_polar(0.05, 2.0), 0.0, 0.0)));
        assert!(read(&spoil(Complex32::from_polar(9.0, -1.0), 0.0, 0.0)));
        // Additive noise at the constellation's own amplitude, and half again as much, which is where it
        // stops. The point of the bound is that it is stated: a test asserting "always decodes" is a
        // test that fails on a day nobody changed anything.
        assert!(read(&spoil(Complex32::ONE, 1.1, 0.0)));
        assert!(!read(&spoil(Complex32::ONE, 1.5, 0.0)));
        // Oscillator leakage on the centre subcarrier at four times the wanted amplitude, which spoils
        // one subcarrier of the region and is carried by the code rate.
        assert!(read(&spoil(Complex32::ONE, 0.0, 4.0)));
    }

    /// A residual carrier offset the acquisition did not remove, at sizes a real one reaches. Nothing
    /// else covers the axis: every retained capture was tuned by the same acquisition, so they all
    /// carry whatever residual that leaves and none of them says what happens at another.
    ///
    /// It does not pin the size of the alias set, and cannot. This channel is flat, so the references
    /// fit their own slope exactly, only the two or three genuine aliases fit at all, and the reading
    /// the transmitter used is always near the front — a cap of one would very nearly pass. What
    /// crowds the set is a frequency-selective channel scattering the reference phases, and the thing
    /// that pins it is the `rogers-n105-dlsch` capture, which read nothing until the set was carried
    /// whole.
    #[test]
    fn a_residual_carrier_offset_reads_because_every_alias_is_carried() {
        let part = Region {
            start: 96,
            resource_blocks: 48,
        };
        let (mib, dci) = (mib(), dci(0, 4));
        let numerology = Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap();
        let center_hz = 881.5e6;
        let slot = 2;
        let (clean, payload) = vector::broadcast(&mib, &dci, part, slot, 1024);
        let expected: Vec<u8> = payload
            .as_chunks::<8>()
            .0
            .iter()
            .map(|byte| byte.iter().fold(0, |value, bit| 2 * value + *bit))
            .collect();
        let first_symbol = allocation(&dci, mib.dmrs_type_a_position)
            .unwrap()
            .first_symbol;
        let mut clock = 0;
        let times: Vec<f64> = (0..clean.len())
            .map(|index| {
                clock +=
                    numerology.prefix(14 * usize::from(slot) + usize::from(first_symbol) + index);
                let body = clock;
                clock += numerology.size;
                body as f64
            })
            .collect();
        for residual_hz in [0.0, 300.0, -1_500.0, 4_000.0, -6_900.0, 7_100.0] {
            let turned: Vec<Vec<Complex32>> = clean
                .iter()
                .zip(&times)
                .map(|(symbol, time)| {
                    let level = Complex32::from_polar(
                        1.0,
                        (std::f64::consts::TAU * residual_hz * time
                            / numerology.sample_rate_hz as f64) as f32,
                    );
                    symbol.iter().map(|sample| sample * level).collect()
                })
                .collect();
            assert_eq!(
                decode_sib1(&turned, &mib, &dci, numerology, center_hz, part, slot).as_deref(),
                Ok(&expected[..]),
                "a residual of {residual_hz} Hz"
            );
        }
    }

    #[test]
    fn an_allocation_with_no_room_for_data_has_no_transport_block() {
        assert_eq!(transport_size(1, 1, 1, 2, 120), None);
    }
}
