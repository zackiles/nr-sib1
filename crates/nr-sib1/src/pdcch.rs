use num_complex::Complex32;

use crate::gold;
use crate::polar::decode_masked;
use crate::{Config, Crc, Mib, SubcarrierSpacing};

/// CORESET#0 and the Type0-PDCCH common search space that `pdcch-ConfigSIB1` selects, per TS 38.213
/// section 13, for the SS/PBCH block that carried the MIB.
///
/// The occasion is the two consecutive slots from `slot`, in a frame whose system frame number has
/// parity `frame`. Only the first is the tabulated slot; the reference capture broadcasts SIB1 in
/// the second, so a decoder that reads the table as a single slot finds nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Monitor {
    /// Which table of TS 38.213 section 13 the index resolved against. A cell that reaches MIB and
    /// not SIB1 can only be diagnosed if the record says which geometry it was given, not merely
    /// which index selected it.
    pub table: &'static str,
    pub resource_blocks: u16,
    pub symbols: u8,
    /// Resource blocks from the first common resource block of the SS/PBCH block down to the first
    /// of CORESET#0, at the common subcarrier spacing.
    pub offset: u8,
    /// Spacing of the control region, which is the common spacing the MIB signalled and need not be
    /// the spacing of the block that carried it.
    pub spacing: SubcarrierSpacing,
    /// Spacing of the block that carried the MIB. Kept beside `spacing` because the offset and the
    /// block's own width are counted in different grids whenever the two differ, and `coreset` needs
    /// both to place the region at all.
    pub block: SubcarrierSpacing,
    /// Whether control elements are mapped to resource-element groups through the interleaver. True
    /// everywhere except Table 13-0 indices 6 to 9, whose geometry is identical to indices 2 to 5 and
    /// which differ in this alone.
    pub interleaved: bool,
    pub slot: u16,
    pub first_symbol: u8,
    pub frame: u8,
}

/// Where CORESET#0 sits under the SS/PBCH block that signalled it, counted in 15 kHz subcarriers.
///
/// That unit is the only exact one. Three quantities meet here and no two of them share a grid: `k_SSB`
/// counts 15 kHz subcarriers at every FR1 spacing (TS 38.211 section 7.4.1.4), the Type0 table's offset
/// counts resource blocks of the *common* spacing, and the block's own half-width counts 120
/// subcarriers of the *block's* spacing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coreset {
    /// 15 kHz subcarriers from the centre of the block down to the lowest subcarrier of the region.
    pub below: usize,
    /// Width of the region in 15 kHz subcarriers.
    pub width: usize,
}

impl Coreset {
    /// The absolute frequency span of the region, for a block centred at `ssb_hz`.
    #[must_use]
    pub fn span(self, ssb_hz: f64) -> std::ops::Range<f64> {
        let start = ssb_hz - self.below as f64 * 15_000.0;
        start..start + self.width as f64 * 15_000.0
    }
}

