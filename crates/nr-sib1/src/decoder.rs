use num_complex::Complex32;

use crate::{
    Combine, Config, Event, Failure, Numerology, Ofdm, Reason, Region, SsbCase, Stage, Sync,
    allocation, coreset, decode_dci, decode_mib, decode_pbch, decode_rrc, detect_dmrs, detect_pss,
    detect_sss, extract, monitors,
};

/// Blocks of one cell followed to the broadcast before the search leaves it for the others in the
/// window. Only blocks that reached the broadcast count, so the half of a cell's blocks that carry the
/// wrong frame parity never spend one.
///
/// Two is enough because a single one of these now walks every monitoring occasion left in the window
/// rather than only its own, so a second block is a second opinion on timing and carrier offset and not
/// a second slice of the window. Raising it multiplies work the first block already did.
const ATTEMPTS: usize = 2;

/// Twenty-millisecond monitoring occasions walked forward from one block. Bounded so that one cell in a
/// long dwell cannot spend the whole window's decode and hide the others; at four redundancy versions
/// cycling, reaching this many leaves a vanishing chance of never seeing RV 0.
const REPEATS: usize = 32;

/// Distinct reasons carried out of a walk. Every repeat that fails produces one, most of them identical,
/// and a failure an operator reads has to stay a sentence rather than becoming a log.
const REPORTED: usize = 4;

/// What the search has learned about one cell in this window.
struct Seen {
    pci: u16,
    sample: u64,
    attempts: usize,
    decoded: bool,
    broadcast: Vec<Reason>,
    block: Vec<Reason>,
}

/// Why one SS/PBCH block did not yield SIB1.
enum Miss {
    /// The block cannot carry SIB1 at all — a candidate position or a frame parity this cell does not
    /// broadcast in. Another block of the same cell can still work, so this spends no attempt, and it
    /// is only worth reporting when no block of the cell ever reached the broadcast.
    Block(Vec<Reason>),
    /// The broadcast was reached and did not decode.
    Broadcast(Vec<Reason>),
}

impl Miss {
    fn block(reason: Reason) -> Self {
        Self::Block(vec![reason])
    }

    fn broadcast(reason: Reason) -> Self {
        Self::Broadcast(vec![reason])
    }
}

