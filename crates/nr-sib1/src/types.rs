use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Release {
    R18,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Duplex {
    Fdd,
    Tdd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SsbCase {
    A,
    B,
    C,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SubcarrierSpacing {
    Khz15,
    Khz30,
}

impl SubcarrierSpacing {
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Khz15 => 15_000,
            Self::Khz30 => 30_000,
        }
    }

    #[must_use]
    pub const fn mu(self) -> u8 {
        match self {
            Self::Khz15 => 0,
            Self::Khz30 => 1,
        }
    }
}

/// Clearances our own receive chain needs that no part of 3GPP asks for. Configuration rather than a
/// constant, and hashed into the session, so a capture records the numbers it was planned against.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guard {
    /// Width around the tuner's own frequency kept out of CORESET#0. A direct-conversion receiver
    /// leaks its oscillator at the frequency it is tuned to, and a control region carrying that spike
    /// loses both its payload and the reference symbols the payload is equalized against.
    ///
    /// TODO: measure the leak and derive this, rather than holding a constant that is generous
    /// wherever it is not simply wrong.
    pub dc_hz: f64,
    /// How far inside the analog passband each edge must sit. The filter corner is not a wall, so an
    /// edge placed exactly on it is measured through the transition band.
    pub margin_hz: f64,
}

