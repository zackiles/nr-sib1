use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{Fft as FftPlan, FftPlanner};

use crate::types::SubcarrierSpacing;

/// Basic time unit of TS 38.211 section 4.1: Tc = 1 / (480 kHz * 4096).
const TC_HZ: u64 = 480_000 * 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Numerology {
    pub spacing: SubcarrierSpacing,
    pub sample_rate_hz: u64,
    pub size: usize,
    pub cp: usize,
    /// Cyclic prefix of the symbol that opens each half subframe. TS 38.211 section 5.3.1 gives it
    /// 16 * kappa extra units of Tc, which is a function of the sample rate rather than of the
    /// numerology, so it cannot be scaled from `cp`.
    pub cp_long: usize,
    pub symbols_per_subframe: usize,
    pub symbols_per_half_subframe: usize,
}

impl Numerology {
    pub fn new(spacing: SubcarrierSpacing, sample_rate_hz: f64) -> Result<Self, &'static str> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err("sample rate must be positive");
        }
        let rate = sample_rate_hz.round() as u64;
        if !rate.is_multiple_of(u64::from(spacing.hz())) {
            return Err("sample rate is not an integer multiple of the subcarrier spacing");
        }
        let size = (rate / u64::from(spacing.hz())) as usize;
        if !size.is_power_of_two() {
            return Err("sample rate does not give a power-of-two transform");
        }
        let mu = spacing.mu();
        let scale = |units: u64| -> Result<usize, &'static str> {
            let product = units * rate;
            if !product.is_multiple_of(TC_HZ) {
                return Err("sample rate does not give a whole-sample cyclic prefix");
            }
            Ok((product / TC_HZ) as usize)
        };
        let cp = scale(9216 >> mu)?;
        let extra = scale(1024)?;
        Ok(Self {
            spacing,
            sample_rate_hz: rate,
            size,
            cp,
            cp_long: cp + extra,
            symbols_per_subframe: 14 << mu,
            symbols_per_half_subframe: 7 << mu,
        })
    }

    /// Cyclic prefix of `symbol`, counted from the start of a subframe.
    #[must_use]
    pub fn prefix(&self, symbol: usize) -> usize {
        if symbol.is_multiple_of(self.symbols_per_half_subframe) {
            self.cp_long
        } else {
            self.cp
        }
    }

    #[must_use]
    pub fn subframe_samples(&self) -> usize {
        (0..self.symbols_per_subframe)
            .map(|symbol| self.size + self.prefix(symbol))
            .sum()
    }
}

pub struct Ofdm {
    numerology: Numerology,
    plan: Arc<dyn FftPlan<f32>>,
    buffer: Vec<Complex32>,
}

impl Ofdm {
    #[must_use]
    pub fn new(numerology: Numerology) -> Self {
        Self {
            plan: FftPlanner::<f32>::new().plan_fft_forward(numerology.size),
            buffer: vec![Complex32::default(); numerology.size],
            numerology,
        }
    }

    #[must_use]
    pub fn numerology(&self) -> Numerology {
        self.numerology
    }

    /// Demodulates one symbol whose cyclic prefix begins at `samples[0]`, returning subcarriers
    /// ordered from -size/2 to size/2 - 1 so that DC sits at the centre.
    pub fn demodulate(&mut self, samples: &[Complex32], prefix: usize) -> Option<Vec<Complex32>> {
        self.demodulate_at(samples, 0, prefix, 0.0)
    }

    /// Demodulates the symbol whose cyclic prefix begins at `samples[offset]`, removing `cfo_hz`
    /// with a phase ramp anchored at `samples[0]`.
    ///
    /// IMPORTANT: the anchor is why this takes an offset rather than a pre-sliced symbol. Restarting
    /// the ramp at each symbol leaves every symbol its own residual phase, proportional to the
    /// carrier offset and to how far the symbol sits from the anchor. Nothing that estimates a
    /// channel from one symbol and applies it to another survives that, which is every reference
    /// signal sparser than one per symbol.
    pub fn demodulate_at(
        &mut self,
        samples: &[Complex32],
        offset: usize,
        prefix: usize,
        cfo_hz: f32,
    ) -> Option<Vec<Complex32>> {
        let size = self.numerology.size;
        let body = samples.get(offset + prefix..offset + prefix + size)?;
        let rate = self.numerology.sample_rate_hz as f64;
        let step = -std::f64::consts::TAU * f64::from(cfo_hz) / rate;
        let anchor = (step * (offset + prefix) as f64).rem_euclid(std::f64::consts::TAU) as f32;
        let step = step as f32;
        for (index, (output, input)) in self.buffer.iter_mut().zip(body).enumerate() {
            *output = *input * Complex32::from_polar(1.0, anchor + step * index as f32);
        }
        self.plan.process(&mut self.buffer);
        let half = size / 2;
        let scale = 1.0 / size as f32;
        Some(
            (0..size)
                .map(|index| self.buffer[(index + half) % size] * scale)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use num_complex::Complex32;

    use super::{Numerology, Ofdm};
    use crate::types::SubcarrierSpacing;

    #[test]
    fn every_supported_numerology_fills_its_subframe_exactly() {
        for spacing in [SubcarrierSpacing::Khz15, SubcarrierSpacing::Khz30] {
            for rate in [7.68e6, 15.36e6, 30.72e6, 61.44e6] {
                let Ok(numerology) = Numerology::new(spacing, rate) else {
                    continue;
                };
                assert_eq!(
                    numerology.subframe_samples(),
                    numerology.sample_rate_hz as usize / 1000,
                    "{spacing:?} at {rate} must tile one millisecond without remainder"
                );
            }
        }
    }

    #[test]
    fn prefix_lengths_match_the_published_reference_case() {
        let thirty = Numerology::new(SubcarrierSpacing::Khz30, 30.72e6).unwrap();
        assert_eq!((thirty.size, thirty.cp, thirty.cp_long), (1024, 72, 88));
        let fifteen = Numerology::new(SubcarrierSpacing::Khz15, 30.72e6).unwrap();
        assert_eq!(
            (fifteen.size, fifteen.cp, fifteen.cp_long),
            (2048, 144, 160)
        );
    }

    #[test]
    fn unusable_sample_rates_are_refused_rather_than_rounded() {
        assert!(Numerology::new(SubcarrierSpacing::Khz30, 40e6).is_err());
        assert!(Numerology::new(SubcarrierSpacing::Khz30, 0.0).is_err());
    }

    #[test]
    fn a_single_subcarrier_survives_the_round_trip() {
        let numerology = Numerology::new(SubcarrierSpacing::Khz30, 30.72e6).unwrap();
        let mut ofdm = Ofdm::new(numerology);
        let size = numerology.size;
        let tone = 100_i64;
        let mut samples = vec![Complex32::default(); numerology.cp + size];
        for (index, slot) in samples.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * tone as f64 * (index as f64 - numerology.cp as f64)
                / size as f64;
            *slot = Complex32::new(phase.cos() as f32, phase.sin() as f32);
        }
        let grid = ofdm.demodulate(&samples, numerology.cp).unwrap();
        let peak = grid
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.norm().total_cmp(&right.1.norm()))
            .unwrap()
            .0;
        assert_eq!(peak as i64 - (size / 2) as i64, tone);
        assert!((grid[peak].norm() - 1.0).abs() < 1e-3);
    }
}