#[must_use]
pub fn decode(samples: &[Complex32], config: &Config) -> Vec<Event> {
    let Ok(numerology) = Numerology::new(config.spacing, config.sample_rate_hz) else {
        return vec![Event::Failure(Failure {
            pci: None,
            stage: Stage::Sync,
            sample: 0,
            reasons: vec![Reason::Message(
                "sample rate is incompatible with the configured NR numerology".into(),
            )],
        })];
    };
    let centers = centers(config, numerology);
    let mut ofdm = Ofdm::new(numerology);
    let mut events = Vec::new();
    let mut seen: Vec<Seen> = Vec::new();
    let mut synced = None;
    for candidate in boundaries(samples, numerology) {
        // DANGER: screen each boundary on one transform before refining it. Every OFDM symbol in the
        // capture is a boundary, so a live window offers thousands, and synchronising all of them
        // costs more than the capture deadline allows: the whole decode is abandoned and reads as a
        // radio fault. A block that survives this is worth the rest.
        let Some(coarse) = ofdm.demodulate_at(samples, candidate, numerology.cp, 0.0) else {
            continue;
        };
        if !centers
            .iter()
            .filter_map(|center| detect_pss(&coarse, *center))
            .any(|pss| pss.score >= 0.2)
        {
            continue;
        }
        let Some(block) = (candidate.saturating_sub(2)..=candidate + 2)
            .filter_map(|start| synchronise(samples, start, numerology, &centers))
            .max_by(|left, right| {
                (left.pss.score * left.sss.score).total_cmp(&(right.pss.score * right.sss.score))
            })
        else {
            continue;
        };
        let Block {
            symbols,
            start,
            center,
            cfo_hz,
            pss,
            sss,
        } = block;
        if f64::from(pss.margin_db.min(sss.margin_db)) < config.minimum_quality_db {
            continue;
        }
        let grid = extract(&symbols, center).unwrap();
        let Some((reference, mib)) = read(&grid, sss.pci) else {
            synced = synced.or(Some((sss.pci, start as u64)));
            continue;
        };
        // A block repeats every twenty milliseconds, so a live window holds dozens of them, and a
        // window can hold several cells. Announce each cell once and spend the remaining blocks on
        // whichever broadcasts have not decoded yet — never stop at the first cell that does, or one
        // operator in a window hides every other.
        let index = if let Some(index) = seen.iter().position(|entry| entry.pci == sss.pci) {
            index
        } else {
            events.push(Event::Sync(Sync {
                pci: sss.pci,
                nid1: sss.nid1,
                nid2: pss.nid2,
                sample: start as u64,
                cfo_hz: f64::from(cfo_hz),
                ssb_hz: config.center_hz
                    + center as f64 * f64::from(config.spacing.hz())
                    + f64::from(cfo_hz),
                quality_db: f64::from(pss.margin_db.min(sss.margin_db)),
                ssb_index: Some(reference.index),
                half_frame: Some(mib.half_frame),
            }));
            events.push(Event::Mib(mib.clone()));
            seen.push(Seen {
                pci: sss.pci,
                sample: start as u64,
                attempts: 0,
                decoded: false,
                broadcast: Vec::new(),
                block: Vec::new(),
            });
            seen.len() - 1
        };
        if seen[index].decoded || seen[index].attempts >= ATTEMPTS {
            continue;
        }
        match system_information(samples, start, numerology, center, cfo_hz, config, &mib) {
            Ok(sib1) => {
                seen[index].decoded = true;
                events.push(Event::Sib1(sib1));
            }
            Err(Miss::Block(reasons)) => {
                if seen[index].block.is_empty() {
                    seen[index].block = reasons;
                }
            }
            Err(Miss::Broadcast(reasons)) => {
                seen[index].attempts += 1;
                // Keep the earliest reasons: blocks are tried in order and the earliest has the most
                // of the window ahead of it, so a later one reports running out of samples in
                // preference to whatever the cell actually did.
                if seen[index].broadcast.is_empty() {
                    seen[index].broadcast = reasons;
                }
            }
        }
    }
    events.extend(unread(&seen, synced));
    events
}

/// The MIB of the block in `grid`, with the demodulation reference its PBCH decoded against.
fn read(grid: &crate::Grid, pci: u16) -> Option<(crate::Dmrs, crate::Mib)> {
    let reference = detect_dmrs(grid, pci, 0..8)?;
    let codeword = decode_pbch(grid, pci, &reference, 4)?;
    let mib = decode_mib(&codeword, pci, reference.index)?;
    Some((reference, mib))
}

/// One failure per cell the window read and could not follow to SIB1, and one for the window itself
/// only when it held no cell at all. A sweep is read back through these, so a window that lost one
/// operator while naming another has to say so rather than reporting the window as a success.
fn unread(seen: &[Seen], synced: Option<(u16, u64)>) -> Vec<Event> {
    if seen.is_empty() {
        let (pci, stage, sample, reason) = match synced {
            Some((pci, sample)) => (
                Some(pci),
                Stage::Pbch,
                sample,
                "cell synchronisation succeeded but PBCH CRC did not validate",
            ),
            None => (
                None,
                Stage::Sync,
                0,
                "no PSS/SSS hypothesis passed correlation and quality checks",
            ),
        };
        return vec![Event::Failure(Failure {
            pci,
            stage,
            sample,
            reasons: vec![Reason::Message(reason.into())],
        })];
    }
    seen.iter()
        .filter(|entry| !entry.decoded)
        .map(|entry| {
            let reasons = if entry.broadcast.is_empty() {
                &entry.block
            } else {
                &entry.broadcast
            };
            Event::Failure(Failure {
                pci: Some(entry.pci),
                stage: Stage::Sib1,
                sample: entry.sample,
                reasons: if reasons.is_empty() {
                    vec![Reason::Message(
                        "no block of this cell carried a Type0-PDCCH occasion inside the window"
                            .into(),
                    )]
                } else {
                    reasons.clone()
                },
            })
        })
        .collect()
}

