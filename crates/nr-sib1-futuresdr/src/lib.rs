#![cfg(feature = "futuresdr")]

use futuresdr::runtime::dev::prelude::*;
use nr_sib1::{Config, Event, Failure, Reason, Stage, decode};
use num_complex::Complex32;

#[derive(Block)]
#[blocking]
#[message_outputs(events)]
pub struct Decoder<I = DefaultCpuReader<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
{
    #[input]
    input: I,
    config: Config,
    samples: Vec<Complex32>,
    limit: Option<usize>,
    exceeded: bool,
}

impl Decoder<DefaultCpuReader<Complex32>> {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self::with_limit(config, None)
    }

    #[must_use]
    pub fn with_limit(config: Config, limit: Option<usize>) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            config,
            samples: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl<I> Kernel for Decoder<I>
where
    I: CpuBufferReader<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        messages: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let input = self.input.slice();
        let count = input.len();
        if count > 0 {
            let accepted = self.limit.map_or(count, |limit| {
                count.min(limit.saturating_sub(self.samples.len()))
            });
            self.samples.extend_from_slice(&input[..accepted]);
            self.exceeded |= accepted < count;
            self.input.consume(count);
        }
        if self.input.finished() {
            let events = if self.exceeded {
                vec![Event::Failure(Failure {
                    pci: None,
                    stage: Stage::Sync,
                    sample: 0,
                    reasons: vec![Reason::Message(format!(
                        "complete window exceeded the configured limit of {} samples",
                        self.limit.unwrap()
                    ))],
                })]
            } else {
                decode(&self.samples, &self.config)
            };
            for event in events {
                messages.post("events", Pmt::Any(Box::new(event))).await?;
            }
            io.finished = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use futuresdr::runtime::Pmt;
    use futuresdr::runtime::mocker::{Mocker, Reader};
    use nr_sib1::{Config, Duplex, Event, Guard, Release, SsbCase, SubcarrierSpacing};
    use num_complex::Complex32;

    use super::Decoder;

    fn config() -> Config {
        Config {
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
            guard: Guard::default(),
        }
    }

    #[test]
    fn configured_limit_reports_truncation() {
        let mut input = Reader::default();
        input.set(vec![Complex32::default(); 3]);
        let decoder = Decoder {
            input,
            config: config(),
            samples: Vec::new(),
            limit: Some(2),
            exceeded: false,
        };
        let mut mock = Mocker::new(decoder);
        mock.run();
        let messages = mock.take_messages();
        let Pmt::Any(event) = messages
            .into_iter()
            .next()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("expected Event PMT");
        };
        let event = event.take::<Event>().expect("Event PMT");
        assert!(matches!(*event, Event::Failure(_)));
    }
}
