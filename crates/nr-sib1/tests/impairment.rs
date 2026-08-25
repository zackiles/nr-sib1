//! Bounds what the broadcast decode tolerates, one impairment at a time, on a retained capture.
//!
//! A regression test that asserts "this must always reach CRC-valid SIB1" is a flaky test wearing a
//! strong one's clothes. Each axis here is therefore pinned at two points: the worst impairment that
//! still reads the cell, and the next step that does not. The pair is the envelope, and a change that
//! moves either end has changed the decoder's reach whether or not it meant to.
//!
//! Every impairment is applied to real samples rather than synthesised ones. A generated cell would let
//! the envelope be widened by generating a friendlier cell, which is the failure mode of synthetic
//! corpora; a real capture cannot be argued with.
//!
//! The two surprises are recorded on the tests that found them: an oscillator's leakage defeats
//! acquisition long before it defeats the control channel, and a quadrature shear that leaves the real
//! axis alone is almost free.

use std::path::PathBuf;

use nr_sib1::{Config, Duplex, Event, Release, SsbCase, SubcarrierSpacing, decode};
use num_complex::Complex32;

fn capture() -> Vec<Complex32> {
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/n3-sib1/capture.sigmf-data"),
    )
    .unwrap();
    bytes.as_chunks::<8>().0[830_000..860_000]
        .iter()
        .map(|sample| {
            Complex32::new(
                i32::from_le_bytes(sample[..4].try_into().unwrap()) as f32,
                i32::from_le_bytes(sample[4..].try_into().unwrap()) as f32,
            )
        })
        .collect()
}

/// Furthest stage the capture reaches once `impair` has been applied to it.
fn read(impair: impl Fn(&[Complex32]) -> Vec<Complex32>) -> &'static str {
    let events = decode(
        &impair(&capture()),
        &Config {
            release: Release::R18,
            band: 3,
            duplex: Duplex::Fdd,
            sample_rate_hz: 7.68e6,
            center_hz: 1_876_954_000.0,
            usable_hz: 5.76e6,
            minimum_channel_bandwidth_hz: 5e6,
            spacing: SubcarrierSpacing::Khz15,
            ssb_case: SsbCase::A,
            gscn: None,
            shared_spectrum: false,
            ntn: false,
            minimum_quality_db: 10.0,
            guard: nr_sib1::Guard::default(),
        },
    );
    if events.iter().any(|event| matches!(event, Event::Sib1(_))) {
        return "sib1";
    }
    if events.iter().any(|event| matches!(event, Event::Mib(_))) {
        return "mib";
    }
    if events.iter().any(|event| matches!(event, Event::Sync(_))) {
        return "sync";
    }
    "nothing"
}

fn power(samples: &[Complex32]) -> f32 {
    samples.iter().map(Complex32::norm_sqr).sum::<f32>() / samples.len() as f32
}

#[test]
fn the_capture_reads_before_anything_is_done_to_it() {
    assert_eq!(read(<[Complex32]>::to_vec), "sib1");
}

