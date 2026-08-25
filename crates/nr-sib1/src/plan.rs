use crate::{Config, Guard, Mib, Numerology, coreset, monitor};

/// One capture rate this platform will plan for, with the transform it gives at 30 kHz.
///
/// IMPORTANT: this ladder is our limitation and not NR's. `Numerology::new` refuses a transform that
/// is not a power of two, which is our check rather than a DSP or 3GPP constraint — `rustfft` is
/// mixed-radix, and 46.08 `MSps` gives a valid 1536-point transform with whole-sample cyclic prefixes.
/// Lifting that check is deliberately not part of this: 46.08 `MSps` is exactly degenerate for the cell
/// that motivated the planner, because its 17.28 MHz usable half-width equals the width of the control
/// region and leaves a feasible set of zero width.
#[derive(Clone, Copy, Debug)]
pub struct Rate {
    pub sample_rate_hz: f64,
    pub transform: usize,
}

pub const RATES: [Rate; 2] = [
    Rate {
        sample_rate_hz: 30_720_000.0,
        transform: 1024,
    },
    Rate {
        sample_rate_hz: 61_440_000.0,
        transform: 2048,
    },
];

/// Fraction of a window the analog filter passes. Outside it the samples are the receiver's own
/// rolloff, so planning against the whole rate plans against spectrum that was never captured.
const USABLE: f64 = 0.75;

/// `Complex32` the cellular decode will hold for one window. It buffers the whole window before decoding
/// it, so one second at 61.44 `MSps` is 491 MB and the dwell has to be bounded by memory rather than by
/// how many monitoring occasions would be nice to have.
const BUDGET: usize = 512 << 20;

/// A capture that can carry the control region of a cell already read to MIB.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub center_hz: f64,
    pub sample_rate_hz: f64,
    pub transform: usize,
    pub usable_hz: f64,
    pub duration_seconds: f64,
    /// How far inside the usable passband the nearer edge of CORESET#0 sits.
    pub coreset_hz: f64,
    /// The same for the SS/PBCH block.
    pub block_hz: f64,
    /// How far the tuner's own frequency sits outside CORESET#0.
    pub dc_hz: f64,
}

impl Plan {
    /// The narrowest of the clearances, which is what the plan actually rests on.
    #[must_use]
    pub fn margin_hz(&self) -> f64 {
        self.coreset_hz.min(self.block_hz).min(self.dc_hz)
    }
}

