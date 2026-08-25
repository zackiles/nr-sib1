use std::io::{Read, Seek};
use std::path::PathBuf;

use nr_sib1::{
    Allocation, Config, Dci, Duplex, Event, Numerology, Ofdm, Release, SsbCase, SubcarrierSpacing,
    allocation, decode, decode_dci, decode_mib, decode_pbch, decode_rrc, decode_sib1, detect_dmrs,
    detect_pss, detect_sss, extract, monitor,
};
use num_complex::Complex32;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/n3-sib1")
        .join(name)
}

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
        guard: nr_sib1::Guard::default(),
    }
}

#[test]
fn annotated_capture_is_acquired_without_sample_annotations() {
    let bytes = std::fs::read(fixture("capture.sigmf-data")).unwrap();
    let samples = bytes
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
    let events = decode(&samples[830_000..860_000], &config());
    // One cell was on the air, so it is announced once however many of its blocks the window holds,
    // and a cell that decoded is never also reported as a failure.
    let [Event::Sync(sync), Event::Mib(mib), Event::Sib1(sib1)] = events.as_slice() else {
        panic!("expected one cell announced once and read, got {events:?}");
    };
    assert_eq!((sync.pci, mib.pci, sib1.pci), (1, 1, 1));
    assert_eq!(sib1.plmn[0].mcc, "001");
    assert_eq!(sib1.plmn[0].mnc, "01");
    assert_eq!(sib1.plmn[0].operator.as_deref(), Some("TEST"));
    assert_eq!(sib1.plmn[0].country, None);
}

#[test]
fn an_empty_window_reports_why_it_produced_no_cell() {
    let events = decode(
        &vec![Complex32::default(); 20_000],
        &Config {
            band: 71,
            center_hz: 635e6,
            ..config()
        },
    );
    let [Event::Failure(failure)] = events.as_slice() else {
        panic!("expected one failure, got {events:?}");
    };
    // A window that reached no cell has no cell to name, which is what tells it apart from a window
    // that found several and lost one of them.
    assert_eq!(failure.pci, None);
}

/// A class this decoder does not implement has to say so where an operator reads it, and the one place
/// that happens is a decode. `monitors` returns nothing for shared spectrum channel access, which is the
/// same empty answer it gives a MIB index no mapped table holds — and those are opposite facts. Reported
/// as the latter it names the cell's own broadcast as the thing at fault, and the cell is fine.
#[test]
fn a_shared_spectrum_cell_is_refused_by_name_and_not_as_a_broadcast_that_would_not_read() {
    let bytes = std::fs::read(fixture("capture.sigmf-data")).unwrap();
    let samples: Vec<Complex32> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|sample| {
            Complex32::new(
                i32::from_le_bytes(sample[..4].try_into().unwrap()) as f32,
                i32::from_le_bytes(sample[4..].try_into().unwrap()) as f32,
            )
        })
        .collect();
    let events = decode(
        &samples[830_000..860_000],
        &Config {
            shared_spectrum: true,
            ..config()
        },
    );
    let report = events
        .iter()
        .filter_map(|event| match event {
            Event::Failure(failure) => Some(failure.report()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        report.contains("shared spectrum channel access is outside this decoder's scope"),
        "reported {report:?}"
    );
    // The same window reads end to end without the flag, so the refusal is the channel access and not
    // anything about this capture.
    assert!(events.iter().all(|event| !matches!(event, Event::Sib1(_))));
}

fn symbol(start: u64, numerology: Numerology, prefix: usize) -> Vec<Complex32> {
    let mut bytes = vec![0_u8; (prefix + numerology.size) * 8];
    let mut file = std::fs::File::open(fixture("capture.sigmf-data")).unwrap();
    file.seek(std::io::SeekFrom::Start(start * 8)).unwrap();
    file.read_exact(&mut bytes).unwrap();
    let samples: Vec<Complex32> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|sample| {
            let real = i32::from_le_bytes(sample[..4].try_into().unwrap()) as f32;
            let imaginary = i32::from_le_bytes(sample[4..].try_into().unwrap()) as f32;
            Complex32::new(real, imaginary)
        })
        .collect();
    Ofdm::new(numerology).demodulate(&samples, prefix).unwrap()
}