/// Uniform noise, not Gaussian: a deterministic sequence matters more to a boundary test than the shape
/// of its distribution, and a seeded generator here beats a dependency.
///
/// The measured floor is a wideband ratio over the whole 7.68 `MSps` window while the carrier occupies
/// 5 MHz of it, so the cell sees about 1.9 dB more than these numbers say.
#[test]
fn the_broadcast_reads_nine_decibels_below_the_noise_and_not_twelve() {
    let noisy = |snr_db: f32| {
        move |samples: &[Complex32]| {
            let mut state = 1_u64;
            let amplitude = (1.5 * power(samples) * 10f32.powf(-snr_db / 10.0)).sqrt();
            let mut uniform = move || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                amplitude * (((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0)
            };
            samples
                .iter()
                .map(|sample| sample + Complex32::new(uniform(), uniform()))
                .collect()
        }
    };
    assert_eq!(read(noisy(-9.0)), "sib1");
    assert_eq!(read(noisy(-12.0)), "nothing");
}

/// The limit is half a subcarrier, which is where an estimator that resolves the offset within one
/// subcarrier stops being able to tell which subcarrier it is in. Nothing about the decoder needs to be
/// generous here — the preflight tunes to a commanded frequency and the bladeRF's own error is orders
/// below this — but a change that narrows it below 6 kHz has broken the fractional estimator.
#[test]
fn a_carrier_offset_is_followed_to_half_a_subcarrier_and_no_further() {
    let offset = |hz: f64| {
        move |samples: &[Complex32]| {
            let step = 2.0 * std::f64::consts::PI * hz / 7.68e6;
            samples
                .iter()
                .enumerate()
                .map(|(sample, value)| {
                    value * Complex32::from_polar(1.0, (step * sample as f64) as f32)
                })
                .collect()
        }
    };
    assert_eq!(read(offset(6_000.0)), "sib1");
    assert_eq!(read(offset(7_500.0)), "nothing");
}

/// Gain imbalance between the two branches, which is one half of the quadrature error a direct
/// conversion receiver leaves behind.
///
/// 15 dB of imbalance is far past anything a working chain produces, and the point of pinning it there
/// is that the axis is nearly free: an image at that level is still 15 dB down on the wanted signal, and
/// the equalizer absorbs it. What fails at 25 dB is acquisition, once one branch has almost nothing left.
#[test]
fn fifteen_decibels_of_gain_imbalance_is_absorbed_and_twenty_five_is_not() {
    let imbalance = |ratio_db: f32| {
        move |samples: &[Complex32]| {
            // Split the ratio symmetrically so the total power is unchanged.
            let half = 10f32.powf(ratio_db / 40.0);
            samples
                .iter()
                .map(|sample| Complex32::new(sample.re * half, sample.im / half))
                .collect()
        }
    };
    assert_eq!(read(imbalance(15.0)), "sib1");
    assert_eq!(read(imbalance(25.0)), "nothing");
}

/// A quadrature shear leaves the real axis alone, and this capture survives 60 degrees of it — which is
/// not a claim about the decoder so much as about the impairment. Do not read this axis as headroom: it
/// is here so that a change which breaks it is noticed, and a real chain's quadrature error arrives with
/// gain imbalance attached, which the test above bounds far more tightly.
#[test]
fn a_quadrature_shear_alone_is_nearly_free() {
    let shear = |degrees: f32| {
        move |samples: &[Complex32]| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            samples
                .iter()
                .map(|sample| Complex32::new(sample.re, sample.re * sin + sample.im * cos))
                .collect()
        }
    };
    assert_eq!(read(shear(60.0)), "sib1");
}

/// Hard limiting to a fraction of the capture's own level, which is what an overloaded front end does.
///
/// QPSK keeps its phase through a limiter, so the broadcast survives clipping that leaves almost no
/// amplitude information at all. This is why the gain preflight is bounded on peak headroom rather than
/// on whether the broadcast still decodes: by the time clipping shows up here the fingerprint features
/// the platform exists to measure are long gone.
#[test]
fn clipping_costs_amplitude_and_the_broadcast_does_not_need_it() {
    let clip = |fraction: f32| {
        move |samples: &[Complex32]| {
            let limit = fraction * power(samples).sqrt();
            samples
                .iter()
                .map(|sample| {
                    let level = sample.norm();
                    if level > limit {
                        sample * (limit / level)
                    } else {
                        *sample
                    }
                })
                .collect()
        }
    };
    assert_eq!(read(clip(0.01)), "sib1");
}

/// The finding this test exists to pin, and the reason the DC guard stays for now.
///
/// In this capture the block sits 3 kHz below the tuner, `coresetZero` 0 gives 24 resource blocks at
/// offset 0, and the block is 20 resource blocks wide — so DC lands near the middle of the control
/// region rather than at its edge, which is the case the guard exists to avoid. The broadcast still reads
/// with leakage 9 dB *stronger* than the wanted signal, and at 12 dB the capture stops acquiring at all.
///
/// What fails first under DC is therefore not the control channel's soft bits but synchronisation,
/// because the correlator sees the spike as signal. Erasing impaired resource elements, DMRS-aware or
/// not, cannot repair that, so it is not the change that removes the guard, and building it would have
/// bought nothing.
///
/// TODO: the change that removes the guard is estimating the offset and subtracting it before the
/// transform, which repairs acquisition and the control region together. Validate that on a capture
/// whose leakage was measured rather than injected — this one had none of its own, so it says what the
/// decoder tolerates and not what the bladeRF produces.
#[test]
fn oscillator_leakage_defeats_acquisition_long_before_it_defeats_the_control_channel() {
    let leakage = |ratio_db: f32| {
        move |samples: &[Complex32]| {
            let offset = power(samples).sqrt() * 10f32.powf(ratio_db / 20.0);
            samples
                .iter()
                .map(|sample| sample + Complex32::new(offset, 0.0))
                .collect()
        }
    };
    assert_eq!(read(leakage(0.0)), "sib1");
    assert_eq!(read(leakage(9.0)), "sib1");
    assert_eq!(read(leakage(12.0)), "nothing");
}