/// Geometry of the control region the MIB pointed at.
///
/// IMPORTANT: one implementation, called by the decoder and by `plan`. A planner that works this out
/// for itself drifts from the decoder and then plans captures the decoder rejects, which is a worse
/// failure than the one it was written to fix.
///
/// DANGER: the offset is in resource blocks of the *control region's* spacing and the block's half-width
/// is in subcarriers of the *block's*, which TS 38.213 section 13 states once for all of Tables 13-0
/// through 13-10A rather than noting it on any of them. Scaling both by one grid is right for `{15,15}`
/// and `{30,30}` and puts the region 1.8 MHz out on `{15,30}` and `{30,15}` — an error invisible until
/// a mixed-spacing cell is met, and indistinguishable from a weak signal when it is.
#[must_use]
pub fn coreset(mib: &Mib, monitor: &Monitor) -> Coreset {
    let common = (monitor.spacing.hz() / 15_000) as usize;
    let block = (monitor.block.hz() / 15_000) as usize;
    Coreset {
        below: 120 * block
            + 12 * usize::from(monitor.offset) * common
            + usize::from(mib.ssb_subcarrier_offset),
        width: 12 * usize::from(monitor.resource_blocks) * common,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dci {
    pub first_resource_block: u16,
    pub resource_blocks: u16,
    pub time: u8,
    pub interleaved: bool,
    pub mcs: u8,
    pub redundancy: u8,
    pub system_information: bool,
}

/// Aggregation levels of the Type0-PDCCH common search space with their candidate counts, per
/// TS 38.213 Table 10.1-1. Levels 1 and 2 are not monitored there, so searching them would only
/// invite a false CRC.
const AGGREGATION: [(usize, usize); 3] = [(4, 4), (8, 2), (16, 1)];

/// One CORESET#0 table of TS 38.213 section 13, keyed the way the specification keys them: on the
/// pair of SS/PBCH and PDCCH subcarrier spacings and on the minimum channel bandwidth of the band.
///
/// DANGER: a wrong row here is far worse than a missing table. A refusal costs one cell; confidently
/// wrong geometry decodes something the cell never transmitted, or fails a CRC in a way that reads as
/// a weak signal. Transcribe only from the published specification, and pin the row in a test.
struct Type0 {
    table: &'static str,
    ssb: SubcarrierSpacing,
    common: SubcarrierSpacing,
    /// Minimum channel bandwidths in MHz the table covers.
    minimum_mhz: &'static [u16],
    /// (resource blocks, symbols, offset in resource blocks of the common spacing), indexed by
    /// `controlResourceSetZero`. A row of no resource blocks is reserved.
    rows: [(u16, u8, u8); 16],
    /// Rows that use non-interleaved CCE-to-REG mapping. Only Table 13-0 has any, where indices 6 to 9
    /// repeat the geometry of 2 to 5 and differ in nothing else — so a decoder that ignored this would
    /// read four rows as duplicates of four others and fail their CRCs.
    sequential: &'static [u8],
}

/// Bands whose CORESET#0 comes from Tables 13-5 and 13-6 whatever their minimum channel bandwidth,
/// per note 17 of TS 38.101-1 Table 5.2-1.
///
/// n79's own minimum is 10 MHz, not 40 — this note, and nothing about its channel widths, is why it
/// reads the 40 MHz tables. n104 carries the same note and sits above the radio's ceiling.
const NOTE_17: [u16; 2] = [79, 104];

/// The Type0-PDCCH tables this decoder maps, transcribed from TS 38.213 V19.4.0 section 13. A
/// combination with no entry is refused rather than mapped onto the nearest table that happens to
/// exist.
///
/// A 3 MHz minimum channel bandwidth selects two tables rather than one, and both are carried. Table
/// 13-0 covers a 3 or 5 MHz carrier and Table 13-1 covers anything wider than 3 MHz, so at a 5 MHz
/// carrier both captions apply — and the carrier's width is only known from the SIB1 being decoded.
/// `monitors` therefore returns both as candidates and the DCI CRC resolves which the cell meant. That
/// is preferable to declaring the case impossible: the alternative refuses every cell on n26, n28, n85
/// and n109 for want of one bit that arrives after the decode.
///
/// Tables 13-1A and 13-4A are absent for a plainer reason: no band in the catalog is operated with
/// shared spectrum channel access, so mapping them would add rows nothing can reach.
const TYPE0: [Type0; 7] = [
    Type0 {
        table: "38.213 13-1",
        ssb: SubcarrierSpacing::Khz15,
        common: SubcarrierSpacing::Khz15,
        // Its caption reads "5 MHz or 10 MHz, or minimum channel bandwidth 3 MHz and channel bandwidth
        // larger than 3 MHz", so a 3 MHz band belongs here too whenever its carrier is wider than that.
        minimum_mhz: &[3, 5, 10],
        rows: [
            (24, 2, 0),
            (24, 2, 2),
            (24, 2, 4),
            (24, 3, 0),
            (24, 3, 2),
            (24, 3, 4),
            (48, 1, 12),
            (48, 1, 16),
            (48, 2, 12),
            (48, 2, 16),
            (48, 3, 12),
            (48, 3, 16),
            (96, 1, 38),
            (96, 2, 38),
            (96, 3, 38),
            (0, 0, 0),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-2",
        ssb: SubcarrierSpacing::Khz15,
        common: SubcarrierSpacing::Khz30,
        minimum_mhz: &[5, 10],
        rows: [
            (24, 2, 5),
            (24, 2, 6),
            (24, 2, 7),
            (24, 2, 8),
            (24, 3, 5),
            (24, 3, 6),
            (24, 3, 7),
            (24, 3, 8),
            (48, 1, 18),
            (48, 1, 20),
            (48, 2, 18),
            (48, 2, 20),
            (48, 3, 18),
            (48, 3, 20),
            (0, 0, 0),
            (0, 0, 0),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-3",
        ssb: SubcarrierSpacing::Khz30,
        common: SubcarrierSpacing::Khz15,
        minimum_mhz: &[5, 10],
        rows: [
            (48, 1, 2),
            (48, 1, 6),
            (48, 2, 2),
            (48, 2, 6),
            (48, 3, 2),
            (48, 3, 6),
            (96, 1, 28),
            (96, 2, 28),
            (96, 3, 28),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-4",
        ssb: SubcarrierSpacing::Khz30,
        common: SubcarrierSpacing::Khz30,
        minimum_mhz: &[5, 10],
        rows: [
            (24, 2, 0),
            (24, 2, 1),
            (24, 2, 2),
            (24, 2, 3),
            (24, 2, 4),
            (24, 3, 0),
            (24, 3, 1),
            (24, 3, 2),
            (24, 3, 3),
            (24, 3, 4),
            (48, 1, 12),
            (48, 1, 14),
            (48, 1, 16),
            (48, 2, 12),
            (48, 2, 14),
            (48, 2, 16),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-5",
        ssb: SubcarrierSpacing::Khz30,
        common: SubcarrierSpacing::Khz15,
        minimum_mhz: &[40],
        rows: [
            (48, 1, 4),
            (48, 2, 4),
            (48, 3, 4),
            (96, 1, 0),
            (96, 1, 56),
            (96, 2, 0),
            (96, 2, 56),
            (96, 3, 0),
            (96, 3, 56),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-6",
        ssb: SubcarrierSpacing::Khz30,
        common: SubcarrierSpacing::Khz30,
        minimum_mhz: &[40],
        rows: [
            (24, 2, 0),
            (24, 2, 4),
            (24, 3, 0),
            (24, 3, 4),
            (48, 1, 0),
            (48, 1, 28),
            (48, 2, 0),
            (48, 2, 28),
            (48, 3, 0),
            (48, 3, 28),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
        ],
        sequential: &[],
    },
    Type0 {
        table: "38.213 13-0",
        ssb: SubcarrierSpacing::Khz15,
        common: SubcarrierSpacing::Khz15,
        minimum_mhz: &[3],
        rows: [
            (12, 2, 0),
            (12, 3, 0),
            (24, 2, 0),
            (24, 2, 2),
            (24, 3, 0),
            (24, 3, 2),
            (24, 2, 0),
            (24, 2, 2),
            (24, 3, 0),
            (24, 3, 2),
            (24, 2, 0),
            (24, 3, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
        ],
        sequential: &[6, 7, 8, 9],
    },
];

/// The one Type0-PDCCH configuration to plan a capture around, which is the widest candidate.
///
/// A capture has to hold whichever hypothesis turns out to be the cell's, so the planner takes the
/// widest rather than the likeliest.
#[must_use]
pub fn monitor(config: &Config, mib: &Mib) -> Option<Monitor> {
    monitors(config, mib)
        .into_iter()
        .max_by_key(|monitor| monitor.resource_blocks)
}

/// Every Type0-PDCCH configuration `pdcch-ConfigSIB1` can mean, given what is known before SIB1.
///
/// Usually one. A band whose minimum channel bandwidth is 3 MHz gets two, because Table 13-0 covers a
/// 3 or 5 MHz carrier and Table 13-1 covers anything wider, and the carrier's width arrives in the SIB1
/// this is being resolved in order to read. Both are standards-valid until a DCI CRC picks one, which is
/// why they are returned rather than one of them being guessed at or the case declared impossible.
///
/// A band operated with shared spectrum channel access gets none. Its rows live in Tables 13-1A and
/// 13-4A, which are not mapped, and every such band also publishes a minimum channel bandwidth that
/// selects a mapped table perfectly happily — so the refusal has to be on the channel access and not
/// on the width. n46 was read against Table 13-1 for months on exactly that mistake.
#[must_use]
pub fn monitors(config: &Config, mib: &Mib) -> Vec<Monitor> {
    /// TS 38.213 Table 13-11 as (O, twice M, first symbol), where a first symbol of -1 means the
    /// SS/PBCH block index selects it. The tabulated number of search space sets per slot is
    /// implied by that rule, so it is not carried here.
    const SEARCH: [(u16, u16, i8); 16] = [
        (0, 2, 0),
        (0, 1, -1),
        (2, 2, 0),
        (2, 1, -1),
        (5, 2, 0),
        (5, 1, -1),
        (7, 2, 0),
        (7, 1, -1),
        (0, 4, 0),
        (5, 4, 0),
        (0, 2, 1),
        (0, 2, 2),
        (2, 2, 1),
        (2, 2, 2),
        (5, 2, 1),
        (5, 2, 2),
    ];
    if config.shared_spectrum {
        return Vec::new();
    }
    let minimum = if NOTE_17.contains(&config.band) {
        40
    } else {
        (config.minimum_channel_bandwidth_hz / 1e6).round() as u16
    };
    let row = usize::from(mib.pdcch_config_sib1 >> 4);
    let (start, twice, first) = SEARCH[usize::from(mib.pdcch_config_sib1 & 15)];
    let index = u16::from(mib.ssb_index);
    let slots_per_frame = 10_u16 << mib.subcarrier_spacing_common.mu();
    let occasion = start * (1 << mib.subcarrier_spacing_common.mu()) + index * twice / 2;
    TYPE0
        .iter()
        .filter(|entry| {
            entry.ssb == config.spacing
                && entry.common == mib.subcarrier_spacing_common
                && entry.minimum_mhz.contains(&minimum)
        })
        .filter_map(|entry| {
            let (resource_blocks, symbols, offset) = entry.rows[row];
            (resource_blocks != 0).then_some(Monitor {
                table: entry.table,
                resource_blocks,
                symbols,
                offset,
                spacing: mib.subcarrier_spacing_common,
                block: config.spacing,
                interleaved: !entry.sequential.contains(&(row as u8)),
                slot: occasion % slots_per_frame,
                first_symbol: match u8::try_from(first) {
                    Ok(symbol) => symbol,
                    Err(_) if index % 2 == 0 => 0,
                    Err(_) => symbols,
                },
                frame: (occasion / slots_per_frame % 2) as u8,
            })
        })
        .collect()
}

/// Recovers the SI-RNTI DCI from the CORESET#0 symbols of one monitoring occasion. `start` is the
/// subcarrier of the first CORESET#0 resource block within each symbol, and `slot` and `symbol` its
/// position in the frame, both of which the demodulation reference sequence depends on.
#[must_use]
pub fn decode_dci(
    symbols: &[Vec<Complex32>],
    monitor: &Monitor,
    pci: u16,
    start: usize,
    slot: u16,
    symbol: u8,
) -> Option<Dci> {
    let count = usize::from(monitor.symbols);
    if symbols.len() != count {
        return None;
    }
    let width = usize::from(monitor.resource_blocks) * 12;
    let region = symbols
        .iter()
        .map(|symbol| symbol.get(start..start + width))
        .collect::<Option<Vec<_>>>()?;
    let reference: Vec<Vec<Complex32>> = (0..count)
        .map(|index| {
            let position = u64::from(symbol) + index as u64;
            let initial =
                ((1 << 17) * (14 * u64::from(slot) + position + 1) * (2 * u64::from(pci) + 1)
                    + 2 * u64::from(pci))
                    % (1 << 31);
            qpsk(initial as u32, width / 4)
        })
        .collect();
    let controls = width / 12 * count / 6;
    let columns = controls / INTERLEAVER;
    let row = 72 / count;
    let payload = assignment(monitor.resource_blocks)? + 28;
    for (aggregation, candidates) in AGGREGATION {
        if aggregation > controls {
            continue;
        }
        let mut searched = Vec::with_capacity(candidates);
        for candidate in 0..candidates {
            let first = aggregation
                * (candidate * controls / (aggregation * candidates) % (controls / aggregation));
            if searched.contains(&first) {
                continue;
            }
            searched.push(first);
            let mut bundles: Vec<usize> = (first..first + aggregation)
                .map(|control| {
                    if monitor.interleaved {
                        ((control % INTERLEAVER) * columns
                            + control / INTERLEAVER
                            + usize::from(pci))
                            % controls
                    } else {
                        control
                    }
                })
                .collect();
            bundles.sort_unstable();
            let mut llr = Vec::with_capacity(aggregation * 108);
            for (index, samples) in region.iter().enumerate() {
                for bundle in &bundles {
                    equalize(
                        &samples[bundle * row..(bundle + 1) * row],
                        &reference[index][bundle * row / 4..(bundle + 1) * row / 4],
                        &mut llr,
                    );
                }
            }
            if llr.len() != aggregation * 108 {
                continue;
            }
            let scrambling = gold(u32::from(pci), llr.len());
            for (value, bit) in llr.iter_mut().zip(scrambling) {
                if bit == 1 {
                    *value = -*value;
                }
            }
            if let Some(decoded) = decode_masked(&llr, payload + 24, 32, Crc::Crc24C, 0xffff)
                && let Some(dci) = parse(&decoded[..payload], monitor.resource_blocks)
            {
                return Some(dci);
            }
        }
    }
    None
}

/// Interleaver depth of CORESET#0, which TS 38.211 section 7.3.2.2 fixes at 2 along with a bundle
/// size of 6 and a shift of the physical cell identity.
const INTERLEAVER: usize = 2;

/// Divides one REG bundle into its demodulation reference and its control elements, equalizes the
/// latter against a channel interpolated across the former, and appends the soft bits. A bundle with
/// no power appends nothing, which leaves the caller's soft bits short of the candidate it was
/// building.
///
/// The reference sits on every fourth subcarrier from the second, counted from the first resource
/// block of CORESET#0, per TS 38.211 section 7.4.1.3.2. Interpolation stays inside the bundle
/// because a bundle is the largest span the cell is allowed to precode as one.
///
/// DANGER: this combines rather than divides, and the difference is not stylistic. Dividing by a
/// channel estimate turns a faded subcarrier into an enormous soft bit and hands the decoder noise
/// with more confidence than it gives the signal, and guarding that division against an absolute
/// floor makes the whole decode depend on the scale of the samples: a capture in normalised floats
/// trips a guard that the same signal in ADC counts sails through. Combining weights each subcarrier
/// by its own strength, which is what a soft-decision decoder wants, and has no singularity.
fn equalize(samples: &[Complex32], reference: &[Complex32], llr: &mut Vec<f32>) {
    let channel: Vec<Complex32> = reference
        .iter()
        .enumerate()
        .map(|(index, expected)| samples[4 * index + 1] * expected.conj())
        .collect();
    let power = channel.iter().map(Complex32::norm_sqr).sum::<f32>() / channel.len() as f32;
    if power == 0.0 {
        return;
    }
    for (index, sample) in samples.iter().enumerate() {
        if index % 4 == 1 {
            continue;
        }
        let position = (index as f32 - 1.0) / 4.0;
        let lower = (position.floor().max(0.0) as usize).min(channel.len() - 1);
        let upper = (lower + 1).min(channel.len() - 1);
        let weight = (position - lower as f32).clamp(0.0, 1.0);
        let estimate = channel[lower] * (1.0 - weight) + channel[upper] * weight;
        let value = sample * estimate.conj() / power;
        llr.push(value.re);
        llr.push(value.im);
    }
}

fn qpsk(initial: u32, length: usize) -> Vec<Complex32> {
    let sequence = gold(initial, 2 * length);
    (0..length)
        .map(|index| {
            Complex32::new(
                std::f32::consts::FRAC_1_SQRT_2 * (1.0 - 2.0 * f32::from(sequence[2 * index])),
                std::f32::consts::FRAC_1_SQRT_2 * (1.0 - 2.0 * f32::from(sequence[2 * index + 1])),
            )
        })
        .collect()
}

/// Width of the frequency-domain resource assignment of DCI format `1_0`, per TS 38.212 section
/// 7.3.1.2.1.
fn assignment(bandwidth: u16) -> Option<usize> {
    let combinations = u32::from(bandwidth) * (u32::from(bandwidth) + 1) / 2;
    (combinations > 1).then(|| combinations.next_power_of_two().ilog2() as usize)
}

fn parse(bits: &[u8], bandwidth: u16) -> Option<Dci> {
    let width = assignment(bandwidth)?;
    if bits.len() != width + 28 {
        return None;
    }
    let mut offset = 0;
    let value = take(bits, &mut offset, width) as u16;
    // TS 38.214 section 5.1.2.2.2 folds allocations wider than half the bandwidth part into the
    // upper half of the value space, so the direct quotient is only the first of two branches.
    let mut resource_blocks = value / bandwidth + 1;
    let mut first_resource_block = value % bandwidth;
    if resource_blocks > bandwidth - first_resource_block {
        resource_blocks = (bandwidth + 2).checked_sub(resource_blocks)?;
        first_resource_block = bandwidth - 1 - first_resource_block;
    }
    if resource_blocks == 0 || first_resource_block + resource_blocks > bandwidth {
        return None;
    }
    let time = take(bits, &mut offset, 4) as u8;
    let interleaved = take(bits, &mut offset, 1) == 1;
    let mcs = take(bits, &mut offset, 5) as u8;
    // A UE is not expected to decode an SI-RNTI PDSCH above QPSK (TS 38.214 section 5.1.3.1), and this
    // search only ever masks with SI-RNTI, so a higher order is a false CRC rather than a modulation
    // this decoder lacks. Invalidating the candidate lets the remaining aggregation levels be tried;
    // reporting it instead would abandon the cell on the strength of a payload it never sent.
    if crate::modulation(mcs)?.0 > 2 {
        return None;
    }
    let redundancy = take(bits, &mut offset, 2) as u8;
    let system_information = take(bits, &mut offset, 1) == 1;
    if bits.get(offset..)?.iter().any(|bit| *bit != 0) {
        return None;
    }
    Some(Dci {
        first_resource_block,
        resource_blocks,
        time,
        interleaved,
        mcs,
        redundancy,
        system_information,
    })
}

fn take(bits: &[u8], offset: &mut usize, length: usize) -> u32 {
    let value = bits[*offset..*offset + length]
        .iter()
        .fold(0, |value, bit| 2 * value + u32::from(*bit));
    *offset += length;
    value
}

#[cfg(test)]
mod tests {
    use crate::{Duplex, Guard, Release, SsbCase};

    use super::{Config, Dci, Mib, Monitor, SubcarrierSpacing, coreset, monitor, monitors, parse};

    fn config(spacing: SubcarrierSpacing, minimum_channel_bandwidth_hz: f64, band: u16) -> Config {
        Config {
            release: Release::R18,
            band,
            duplex: Duplex::Fdd,
            sample_rate_hz: 30.72e6,
            center_hz: 881.5e6,
            usable_hz: 23.04e6,
            minimum_channel_bandwidth_hz,
            spacing,
            ssb_case: SsbCase::A,
            gscn: None,
            shared_spectrum: false,
            ntn: false,
            minimum_quality_db: 6.0,
            guard: Guard::default(),
        }
    }

    fn mib(pdcch_config_sib1: u8, ssb_index: u8) -> Mib {
        Mib {
            pci: 377,
            system_frame: 136,
            subcarrier_spacing_common: SubcarrierSpacing::Khz15,
            ssb_subcarrier_offset: 0,
            dmrs_type_a_position: 2,
            pdcch_config_sib1,
            cell_barred: false,
            intra_frequency_reselection: true,
            ssb_index,
            half_frame: false,
        }
    }

    #[test]
    fn sib1_assignment_round_trips_the_reference_fields() {
        let mut bits = Vec::new();
        for (value, length) in [
            (168_u32, 9),
            (0, 4),
            (0, 1),
            (5, 5),
            (0, 2),
            (0, 1),
            (0, 15),
        ] {
            bits.extend((0..length).rev().map(|shift| ((value >> shift) & 1) as u8));
        }
        assert_eq!(
            parse(&bits, 24),
            Some(Dci {
                first_resource_block: 0,
                resource_blocks: 8,
                time: 0,
                interleaved: false,
                mcs: 5,
                redundancy: 0,
                system_information: false,
            })
        );
    }

    /// SIB1 is never sent above QPSK (TS 38.214 section 5.1.3.1) and this search only masks with
    /// SI-RNTI, so a 16QAM MCS is a CRC that passed on a payload the cell never sent. Accepting it
    /// would abandon the cell on that payload instead of trying the candidates left.
    #[test]
    fn a_modulation_sib1_is_never_sent_at_invalidates_the_candidate() {
        let bits = |mcs: u32| {
            let mut bits = Vec::new();
            for (value, length) in [
                (168_u32, 9),
                (0, 4),
                (0, 1),
                (mcs, 5),
                (0, 2),
                (0, 1),
                (0, 15),
            ] {
                bits.extend((0..length).rev().map(|shift| ((value >> shift) & 1) as u8));
            }
            bits
        };
        assert!(parse(&bits(9), 24).is_some());
        assert_eq!(parse(&bits(10), 24), None);
    }

    /// TS 38.214 section 5.1.2.2.2 encodes 36 of 48 resource blocks from offset 0 as 671, which is
    /// past the fold because the length exceeds half the bandwidth part.
    #[test]
    fn wide_assignments_decode_through_the_folded_branch() {
        let mut bits = Vec::new();
        for (value, length) in [
            (671_u32, 11),
            (0, 4),
            (0, 1),
            (2, 5),
            (0, 2),
            (0, 1),
            (0, 15),
        ] {
            bits.extend((0..length).rev().map(|shift| ((value >> shift) & 1) as u8));
        }
        let dci = parse(&bits, 48).unwrap();
        assert_eq!((dci.first_resource_block, dci.resource_blocks), (0, 36));
    }

    #[test]
    fn live_n71_selectors_address_the_normative_common_search_space() {
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 71), &mib(0x7b, 2)),
            Some(Monitor {
                table: "38.213 13-1",
                resource_blocks: 48,
                symbols: 1,
                offset: 16,
                spacing: SubcarrierSpacing::Khz15,
                block: SubcarrierSpacing::Khz15,
                interleaved: true,
                slot: 2,
                first_symbol: 2,
                frame: 0,
            })
        );
    }

    #[test]
    fn the_reference_capture_selectors_open_the_first_slot_of_an_even_frame() {
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &mib(0, 0)),
            Some(Monitor {
                table: "38.213 13-1",
                resource_blocks: 24,
                symbols: 2,
                offset: 0,
                spacing: SubcarrierSpacing::Khz15,
                block: SubcarrierSpacing::Khz15,
                interleaved: true,
                slot: 0,
                first_symbol: 0,
                frame: 0,
            })
        );
    }

    /// PCI 577 on n77/n78: 30 kHz throughout, `controlResourceSetZero` 11 and `searchSpaceZero` 5.
    /// Row 11 of Table 13-4 is 48 resource blocks over one symbol at an offset of 14, which is
    /// 17.28 MHz of control region straddling the block almost symmetrically — the geometry that no
    /// 30.72 `MSps` tile could hold unless the block sat near its centre.
    #[test]
    fn the_thirty_kilohertz_table_places_a_wide_region_astride_its_block() {
        let mib = Mib {
            subcarrier_spacing_common: SubcarrierSpacing::Khz30,
            ..mib(0xb5, 3)
        };
        let monitor = monitor(&config(SubcarrierSpacing::Khz30, 10.0e6, 78), &mib).unwrap();
        assert_eq!(monitor.table, "38.213 13-4");
        assert_eq!(
            (monitor.resource_blocks, monitor.symbols, monitor.offset),
            (48, 1, 14)
        );
        assert_eq!(monitor.slot, 11);
        let region = coreset(&mib, &monitor);
        let span = region.span(3_479_526_000.0);
        assert!((span.end - span.start - 17_280_000.0).abs() < f64::EPSILON);
        assert!((span.start - (3_479_526_000.0 - 8_640_000.0)).abs() < f64::EPSILON);
    }

    /// The four windows that reached PCI 577 and never read it, from
    /// `data/sessions/1787528615-nr-audit`. Every one of them needed more window than a 30.72 `MSps`
    /// tile passes, which is the whole of that cell's failure: two reported a control region below the
    /// capture and two reported a DCI whose CRC did not validate, and neither described the air.
    #[test]
    fn every_window_that_reached_the_unread_cell_was_too_narrow_for_it() {
        for (center_hz, ssb_hz, index) in [
            (3_470_507_000.0, 3_479_519_570.0, 0xb5),
            (3_468_923_000.0, 3_479_526_000.0, 0xb5),
            (3_648_812_000.0, 3_639_363_412.0, 0xa5),
            (3_639_357_317.0 + 9.45e6, 3_639_357_317.0, 0xa5),
        ] {
            let mib = Mib {
                subcarrier_spacing_common: SubcarrierSpacing::Khz30,
                ..mib(index, 3)
            };
            let span = coreset(
                &mib,
                &monitor(&config(SubcarrierSpacing::Khz30, 10.0e6, 78), &mib).unwrap(),
            )
            .span(ssb_hz);
            let required = 2.0
                * (span.start - center_hz)
                    .abs()
                    .max((span.end - center_hz).abs());
            assert!(
                required > 30.72e6 * 0.75,
                "{center_hz} needed only {required} Hz, so narrowness was not its problem"
            );
        }
    }

    /// DANGER: `k_SSB` counts 15 kHz subcarriers at every FR1 spacing, so it moves the region half as
    /// far on a 30 kHz grid as on a 15 kHz one. Reading it as grid subcarriers is what once put every
    /// 30 kHz cell's control region nearly two resource blocks below where it was transmitted.
    #[test]
    fn the_subcarrier_offset_reaches_the_same_distance_at_either_spacing() {
        let fifteen = Mib {
            ssb_subcarrier_offset: 8,
            ..mib(0, 0)
        };
        let thirty = Mib {
            subcarrier_spacing_common: SubcarrierSpacing::Khz30,
            ssb_subcarrier_offset: 8,
            ..mib(0, 0)
        };
        let below =
            |mib: &Mib, ssb| coreset(mib, &monitor(&config(ssb, 5.0e6, 3), mib).unwrap()).below;
        assert_eq!(
            below(&fifteen, SubcarrierSpacing::Khz15) - 120,
            below(&thirty, SubcarrierSpacing::Khz30) - 240
        );
    }

    #[test]
    fn a_search_space_that_defers_to_the_block_index_alternates_with_it() {
        let even = monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &mib(0x01, 0)).unwrap();
        let odd = monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &mib(0x01, 1)).unwrap();
        assert_eq!(even.first_symbol, 0);
        assert_eq!(odd.first_symbol, even.symbols);
        assert_eq!((even.slot, odd.slot), (0, 0));
    }

    #[test]
    fn unsupported_configurations_are_refused_rather_than_mapped_onto_the_wrong_grid() {
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &mib(0xf0, 0)),
            None
        );
        // {15, 15} has no 40 MHz table: only the 30 kHz block families reach that minimum.
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 40.0e6, 3), &mib(0, 0)),
            None
        );
        // A minimum nothing publishes, which is what an unmapped band arrives as.
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 25.0e6, 3), &mib(0, 0)),
            None
        );
    }

    /// One row of each mixed-spacing and 40 MHz table, transcribed from TS 38.213 V19.4.0 section 13.
    ///
    /// Every one of these was a refusal before, so a cell using any of them read as an unsupported
    /// configuration rather than as a cell. The rows are pinned individually because a table is only as
    /// good as its data: geometry from the wrong row decodes something the cell never sent.
    #[test]
    fn each_added_table_resolves_the_row_the_specification_publishes() {
        let with = |common, index: u8| Mib {
            subcarrier_spacing_common: common,
            ..mib(index << 4, 0)
        };
        // Table 13-2 {15, 30} row 9: 48 resource blocks, one symbol, offset 20.
        let thirteen_two = with(SubcarrierSpacing::Khz30, 9);
        let resolved = monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &thirteen_two).unwrap();
        assert_eq!(resolved.table, "38.213 13-2");
        assert_eq!(
            (resolved.resource_blocks, resolved.symbols, resolved.offset),
            (48, 1, 20)
        );
        // Table 13-3 {30, 15} row 6: 96 resource blocks, one symbol, offset 28.
        let thirteen_three = with(SubcarrierSpacing::Khz15, 6);
        let resolved = monitor(
            &config(SubcarrierSpacing::Khz30, 10.0e6, 41),
            &thirteen_three,
        )
        .unwrap();
        assert_eq!(resolved.table, "38.213 13-3");
        assert_eq!(
            (resolved.resource_blocks, resolved.symbols, resolved.offset),
            (96, 1, 28)
        );
        // Table 13-5 {30, 15} row 4: 96 resource blocks, one symbol, offset 56.
        let thirteen_five = with(SubcarrierSpacing::Khz15, 4);
        let resolved =
            monitor(&config(SubcarrierSpacing::Khz30, 40.0e6, 1), &thirteen_five).unwrap();
        assert_eq!(resolved.table, "38.213 13-5");
        assert_eq!(
            (resolved.resource_blocks, resolved.symbols, resolved.offset),
            (96, 1, 56)
        );
        // Table 13-6 {30, 30} row 5: 48 resource blocks, one symbol, offset 28.
        let thirteen_six = with(SubcarrierSpacing::Khz30, 5);
        let resolved =
            monitor(&config(SubcarrierSpacing::Khz30, 40.0e6, 1), &thirteen_six).unwrap();
        assert_eq!(resolved.table, "38.213 13-6");
        assert_eq!(
            (resolved.resource_blocks, resolved.symbols, resolved.offset),
            (48, 1, 28)
        );
    }

    /// n79 reads the 40 MHz tables through note 17 of TS 38.101-1 Table 5.2-1 and not through its own
    /// channel widths, whose minimum is 10 MHz. Keying that on the bandwidth alone would have sent it to
    /// Table 13-4, where index 5 is 24 resource blocks rather than 48.
    #[test]
    fn the_noted_band_reads_the_forty_megahertz_table_at_its_own_ten_megahertz_minimum() {
        let mib = Mib {
            subcarrier_spacing_common: SubcarrierSpacing::Khz30,
            ..mib(5 << 4, 0)
        };
        let noted = monitor(&config(SubcarrierSpacing::Khz30, 10.0e6, 79), &mib).unwrap();
        assert_eq!(noted.table, "38.213 13-6");
        assert_eq!((noted.resource_blocks, noted.offset), (48, 28));
        let ordinary = monitor(&config(SubcarrierSpacing::Khz30, 10.0e6, 78), &mib).unwrap();
        assert_eq!(ordinary.table, "38.213 13-4");
        assert_eq!((ordinary.resource_blocks, ordinary.offset), (24, 0));
    }

    /// DANGER: the block's half-width is counted in the block's own subcarriers and the table's offset in
    /// the control region's resource blocks. Scaling both by one grid agrees with the specification at
    /// `{15,15}` and `{30,30}` and disagrees by 120 subcarriers — 1.8 MHz — at both mixed pairs, which is
    /// why this pins all four rather than the two the audit meets most often.
    #[test]
    fn each_spacing_pair_places_the_region_in_the_grid_that_measures_it() {
        // Chosen so that every pair resolves a row with offset 2, leaving the block's half-width as the
        // only term that differs between them.
        let placement = |ssb, common, index: u8, band| {
            let mib = Mib {
                subcarrier_spacing_common: common,
                ssb_subcarrier_offset: 3,
                ..mib(index << 4, 0)
            };
            let resolved = monitor(&config(ssb, 5.0e6, band), &mib).unwrap();
            (resolved.table, coreset(&mib, &resolved))
        };
        // {15, 15} Table 13-1 row 1: offset 2 at 15 kHz. 120 + 24 + 3.
        let (table, region) = placement(SubcarrierSpacing::Khz15, SubcarrierSpacing::Khz15, 1, 3);
        assert_eq!(table, "38.213 13-1");
        assert_eq!(region.below, 120 + 24 + 3);
        // {30, 30} Table 13-4 row 2: offset 2 at 30 kHz. 240 + 48 + 3.
        let (table, region) = placement(SubcarrierSpacing::Khz30, SubcarrierSpacing::Khz30, 2, 78);
        assert_eq!(table, "38.213 13-4");
        assert_eq!(region.below, 240 + 48 + 3);
        // {30, 15} Table 13-3 row 0: offset 2 at 15 kHz under a 30 kHz block. 240 + 24 + 3 — the pair
        // that a single grid would place 120 subcarriers too high.
        let (table, region) = placement(SubcarrierSpacing::Khz30, SubcarrierSpacing::Khz15, 0, 41);
        assert_eq!(table, "38.213 13-3");
        assert_eq!(region.below, 240 + 24 + 3);
        // {15, 30} Table 13-2 row 0: offset 5 at 30 kHz under a 15 kHz block. 120 + 120 + 3 — and 120
        // too low under a single grid.
        let (table, region) = placement(SubcarrierSpacing::Khz15, SubcarrierSpacing::Khz30, 0, 3);
        assert_eq!(table, "38.213 13-2");
        assert_eq!(region.below, 120 + 120 + 3);
        // The width is the control region's alone, so this row's 24 resource blocks span 8.64 MHz at
        // 30 kHz however wide the block that pointed at it.
        assert_eq!(region.width, 12 * 24 * 2);
    }

    /// A 3 MHz minimum channel bandwidth is the one case where two tables are standards-valid at once,
    /// and the bit that separates them — the carrier's width — arrives in the SIB1 being decoded. Both
    /// are offered so a DCI CRC can settle it, rather than the case being declared impossible.
    #[test]
    fn a_three_megahertz_minimum_offers_both_tables_its_caption_allows() {
        let mib = mib(2 << 4, 0);
        let candidates = monitors(&config(SubcarrierSpacing::Khz15, 3.0e6, 28), &mib);
        let tables: Vec<&str> = candidates.iter().map(|monitor| monitor.table).collect();
        assert_eq!(tables, ["38.213 13-1", "38.213 13-0"]);
        // Row 2 is 24 resource blocks at offset 4 in Table 13-1 and at offset 0 in Table 13-0, so the
        // two hypotheses genuinely place the region in different spectrum.
        assert_eq!(candidates[0].offset, 4);
        assert_eq!(candidates[1].offset, 0);
        // Anything wider has one interpretation, and a band with no published channel has none.
        assert_eq!(
            monitors(&config(SubcarrierSpacing::Khz15, 5.0e6, 3), &mib).len(),
            1
        );
        assert_eq!(
            monitors(&config(SubcarrierSpacing::Khz15, f64::INFINITY, 3), &mib).len(),
            0
        );
    }

    /// Table 13-0 indices 6 to 9 repeat the geometry of 2 to 5 exactly and differ only in using
    /// non-interleaved CCE-to-REG mapping, so a decoder that read the geometry alone would treat four
    /// rows as duplicates of four others and fail every CRC on them.
    #[test]
    fn the_rows_that_differ_only_in_their_control_mapping_are_not_read_as_duplicates() {
        let table = |index: u8| {
            monitors(
                &config(SubcarrierSpacing::Khz15, 3.0e6, 28),
                &mib(index << 4, 0),
            )
            .into_iter()
            .find(|monitor| monitor.table == "38.213 13-0")
            .unwrap()
        };
        for (interleaved, sequential) in [(2, 6), (3, 7), (4, 8), (5, 9)] {
            let interleaved = table(interleaved);
            let sequential = table(sequential);
            assert_eq!(
                (
                    interleaved.resource_blocks,
                    interleaved.symbols,
                    interleaved.offset
                ),
                (
                    sequential.resource_blocks,
                    sequential.symbols,
                    sequential.offset
                )
            );
            assert!(interleaved.interleaved);
            assert!(!sequential.interleaved);
        }
        // The narrowest region NR defines, which only this table reaches.
        assert_eq!(table(0).resource_blocks, 12);
        assert!(table(0).interleaved);
    }

    /// A band operated with shared spectrum channel access has to refuse on the channel access, because
    /// its minimum channel bandwidth selects a mapped table without complaint. n46 publishes 10 MHz and
    /// so read Table 13-1 for months while needing the unmapped 13-1A, which is a wrong decode and not a
    /// missing one.
    #[test]
    fn shared_spectrum_refuses_on_its_channel_access_and_not_on_its_width() {
        let mib = mib(0, 0);
        let licensed = config(SubcarrierSpacing::Khz30, 10.0e6, 46);
        assert_eq!(monitors(&licensed, &mib).len(), 1);
        let shared = Config {
            shared_spectrum: true,
            ..licensed
        };
        assert_eq!(monitors(&shared, &mib).len(), 0);
    }

    /// n110 publishes a 3 MHz channel at 15 kHz and no channel at all at 30 kHz, so it reads both of the
    /// tables a 3 MHz minimum admits at the one spacing and none at the other. The catalog carries that
    /// as `None`, which arrives here as an infinite minimum.
    #[test]
    fn the_three_megahertz_band_reads_two_tables_at_one_spacing_and_none_at_the_other() {
        let mib = mib(0, 0);
        let published = monitors(&config(SubcarrierSpacing::Khz15, 3.0e6, 110), &mib);
        let tables: Vec<&str> = published.iter().map(|monitor| monitor.table).collect();
        assert_eq!(tables, ["38.213 13-1", "38.213 13-0"]);
        let thirty = Mib {
            subcarrier_spacing_common: SubcarrierSpacing::Khz30,
            ..mib
        };
        assert_eq!(
            monitors(
                &config(SubcarrierSpacing::Khz30, f64::INFINITY, 110),
                &thirty
            )
            .len(),
            0
        );
    }

    /// A 3 MHz minimum selects the same table as a 5 MHz one, because Table 13-1's caption covers those
    /// bands at any carrier wider than 3 MHz and the carrier's width is not known until SIB1.
    #[test]
    fn a_three_megahertz_minimum_reads_the_five_megahertz_table() {
        assert_eq!(
            monitor(&config(SubcarrierSpacing::Khz15, 3.0e6, 28), &mib(0, 0)),
            monitor(&config(SubcarrierSpacing::Khz15, 5.0e6, 28), &mib(0, 0))
        );
    }
}