#[test]
#[allow(clippy::too_many_lines)]
fn annotated_capture_decodes_the_physical_cell_identity() {
    let numerology = Numerology::new(SubcarrierSpacing::Khz15, 7.68e6).unwrap();
    let detected = detect_pss(&symbol(70_401, numerology, numerology.cp), 0).unwrap();
    assert_eq!(detected.nid2, 1);
    assert!(
        detected.score > 0.8,
        "PSS normalized correlation was only {}",
        detected.score
    );
    assert!(
        detected.margin_db > 10.0,
        "PSS hypothesis margin was only {}",
        detected.margin_db
    );
    let detected =
        detect_sss(&symbol(71_497, numerology, numerology.cp), 0, detected.nid2).unwrap();
    assert_eq!((detected.nid1, detected.pci), (0, 1));
    assert!(
        detected.score > 0.8,
        "SSS normalized correlation was only {}",
        detected.score
    );
    assert!(
        detected.margin_db > 10.0,
        "SSS hypothesis margin was only {}",
        detected.margin_db
    );
    let symbols = (0..4)
        .map(|index| symbol(70_401 + index * 548, numerology, numerology.cp))
        .collect::<Vec<_>>();
    let grid = extract(&symbols, 0).unwrap();
    let reference = detect_dmrs(&grid, detected.pci, 0..8).unwrap();
    assert_eq!(reference.index, 0);
    assert!(
        reference.score > 0.1,
        "PBCH reference correlation was only {}",
        reference.score
    );
    assert!(
        reference.margin_db > 10.0,
        "PBCH reference hypothesis margin was only {}",
        reference.margin_db
    );
    let codeword = decode_pbch(&grid, detected.pci, &reference, 4)
        .expect("PBCH polar code or CRC did not validate");
    let mib = decode_mib(&codeword, detected.pci, reference.index).unwrap();
    assert_eq!(mib.system_frame, 886);
    assert_eq!(mib.subcarrier_spacing_common, SubcarrierSpacing::Khz15);
    assert_eq!(mib.ssb_subcarrier_offset, 8);
    assert_eq!(mib.dmrs_type_a_position, 2);
    assert_eq!(mib.pdcch_config_sib1, 0);
    assert!(!mib.cell_barred);
    assert!(!mib.intra_frequency_reselection);
    assert_eq!(mib.ssb_index, 0);
    let monitor = monitor(&config(), &mib).unwrap();
    let control = vec![
        symbol(844_979, numerology, numerology.cp_long),
        symbol(845_531, numerology, numerology.cp),
    ];
    // The reference block sits eight subcarriers above the common grid, and CORESET#0 opens at that
    // grid, so the control region starts 128 subcarriers below the centre of the transform.
    let bwp = numerology.size / 2 - 120 - usize::from(mib.ssb_subcarrier_offset);
    let dci = decode_dci(&control, &monitor, mib.pci, bwp, 1, monitor.first_symbol).unwrap();
    assert_eq!(
        dci,
        Dci {
            first_resource_block: 0,
            resource_blocks: 8,
            time: 0,
            interleaved: false,
            mcs: 5,
            redundancy: 0,
            system_information: false,
        }
    );
    let placement = allocation(&dci, mib.dmrs_type_a_position).unwrap();
    assert_eq!(
        placement,
        Allocation {
            mapping: nr_sib1::Mapping::A,
            slots: 0,
            first_symbol: 2,
            symbols: 12,
        }
    );
    let mut start = 846_079;
    let data = (0..12)
        .map(|index| {
            let prefix = numerology.prefix(index + 2);
            let output = symbol(start, numerology, prefix);
            start += (prefix + numerology.size) as u64;
            output
        })
        .collect::<Vec<_>>();
    let part = nr_sib1::Region {
        start: bwp,
        resource_blocks: usize::from(monitor.resource_blocks),
    };
    let sib1 = decode_sib1(&data, &mib, &dci, numerology, 1_876_954_000.0, part, 1)
        .expect("DL-SCH LDPC or transport CRC did not validate");
    // The geometry the decoder uses is the geometry the planner uses, so the one place it is worked
    // out has to agree with the position this hand-checked capture actually decoded from.
    assert_eq!(
        bwp,
        numerology.size / 2 - nr_sib1::coreset(&mib, &monitor).below
    );
    assert_eq!(sib1, std::fs::read(fixture("sib1-transport.bin")).unwrap());
    let rrc = decode_rrc(&sib1, mib.pci, mib.cell_barred).unwrap();
    assert_eq!(rrc.plmn[0].mcc, "001");
    assert_eq!(rrc.plmn[0].mnc, "01");
    assert_eq!(rrc.bands, vec![3]);
    assert_eq!(rrc.tracking_area_code, Some(7));
    assert_eq!(rrc.cell_identity, Some(6576));
    assert_eq!(rrc.downlink_bandwidth_prb, 25);
    assert_eq!(rrc.initial_uplink_hz, Some(1_779_850_000.0));
    assert_eq!(rrc.initial_uplink_bwp.location_and_bandwidth, 6600);
    assert_eq!(
        rrc.initial_uplink_bwp.subcarrier_spacing,
        SubcarrierSpacing::Khz15
    );
    assert_eq!(rrc.prach.configuration_index, 1);
    assert_eq!(rrc.prach.message_one_fdm, 1);
    assert_eq!(rrc.prach.frequency_start, 4);
    assert_eq!(rrc.prach.root_sequence_index, 1);
}

#[test]
fn annotated_sib1_capture_is_present_with_expected_truth() {
    let iq = std::fs::metadata(fixture("capture.sigmf-data")).unwrap();
    assert_eq!(iq.len(), 8_802_176);
    let metadata = std::fs::read_to_string(fixture("capture.sigmf-meta")).unwrap();
    for expected in [
        "\"core:datatype\": \"ci32_le\"",
        "\"core:sample_rate\": 7680000.0",
        "\"core:frequency\": 1876954000.0",
        "NcellID = 1",
        "SFN = 886",
        "'controlResourceSetZero': 0",
        "'searchSpaceZero': 0",
        "'ssb-SubcarrierOffset': 8",
    ] {
        assert!(
            metadata.contains(expected),
            "fixture metadata lost {expected}"
        );
    }
    assert!(fixture("nr-rrc-17.3.0.asn1").metadata().unwrap().len() > 1_000_000);
}
