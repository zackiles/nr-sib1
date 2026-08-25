use std::path::PathBuf;

use futuresdr::blocks::VectorSource;
use futuresdr::runtime::dev::prelude::*;
use nr_sib1::{Config, Event};
use nr_sib1_futuresdr::Decoder;
use num_complex::Complex32;
use serde_json::Value;

#[derive(Block)]
#[message_inputs(events)]
struct Print;

impl Print {
    #[allow(clippy::unused_async)]
    async fn events(
        &mut self,
        _io: &mut WorkIo,
        _messages: &mut MessageOutputs,
        _meta: &BlockMeta,
        message: Pmt,
    ) -> Result<Pmt> {
        if let Pmt::Any(value) = message
            && let Some(event) = value.downcast_ref::<Event>()
        {
            println!("{}", serde_json::to_string_pretty(event).unwrap());
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for Print {}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let default =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../nr-sib1/tests/fixtures/n3-sib1");
    let directory = std::env::args_os().nth(1).map_or(default, PathBuf::from);
    let truth: Value = serde_json::from_slice(&std::fs::read(directory.join("expect.json"))?)?;
    let config: Config = serde_json::from_value(truth["config"].clone())?;
    let bytes = std::fs::read(directory.join("capture.sigmf-data"))?;
    let all = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|sample| {
            Complex32::new(
                i32::from_le_bytes(sample[..4].try_into().unwrap()) as f32,
                i32::from_le_bytes(sample[4..].try_into().unwrap()) as f32,
            )
        })
        .collect::<Vec<_>>();
    let from = truth["from"].as_u64().unwrap_or(0) as usize;
    let to = truth["to"]
        .as_u64()
        .map_or(all.len(), |value| value as usize);
    let mut graph = Flowgraph::new();
    let source = VectorSource::<Complex32>::new(all[from..to].to_vec());
    let decoder = Decoder::new(config);
    let print = Print;
    connect!(graph, source.output > input.decoder; decoder.events | events.print);
    Runtime::new().run(graph)?;
    Ok(())
}