fn boundaries(samples: &[Complex32], numerology: Numerology) -> Vec<usize> {
    let width = numerology.size + numerology.cp;
    if samples.len() < width {
        return Vec::new();
    }
    let mut scores = Vec::with_capacity(samples.len() - width + 1);
    let mut correlation = Complex32::default();
    let mut energy = 0.0;
    for index in 0..numerology.cp {
        let prefix = samples[index];
        let tail = samples[numerology.size + index];
        correlation += prefix * tail.conj();
        energy += prefix.norm_sqr() + tail.norm_sqr();
    }
    for start in 0..=samples.len() - width {
        scores.push(2.0 * correlation.norm() / energy.max(f32::EPSILON));
        if start == samples.len() - width {
            break;
        }
        let old_prefix = samples[start];
        let old_tail = samples[start + numerology.size];
        let new_prefix = samples[start + numerology.cp];
        let new_tail = samples[start + numerology.size + numerology.cp];
        correlation += new_prefix * new_tail.conj() - old_prefix * old_tail.conj();
        energy += new_prefix.norm_sqr() + new_tail.norm_sqr()
            - old_prefix.norm_sqr()
            - old_tail.norm_sqr();
    }
    let mut found = Vec::new();
    for index in 1..scores.len().saturating_sub(1) {
        if scores[index] > 0.5
            && scores[index] >= scores[index - 1]
            && scores[index] > scores[index + 1]
        {
            if let Some(previous) = found.last_mut()
                && index - *previous < numerology.cp / 2
            {
                if scores[index] > scores[*previous] {
                    *previous = index;
                }
            } else {
                found.push(index);
            }
        }
    }
    found
}

/// An SS/PBCH block, with the timing and frequency the search resolved it at.
struct Block {
    symbols: Vec<Vec<Complex32>>,
    start: usize,
    center: isize,
    cfo_hz: f32,
    pss: crate::Pss,
    sss: crate::Sss,
}

fn synchronise(
    samples: &[Complex32],
    start: usize,
    numerology: Numerology,
    centers: &[isize],
) -> Option<Block> {
    let stride = numerology.size + numerology.cp;
    let correlation: Complex32 = (0..4)
        .flat_map(|symbol| (0..numerology.cp).map(move |index| symbol * stride + index))
        .map_while(|index| {
            let prefix = samples.get(start + index)?;
            let tail = samples.get(start + numerology.size + index)?;
            Some(prefix * tail.conj())
        })
        .sum();
    let mut cfo_hz = -correlation.arg() * numerology.sample_rate_hz as f32
        / (std::f32::consts::TAU * numerology.size as f32);
    let mut ofdm = Ofdm::new(numerology);
    let mut symbols = (0..4)
        .map(|index| ofdm.demodulate_at(samples, start + index * stride, numerology.cp, cfo_hz))
        .collect::<Option<Vec<_>>>()?;
    let (center, pss, sss) = centers
        .iter()
        .filter_map(|center| {
            let pss = detect_pss(&symbols[0], *center)?;
            if pss.score < 0.2 {
                return None;
            }
            let sss = detect_sss(&symbols[2], *center, pss.nid2)?;
            (sss.score >= 0.2).then_some((*center, pss, sss))
        })
        .max_by(|left, right| {
            (left.1.score * left.2.score).total_cmp(&(right.1.score * right.2.score))
        })?;
    // Refine against the block's own reference signals, which sit two symbols apart and so resolve a
    // residual offset far finer than a cyclic prefix can. Anything left over turns into a phase
    // slope across symbols, and the sparse references of a PDSCH cannot tell that from the channel.
    if let Some(grid) = extract(&symbols, center)
        && let Some(reference) = detect_dmrs(&grid, sss.pci, 0..8)
        && reference.channel[1].norm_sqr() > f32::EPSILON
        && reference.channel[3].norm_sqr() > f32::EPSILON
    {
        let drift = (reference.channel[3] * reference.channel[1].conj()).arg();
        cfo_hz += drift * numerology.sample_rate_hz as f32
            / (std::f32::consts::TAU * 2.0 * stride as f32);
        symbols = (0..4)
            .map(|index| ofdm.demodulate_at(samples, start + index * stride, numerology.cp, cfo_hz))
            .collect::<Option<Vec<_>>>()?;
    }
    Some(Block {
        symbols,
        start,
        center,
        cfo_hz,
        pss,
        sss,
    })
}

