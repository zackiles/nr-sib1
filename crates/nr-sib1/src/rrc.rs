use asn1_codecs::PerCodecData;
use asn1_codecs::uper::UperCodec;

use bitvec::slice::BitSlice;

use crate::nr_rrc::{
    BCCH_DL_SCH_Message, BCCH_DL_SCH_MessageType, BCCH_DL_SCH_MessageType_c1,
    BWP_UplinkCommonRach_ConfigCommon, RACH_ConfigCommonPrach_RootSequenceIndex,
};
use crate::{Bwp, Plmn, Prach, Sib1, SubcarrierSpacing};

#[must_use]
pub fn decode(input: &[u8], pci: u16, cell_barred: bool) -> Option<Sib1> {
    let mut data = PerCodecData::from_slice_uper(input);
    let message = BCCH_DL_SCH_Message::uper_decode(&mut data).ok()?;
    let BCCH_DL_SCH_MessageType::C1(BCCH_DL_SCH_MessageType_c1::SystemInformationBlockType1(sib1)) =
        message.message
    else {
        return None;
    };
    let identities = &sib1.cell_access_related_info.plmn_identity_info_list.0;
    let first = identities.first()?;
    let plmn = identities
        .iter()
        .flat_map(|info| &info.plmn_identity_list.0)
        .map(|identity| Plmn {
            mcc: identity
                .mcc
                .as_ref()
                .map_or_else(String::new, |mcc| digits(&mcc.0)),
            mnc: digits(&identity.mnc.0),
        })
        .collect();
    let serving = sib1.serving_cell_config_common?;
    let bands = serving
        .downlink_config_common
        .frequency_info_dl
        .frequency_band_list
        .0
        .iter()
        .filter_map(|band| band.freq_band_indicator_nr.as_ref().map(|value| value.0))
        .collect();
    let downlink_bandwidth_prb = serving
        .downlink_config_common
        .frequency_info_dl
        .scs_specific_carrier_list
        .0
        .first()?
        .carrier_bandwidth
        .0;
    let tdd_pattern = serving
        .tdd_ul_dl_configuration_common
        .as_ref()
        .and_then(|config| {
            let pattern = &config.pattern1;
            Some(crate::TddPattern {
                reference_spacing: spacing(config.reference_subcarrier_spacing.0)?,
                periodicity_us: periodicity(pattern.dl_ul_transmission_periodicity.0)?,
                downlink_slots: pattern.nrof_downlink_slots.0,
                downlink_symbols: pattern.nrof_downlink_symbols.0,
                uplink_slots: pattern.nrof_uplink_slots.0,
                uplink_symbols: pattern.nrof_uplink_symbols.0,
            })
        });
    let uplink = serving.uplink_config_common?;
    let initial_uplink_hz = uplink
        .frequency_info_ul
        .absolute_frequency_point_a
        .as_ref()
        .map(|value| arfcn(value.0));
    let bwp = uplink.initial_uplink_bwp;
    let BWP_UplinkCommonRach_ConfigCommon::Setup(rach) = bwp.rach_config_common? else {
        return None;
    };
    let generic = rach.rach_config_generic;
    let root_sequence_index = match rach.prach_root_sequence_index {
        RACH_ConfigCommonPrach_RootSequenceIndex::L839(index) => index.0,
        RACH_ConfigCommonPrach_RootSequenceIndex::L139(index) => u16::from(index.0),
    };
    Some(Sib1 {
        pci,
        bands,
        plmn,
        tracking_area_code: first
            .tracking_area_code
            .as_ref()
            .map(|value| bits(&value.0) as u32),
        cell_identity: Some(bits(&first.cell_identity.0)),
        cell_barred,
        downlink_bandwidth_prb,
        tdd_pattern,
        initial_uplink_hz,
        initial_uplink_bwp: Bwp {
            location_and_bandwidth: bwp.generic_parameters.location_and_bandwidth.0,
            subcarrier_spacing: spacing(bwp.generic_parameters.subcarrier_spacing.0)?,
        },
        prach: Prach {
            configuration_index: generic.prach_configuration_index.0,
            message_one_fdm: 1 << generic.msg1_fdm.0,
            frequency_start: generic.msg1_frequency_start.0,
            root_sequence_index,
            zero_correlation_zone: generic.zero_correlation_zone_config.0,
        },
    })
}

fn digits(values: &[crate::nr_rrc::MCC_MNC_Digit]) -> String {
    values
        .iter()
        .map(|digit| char::from(b'0' + digit.0))
        .collect()
}

fn bits(values: &BitSlice<u8, bitvec::order::Msb0>) -> u64 {
    values
        .iter()
        .fold(0, |value, bit| 2 * value + u64::from(*bit))
}

fn spacing(value: u8) -> Option<SubcarrierSpacing> {
    match value {
        0 => Some(SubcarrierSpacing::Khz15),
        1 => Some(SubcarrierSpacing::Khz30),
        _ => None,
    }
}

fn periodicity(value: u8) -> Option<u32> {
    [500, 625, 1_000, 1_250, 2_000, 2_500, 5_000, 10_000]
        .get(usize::from(value))
        .copied()
}

fn arfcn(value: u32) -> f64 {
    if value < 600_000 {
        f64::from(value) * 5e3
    } else if value < 2_016_667 {
        3e9 + f64::from(value - 600_000) * 15e3
    } else {
        24_250_080_000.0 + f64::from(value - 2_016_667) * 60e3
    }
}