impl Default for Guard {
    fn default() -> Self {
        Self {
            dc_hz: 500e3,
            margin_hz: 500e3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub release: Release,
    pub band: u16,
    pub duplex: Duplex,
    pub sample_rate_hz: f64,
    pub center_hz: f64,
    /// Width around `center_hz` the analog filter actually passes. Outside it the window is the
    /// receiver's own rolloff, so a control region that reaches past this edge was never captured
    /// however much of the transform is nominally there.
    pub usable_hz: f64,
    /// Minimum channel bandwidth of the band, which is what selects the CORESET#0 table of TS 38.213
    /// section 13. It is a property of the band and the numerology, never a constant.
    pub minimum_channel_bandwidth_hz: f64,
    pub spacing: SubcarrierSpacing,
    pub ssb_case: SsbCase,
    pub gscn: Option<u32>,
    pub shared_spectrum: bool,
    pub ntn: bool,
    pub minimum_quality_db: f64,
    #[serde(default)]
    pub guard: Guard,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sync {
    pub pci: u16,
    pub nid1: u16,
    pub nid2: u8,
    pub sample: u64,
    pub cfo_hz: f64,
    pub ssb_hz: f64,
    pub quality_db: f64,
    pub ssb_index: Option<u8>,
    pub half_frame: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mib {
    pub pci: u16,
    pub system_frame: u16,
    pub half_frame: bool,
    pub subcarrier_spacing_common: SubcarrierSpacing,
    pub ssb_subcarrier_offset: u8,
    pub dmrs_type_a_position: u8,
    pub pdcch_config_sib1: u8,
    pub cell_barred: bool,
    pub intra_frequency_reselection: bool,
    pub ssb_index: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plmn {
    pub mcc: String,
    pub mnc: String,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub operator: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sib1 {
    pub pci: u16,
    pub bands: Vec<u16>,
    pub plmn: Vec<Plmn>,
    pub tracking_area_code: Option<u32>,
    pub cell_identity: Option<u64>,
    pub cell_barred: bool,
    pub downlink_bandwidth_prb: u16,
    pub tdd_pattern: Option<TddPattern>,
    pub initial_uplink_hz: Option<f64>,
    pub initial_uplink_bwp: Bwp,
    pub prach: Prach,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TddPattern {
    pub reference_spacing: SubcarrierSpacing,
    pub periodicity_us: u32,
    pub downlink_slots: u16,
    pub downlink_symbols: u8,
    pub uplink_slots: u16,
    pub uplink_symbols: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bwp {
    pub location_and_bandwidth: u16,
    pub subcarrier_spacing: SubcarrierSpacing,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prach {
    pub configuration_index: u8,
    pub message_one_fdm: u8,
    pub frequency_start: u16,
    pub root_sequence_index: u16,
    pub zero_correlation_zone: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Stage {
    Sync,
    Pbch,
    Pdcch,
    Pdsch,
    Sib1,
}

/// Why a cell was not followed to SIB1.
///
/// Structured rather than prose because these are read back to decide what to do next, and the two
/// that matter most are indistinguishable as sentences: a control region that fell outside the capture
/// is a plannable geometry problem, while a control region that was present and failed its CRC is a
/// signal problem. Reporting the first as the second is what hid a whole cell for a sweep.
///
/// `Message` carries the prose conditions of the occasion walk — a slot that precedes the block, a
/// frame of the wrong parity — which are narrations of position rather than kinds of failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// CORESET#0, where the MIB placed it, reaches outside the window that found the block.
    /// `required_hz` is the narrowest window centred on the tuner that would hold the whole region;
    /// `available_hz` is what the analog filter passed.
    CoresetOutsideCapture {
        required_hz: f64,
        available_hz: f64,
    },
    /// The MIB signalled a common spacing that is not the spacing this capture was demodulated at, so
    /// the control region sits on a grid this transform cannot address.
    CoresetSpacing {
        common_hz: u32,
        captured_hz: u32,
    },
    /// A UE is not expected to decode an SI-RNTI PDSCH above QPSK (TS 38.214 section 5.1.3.1), so this
    /// says the DCI was misparsed rather than that a demapper is missing.
    UnexpectedSib1Modulation {
        qm: u8,
    },
    /// The frequency-domain assignment reaches outside the initial bandwidth part, which for a SIB1 is
    /// CORESET#0 — so the DCI was misparsed rather than the block being weak.
    Assigned {
        first_resource_block: usize,
        resource_blocks: usize,
        bwp: usize,
    },
    Unmapped {
        row: u8,
        dmrs_type_a_position: u8,
    },
    Unmodulated {
        mcs: u8,
    },
    Symbols {
        wanted: usize,
        got: usize,
    },
    /// No carrier-phase model explained the reference symbols it was fitted to, so nothing between
    /// them could be equalized against one.
    Rotation,
    /// The allocation implies a transport block outside what a single code block carries.
    TransportSize,
    /// TS 38.212 section 7.2.2 selected base graph one for a SIB1, which cannot happen while SI-RNTI
    /// forces QPSK and SIB1 stays under 2976 bits. Only the encoder for base graph two exists, so this
    /// says a premise moved rather than that the block was weak.
    BaseGraph {
        size: usize,
        rate: f64,
    },
    /// Shared spectrum channel access, which selects Tables 13-1A and 13-4A and indexes its SS/PBCH
    /// blocks off a discovery burst. Neither is implemented, so this is a class outside the decoder's
    /// scope rather than a cell it looked for and could not read.
    SharedSpectrum,
    /// Everything the decoder needed was present and the block did not validate.
    Undecoded,
    Message(String),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CoresetOutsideCapture {
                required_hz,
                available_hz,
            } => write!(
                formatter,
                "CORESET#0 needs {:.2} MHz of window and this capture passes {:.2} MHz",
                required_hz / 1e6,
                available_hz / 1e6
            ),
            Self::CoresetSpacing {
                common_hz,
                captured_hz,
            } => write!(
                formatter,
                "CORESET#0 is at {} kHz and this capture was demodulated at {} kHz",
                common_hz / 1000,
                captured_hz / 1000
            ),
            Self::UnexpectedSib1Modulation { qm } => write!(
                formatter,
                "the SI-RNTI DCI asked for modulation order {qm}, which SIB1 is never sent at"
            ),
            Self::Assigned {
                first_resource_block,
                resource_blocks,
                bwp,
            } => write!(
                formatter,
                "the assignment of {resource_blocks} resource blocks from {first_resource_block} \
                 reaches outside a {bwp}-block bandwidth part"
            ),
            Self::Unmapped {
                row,
                dmrs_type_a_position,
            } => write!(
                formatter,
                "time-domain row {row} has no entry for dmrs-TypeA-Position {dmrs_type_a_position}"
            ),
            Self::Unmodulated { mcs } => write!(formatter, "MCS {mcs} is reserved"),
            Self::Symbols { wanted, got } => write!(
                formatter,
                "the allocation is {wanted} symbols and {got} were demodulated"
            ),
            Self::Rotation => {
                formatter.write_str("no carrier-phase model explained the PDSCH reference symbols")
            }
            Self::TransportSize => {
                formatter.write_str("the allocation implies no single-code-block transport size")
            }
            Self::BaseGraph { size, rate } => write!(
                formatter,
                "{size} bits at rate {rate:.3} selects base graph one, which no SIB1 uses"
            ),
            Self::SharedSpectrum => formatter.write_str(
                "shared spectrum channel access is outside this decoder's scope: it needs Tables \
                 13-1A and 13-4A and discovery-burst SS/PBCH indexing, and has neither",
            ),
            Self::Undecoded => formatter.write_str("the DL-SCH did not decode"),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Failure {
    /// The cell this describes, when the search got far enough to read one. A window can hold several
    /// cells and lose only some of them, so a failure that named no cell could not be told apart from
    /// a window that decoded nothing.
    pub pci: Option<u16>,
    pub stage: Stage,
    pub sample: u64,
    /// Every distinct reason the walk collected, bounded so an operator reads a sentence rather than a
    /// log. Several are normal: one cell offers many occasions and they can fail differently.
    pub reasons: Vec<Reason>,
}

impl Failure {
    /// The reasons as one sentence, which is what a record and an operator get.
    #[must_use]
    pub fn report(&self) -> String {
        self.reasons
            .iter()
            .map(Reason::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Sync(Sync),
    Mib(Mib),
    Sib1(Sib1),
    Failure(Failure),
}