fn centers(config: &Config, numerology: Numerology) -> Vec<isize> {
    let half = config.sample_rate_hz / 2.0 - 2.0e6;
    let low = config.center_hz - half;
    let high = config.center_hz + half;
    let mut frequencies = vec![config.center_hz];
    if config.center_hz < 3.0e9 {
        for m in [1.0, 3.0, 5.0] {
            let first = ((low - m * 50e3) / 1.2e6).ceil().max(0.0) as u32;
            let last = ((high - m * 50e3) / 1.2e6).floor().max(0.0) as u32;
            frequencies.extend((first..=last).map(|n| f64::from(n) * 1.2e6 + m * 50e3));
        }
    } else {
        let first = ((low - 3.0e9) / 1.44e6).ceil().max(0.0) as u32;
        let last = ((high - 3.0e9) / 1.44e6).floor().max(0.0) as u32;
        frequencies.extend((first..=last).map(|n| 3.0e9 + f64::from(n) * 1.44e6));
    }
    let spacing = f64::from(config.spacing.hz());
    let limit = isize::try_from(numerology.size / 2 - 120).unwrap();
    let mut offsets = frequencies
        .into_iter()
        .map(|frequency| ((frequency - config.center_hz) / spacing).round() as isize)
        .filter(|offset| offset.abs() <= limit)
        .collect::<Vec<_>>();
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// Symbol of the frame that opens the SS/PBCH block of `mib`, per the FR1 candidate positions of
/// TS 38.213 section 4.1.
fn ssb_symbol(mib: &crate::Mib, numerology: Numerology, ssb_case: SsbCase) -> Option<usize> {
    let candidates: &[usize] = match ssb_case {
        SsbCase::A => &[2, 8, 16, 22],
        SsbCase::B => &[4, 8, 16, 20, 32, 36, 44, 48],
        SsbCase::C => &[2, 8, 16, 22, 30, 36, 44, 50],
    };
    let symbol = *candidates.get(usize::from(mib.ssb_index))?;
    Some(symbol + usize::from(mib.half_frame) * numerology.symbols_per_subframe * 5)
}

/// Walks from the SS/PBCH block to the Type0-PDCCH monitoring occasions the MIB selected, and
/// decodes SIB1 out of the first one that validates both CRCs.
///
/// DANGER: the two occasions are reached by accumulating symbol lengths from the block rather than
/// by scanning for them. Scanning finds a candidate the cell never transmitted roughly as often as
/// it finds the real one, because a 39-bit payload behind a 24-bit CRC is not a strong enough filter
/// to be handed hundreds of positions. The walk is one piece for that reason — every step is measured
/// from the last, and a step that resolved its own position would be the scan this avoids.
fn system_information(
    samples: &[Complex32],
    ssb: usize,
    numerology: Numerology,
    center: isize,
    cfo_hz: f32,
    config: &Config,
    mib: &crate::Mib,
) -> Result<crate::Sib1, Miss> {
    let candidates = monitors(config, mib);
    if candidates.is_empty() {
        // `monitors` is the refusal and this is only its report, but the two empty answers it can give
        // are opposite facts. A shared-spectrum band reads here as a MIB index nothing maps, which
        // sends an operator looking at the cell — and the cell is fine, it is the receiver that has no
        // tables for the class.
        if config.shared_spectrum {
            return Err(Miss::broadcast(Reason::SharedSpectrum));
        }
        return Err(Miss::broadcast(Reason::Message(format!(
            "pdcch-ConfigSIB1 {} is not a CORESET#0 this decoder maps at {} kHz over a {} MHz minimum channel",
            mib.pdcch_config_sib1,
            config.spacing.hz() / 1000,
            config.minimum_channel_bandwidth_hz / 1e6
        ))));
    }
    // More than one table can be standards-valid before SIB1, and only a DCI CRC tells them apart, so
    // each is tried in turn. The reported failure is the one that got furthest rather than the last:
    // a candidate refused for its geometry says nothing about the cell that a candidate reaching the
    // broadcast has not already said better.
    let mut furthest: Option<Miss> = None;
    for monitor in &candidates {
        match broadcast(
            samples, ssb, numerology, center, cfo_hz, config, mib, monitor,
        ) {
            Ok(sib1) => return Ok(sib1),
            Err(miss) => {
                if matches!(miss, Miss::Broadcast(_)) || furthest.is_none() {
                    furthest = Some(miss);
                }
            }
        }
    }
    Err(furthest.unwrap())
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn broadcast(
    samples: &[Complex32],
    ssb: usize,
    numerology: Numerology,
    center: isize,
    cfo_hz: f32,
    config: &Config,
    mib: &crate::Mib,
    monitor: &crate::Monitor,
) -> Result<crate::Sib1, Miss> {
    // The control region is demodulated out of the same transform the block was, so a common spacing
    // that differs from the block's is a grid this capture cannot address at all. Saying so beats
    // indexing the wrong subcarriers and reporting the CRC failure that follows.
    if monitor.spacing != config.spacing {
        return Err(Miss::broadcast(Reason::CoresetSpacing {
            common_hz: monitor.spacing.hz(),
            captured_hz: config.spacing.hz(),
        }));
    }
    let opening = ssb_symbol(mib, numerology, config.ssb_case).ok_or_else(|| {
        Miss::block(Reason::Message(format!(
            "SS/PBCH block index {} is not a candidate position of case {:?}",
            mib.ssb_index, config.ssb_case
        )))
    })?;
    // DANGER: k_SSB counts 15 kHz subcarriers everywhere in FR1, whatever spacing the block itself uses
    // (TS 38.211 section 7.4.1.4), so on a 30 kHz grid it reaches half as far. `coreset` is the one
    // place that arithmetic lives, and it is shared with the capture planner so the two cannot drift:
    // subtracting k_SSB as grid subcarriers once put CORESET#0 nearly two resource blocks below where
    // every 30 kHz cell transmitted it, and each of them failed its DCI CRC while every 15 kHz cell
    // decoded.
    let region = coreset(mib, monitor);
    let grid = isize::try_from(config.spacing.hz() / 15_000).unwrap();
    let first = isize::try_from(numerology.size / 2).unwrap() + center
        - isize::try_from(region.below).unwrap() / grid;
    let width = isize::try_from(region.width).unwrap() / grid;
    // DANGER: bound both edges, and bound them against what the analog filter passed rather than
    // against the transform. Only the lower edge was ever checked, so a region running off the top fell
    // out of a slice returning nothing and was reported as a DCI whose CRC did not validate — a cell
    // absent from the samples, described as a cell that was there and unreadable. A 48-resource-block
    // region at 30 kHz is 17.28 MHz wide and straddles its block almost symmetrically, so a tile grid
    // that puts the block near an edge cannot hold it at any transform size.
    let edge = (config.usable_hz / 2.0 / f64::from(config.spacing.hz())).floor() as isize;
    let usable = isize::try_from(numerology.size / 2).unwrap() - edge
        ..isize::try_from(numerology.size / 2).unwrap() + edge;
    if first < usable.start || first + width > usable.end {
        let span = region.span(config.center_hz + center as f64 * f64::from(config.spacing.hz()));
        let reach = (span.start - config.center_hz)
            .abs()
            .max((span.end - config.center_hz).abs());
        return Err(Miss::broadcast(Reason::CoresetOutsideCapture {
            required_hz: 2.0 * reach,
            available_hz: config.usable_hz,
        }));
    }
    let bwp = usize::try_from(first).unwrap();
    if mib.system_frame % 2 != u16::from(monitor.frame) {
        return Err(Miss::block(Reason::Message(format!(
            "SIB1 is broadcast in frames of parity {} and this block opened frame {}",
            monitor.frame, mib.system_frame
        ))));
    }
    // Walking symbol lengths from the block is what keeps this honest, so the walk has to stay
    // forward-only: a slot behind the block belongs to the frame before it.
    let walk = |symbol: usize| {
        (opening <= symbol).then(|| {
            ssb + (opening..symbol)
                .map(|symbol| numerology.prefix(symbol) + numerology.size)
                .sum::<usize>()
        })
    };
    let occasion = |slot: u16, advance: usize| {
        let control = advance + 14 * usize::from(slot) + usize::from(monitor.first_symbol);
        let mut offset = walk(control).ok_or_else(|| {
            Miss::block(Reason::Message(format!(
                "the occasion in slot {slot} precedes this block"
            )))
        })?;
        let mut ofdm = Ofdm::new(numerology);
        let symbols = demodulate(
            &mut ofdm,
            samples,
            &mut offset,
            control,
            usize::from(monitor.symbols),
            cfo_hz,
        )
        .ok_or_else(|| {
            Miss::broadcast(Reason::Message(format!(
                "the occasion in slot {slot} runs past the captured window"
            )))
        })?;
        let dci = decode_dci(&symbols, monitor, mib.pci, bwp, slot, monitor.first_symbol)
            .ok_or_else(|| {
                Miss::broadcast(Reason::Message(format!(
                    "no SI-RNTI DCI in slot {slot} validated its CRC"
                )))
            })?;
        let allocation = allocation(&dci, mib.dmrs_type_a_position).map_err(Miss::broadcast)?;
        let slot = slot + u16::from(allocation.slots);
        let data = advance + 14 * usize::from(slot) + usize::from(allocation.first_symbol);
        let mut offset = walk(data)
            .ok_or_else(|| Miss::block(Reason::Message("the PDSCH precedes this block".into())))?;
        let symbols = demodulate(
            &mut ofdm,
            samples,
            &mut offset,
            data,
            usize::from(allocation.symbols),
            cfo_hz,
        )
        .ok_or_else(|| {
            Miss::broadcast(Reason::Message(format!(
                "SI-RNTI DCI decoded in slot {slot} but its PDSCH runs past the captured window"
            )))
        })?;
        let part = Region {
            start: bwp,
            resource_blocks: usize::from(monitor.resource_blocks),
        };
        let scheduling = || {
            // The scheduling the DCI asked for belongs with the reason, because every remaining
            // question about a validated DCI whose block did not decode is a question about it.
            Reason::Message(format!(
                "SI-RNTI DCI decoded in slot {slot}: RB {}+{}, MCS {}, RV {}, {} mapping",
                dci.first_resource_block,
                dci.resource_blocks,
                dci.mcs,
                dci.redundancy,
                if dci.interleaved {
                    "interleaved"
                } else {
                    "direct"
                }
            ))
        };
        let candidates = crate::pdsch::soft(
            &symbols,
            mib,
            &dci,
            numerology,
            config.center_hz,
            part,
            slot,
        )
        .map_err(|reason| Miss::Broadcast(vec![scheduling(), reason]))?;
        Ok((candidates, scheduling()))
    };
    // The occasion recurs every twenty milliseconds, which is what the frame parity above expresses,
    // and walking those repeats rather than only the block's own occasion is the difference between
    // reaching a cell and reading it: SIB1 repeats with its redundancy version cycling, RV 1 and RV 2
    // puncture too many systematic bits to decode without the RV 0 they repeat, and a live n5 cell
    // offered RV 2, RV 1 and RV 1 on the three occasions a single-occasion search happened to reach.
    //
    // DANGER: the sample offset advances by whole frames and the slot number does not. Every
    // demodulation reference sequence in the broadcast is initialised from the slot within its own
    // frame, so a repeat two frames later is still the same slot, and advancing the slot number to
    // match the offset would scramble every sequence against a slot the cell never used.
    let period = 20 * numerology.symbols_per_subframe;
    let repeats = samples
        .len()
        .saturating_sub(ssb)
        .checked_div(20 * numerology.subframe_samples())
        .unwrap_or(0)
        .min(REPEATS);
    let mut reasons: Vec<Reason> = Vec::new();
    let mut reached = false;
    let mut combined: Vec<Option<Combine>> = Vec::new();
    let mut keep = |collected: Vec<Reason>| {
        for reason in collected {
            let sentence = reason.to_string();
            if reasons.len() < REPORTED && !reasons.iter().any(|kept| kept.to_string() == sentence)
            {
                reasons.push(reason);
            }
        }
    };
    for repeat in 0..=repeats {
        for step in 0..2 {
            let (candidates, scheduling) = match occasion(monitor.slot + step, repeat * period) {
                Ok(found) => found,
                Err(Miss::Block(collected)) => {
                    keep(collected);
                    continue;
                }
                Err(Miss::Broadcast(collected)) => {
                    reached = true;
                    keep(collected);
                    continue;
                }
            };
            reached = true;
            // Grow only. The reading at an index is what its accumulator holds, so shrinking to a
            // shorter list and growing again on the next occasion would discard everything the longer
            // one had gathered — and the count moves with the noise floor of the phase fit.
            if combined.len() < candidates.len() {
                combined.resize_with(candidates.len(), || None);
            }
            for (soft, held) in candidates.iter().zip(&mut combined) {
                // Occasions of one SI window carry the same transport block, so their soft bits belong
                // in one circular buffer rather than in one decode each: the redundancy versions are
                // positions in that buffer and RV 1 and RV 2 do not reach the systematic bits alone.
                //
                // One accumulator per carrier-phase candidate. The candidates are competing readings of
                // the same air, differing by up to half a turn at the data symbols, so summing across
                // them cancels every one of them.
                if !held
                    .as_ref()
                    .is_some_and(|kept| kept.holds(soft.size, soft.rate))
                {
                    *held = match Combine::new(soft.size, soft.rate) {
                        Ok(fresh) => Some(fresh),
                        Err(reason) => {
                            keep(vec![scheduling.clone(), reason]);
                            continue;
                        }
                    };
                }
                let buffer = held.as_mut().expect("just seated");
                let read = buffer
                    .add(&soft.llr, soft.order, soft.redundancy)
                    .and_then(|()| buffer.read());
                match read {
                    Ok(transport) => {
                        if let Some(sib1) = decode_rrc(&transport, mib.pci, mib.cell_barred) {
                            return Ok(sib1);
                        }
                        keep(vec![Reason::Message(
                            "the DL-SCH block validated but did not parse as SIB1".into(),
                        )]);
                    }
                    Err(reason) => keep(vec![
                        scheduling.clone(),
                        if buffer.occasions() > 1 {
                            Reason::Message(format!(
                                "the DL-SCH did not decode over {} combined occasions",
                                buffer.occasions()
                            ))
                        } else {
                            reason
                        },
                    ]),
                }
            }
        }
    }
    Err(if reached {
        Miss::Broadcast(reasons)
    } else {
        Miss::Block(reasons)
    })
}

fn demodulate(
    ofdm: &mut Ofdm,
    samples: &[Complex32],
    offset: &mut usize,
    symbol: usize,
    count: usize,
    cfo_hz: f32,
) -> Option<Vec<Vec<Complex32>>> {
    (0..count)
        .map(|index| {
            let numerology = ofdm.numerology();
            let prefix = numerology.prefix(symbol + index);
            let output = ofdm.demodulate_at(samples, *offset, prefix, cfo_hz)?;
            *offset += prefix + numerology.size;
            Some(output)
        })
        .collect()
}
