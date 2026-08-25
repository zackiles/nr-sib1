use std::path::{Path, PathBuf};

use nr_sib1::{Config, decode};
use num_complex::Complex32;
use serde_json::Value;

fn samples(path: &Path, datatype: &str) -> Result<Vec<Complex32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    match datatype {
        "cf32_le" => Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|sample| {
                Complex32::new(
                    f32::from_le_bytes(sample[..4].try_into().unwrap()),
                    f32::from_le_bytes(sample[4..].try_into().unwrap()),
                )
            })
            .collect()),
        "ci32_le" => Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|sample| {
                Complex32::new(
                    i32::from_le_bytes(sample[..4].try_into().unwrap()) as f32,
                    i32::from_le_bytes(sample[4..].try_into().unwrap()) as f32,
                )
            })
            .collect()),
        value => {
            Err(format!("unsupported SigMF datatype {value:?}; expected cf32_le or ci32_le").into())
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/n3-sib1");
    let directory = std::env::args_os().nth(1).map_or(default, PathBuf::from);
    let data = if directory.is_dir() {
        directory.join("capture.sigmf-data")
    } else {
        directory.clone()
    };
    let base = data.parent().unwrap_or(Path::new("."));
    let truth: Value = serde_json::from_slice(&std::fs::read(base.join("expect.json"))?)?;
    let config: Config = serde_json::from_value(truth["config"].clone())?;
    let metadata: Value = serde_json::from_slice(&std::fs::read(base.join("capture.sigmf-meta"))?)?;
    let datatype = metadata["global"]["core:datatype"]
        .as_str()
        .ok_or("SigMF metadata has no core:datatype")?;
    let all = samples(&data, datatype)?;
    let from = truth["from"].as_u64().unwrap_or(0) as usize;
    let to = truth["to"]
        .as_u64()
        .map_or(all.len(), |value| value as usize);
    for event in decode(&all[from..to], &config) {
        println!("{}", serde_json::to_string_pretty(&event)?);
    }
    Ok(())
}