/// Solves for a tuner frequency and sample rate that hold the whole Type0-PDCCH control region of a
/// decoded cell, given where its SS/PBCH block was found.
///
/// The constraints are all of: CORESET#0 inside the usable passband, the SS/PBCH block inside it too,
/// and the tuner's own frequency outside CORESET#0 by `guard.dc_hz` — each with `guard.margin_hz` to
/// spare. Retuning toward a block satisfies the first two and violates the third, which is why doing
/// it by eye produced cells that synchronised at full quality and yielded no DCI at all.
///
/// The lowest rate that leaves a feasible set is taken, and the centre is the midpoint of that set so
/// that the plan spends its slack on both constraints rather than sitting against one of them.
#[must_use]
pub fn plan(config: &Config, mib: &Mib, ssb_hz: f64, seconds: f64, guard: Guard) -> Option<Plan> {
    let monitor = monitor(config, mib)?;
    let region = coreset(mib, &monitor).span(ssb_hz);
    // The block is 240 subcarriers of its own spacing, and it has to survive the retune as well: a
    // centre that holds the control region and loses PSS/SSS reaches nothing to decode.
    let block = 120.0 * f64::from(config.spacing.hz());
    for rate in RATES {
        if Numerology::new(mib.subcarrier_spacing_common, rate.sample_rate_hz).is_err() {
            continue;
        }
        let usable = rate.sample_rate_hz * USABLE;
        let half = usable / 2.0 - guard.margin_hz;
        let low = (region.end - half).max(ssb_hz + block - half);
        let high = (region.start + half).min(ssb_hz - block + half);
        // Two sides clear the oscillator, and they are symmetric about the region whenever it straddles
        // its own block. Taking the wider keeps the choice determinate rather than incidental.
        let below = (low, high.min(region.start - guard.dc_hz));
        let above = (low.max(region.end + guard.dc_hz), high);
        let (from, to) = if below.1 - below.0 >= above.1 - above.0 {
            below
        } else {
            above
        };
        if to < from {
            continue;
        }
        let center_hz = f64::midpoint(from, to);
        return Some(Plan {
            center_hz,
            sample_rate_hz: rate.sample_rate_hz,
            transform: rate.transform,
            usable_hz: usable,
            duration_seconds: seconds.min(
                BUDGET as f64 / (size_of::<num_complex::Complex32>() as f64 * rate.sample_rate_hz),
            ),
            coreset_hz: (usable / 2.0
                - (region.start - center_hz)
                    .abs()
                    .max((region.end - center_hz).abs()))
            .max(0.0),
            block_hz: (usable / 2.0 - (ssb_hz - center_hz).abs() - block).max(0.0),
            dc_hz: (region.start - center_hz).max(center_hz - region.end),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{RATES, plan};
    use crate::{
        Config, Duplex, Guard, Mib, Numerology, Release, SsbCase, SubcarrierSpacing, coreset,
        monitor,
    };

    fn config(sample_rate_hz: f64, center_hz: f64) -> Config {
        Config {
            release: Release::R18,
            band: 78,
            duplex: Duplex::Tdd,
            sample_rate_hz,
            center_hz,
            usable_hz: sample_rate_hz * 0.75,
            minimum_channel_bandwidth_hz: 10e6,
            spacing: SubcarrierSpacing::Khz30,
            ssb_case: SsbCase::C,
            gscn: None,
            shared_spectrum: false,
            ntn: false,
            minimum_quality_db: 6.0,
            guard: Guard::default(),
        }
    }

    /// PCI 577 as the audit read it: 30 kHz throughout, `controlResourceSetZero` 11, `searchSpaceZero`
    /// 5, SS/PBCH index 3.
    fn cell() -> Mib {
        Mib {
            pci: 577,
            system_frame: 736,
            half_frame: false,
            subcarrier_spacing_common: SubcarrierSpacing::Khz30,
            ssb_subcarrier_offset: 0,
            dmrs_type_a_position: 2,
            pdcch_config_sib1: 0xb5,
            cell_barred: false,
            intra_frequency_reselection: true,
            ssb_index: 3,
        }
    }

    #[test]
    fn the_ladder_agrees_with_the_transforms_it_claims() {
        for rate in RATES {
            let numerology =
                Numerology::new(SubcarrierSpacing::Khz30, rate.sample_rate_hz).unwrap();
            assert_eq!(numerology.size, rate.transform);
        }
    }

    /// The tile grid put this cell's block 9 to 10.6 MHz from the tuner, where a 17.28 MHz control
    /// region cannot fit a 23.04 MHz passband at any centre. The planner has to say so rather than
    /// offering a window the decoder will reject.
    #[test]
    fn a_wide_control_region_is_refused_the_rate_that_cannot_hold_it() {
        let narrow = plan(
            &config(30.72e6, 3_470_507_000.0),
            &cell(),
            3_479_520_000.0,
            2.0,
            Guard::default(),
        )
        .unwrap();
        assert!((narrow.sample_rate_hz - 61.44e6).abs() < f64::EPSILON);
    }

    /// Every constraint holds on the plan, and the one that is easiest to satisfy by accident — the
    /// oscillator clear of the control region — is the one retuning by eye always broke.
    #[test]
    fn the_plan_holds_every_constraint_it_was_given() {
        let guard = Guard::default();
        let mib = cell();
        let ssb_hz = 3_479_520_000.0;
        let plan = plan(&config(30.72e6, 3_470_507_000.0), &mib, ssb_hz, 2.0, guard).unwrap();
        let region = coreset(
            &mib,
            &monitor(&config(30.72e6, 3_470_507_000.0), &mib).unwrap(),
        )
        .span(ssb_hz);
        let edge = plan.usable_hz / 2.0;
        assert!(region.start >= plan.center_hz - edge);
        assert!(region.end <= plan.center_hz + edge);
        assert!(ssb_hz - 3.6e6 >= plan.center_hz - edge);
        assert!(ssb_hz + 3.6e6 <= plan.center_hz + edge);
        assert!(plan.center_hz <= region.start - guard.dc_hz);
        assert!(plan.margin_hz() >= 0.0);
        // The midpoint of the feasible set, which for this cell is 8.6 to 14.4 MHz below the block.
        assert!((ssb_hz - plan.center_hz - 11.52e6).abs() < 1e3);
    }

    /// One second at 61.44 `MSps` is 491 MB of `Complex32`, and the decode holds the whole window.
    #[test]
    fn the_dwell_is_bounded_by_what_the_decode_will_hold() {
        let plan = plan(
            &config(30.72e6, 3_470_507_000.0),
            &cell(),
            3_479_520_000.0,
            60.0,
            Guard::default(),
        )
        .unwrap();
        assert!(plan.duration_seconds < 1.2);
        assert!(plan.duration_seconds * plan.sample_rate_hz * 8.0 <= (512 << 20) as f64);
    }
}
