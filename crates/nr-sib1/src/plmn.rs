use crate::Plmn;

/// The ITU-T E.212 assignment behind a PLMN identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operator {
    /// ISO 3166-1 alpha-2 country of the assignment. The test and international ranges have none.
    pub country: Option<&'static str>,
    /// The name the network is sold under, which is the one an operator in the field recognises. A
    /// licensee that sells under several brands is named by its first.
    pub name: &'static str,
}

/// ITU-T E.212 assignments as `mcc`, `mnc`, country, name, embedded so a lookup needs no filesystem.
/// Derived from the public `mcc-mnc-list` compilation of the ITU bulletins, reduced to assignments a
/// cell can actually broadcast: three-digit country codes, two- or three-digit network codes, one
/// entry per identity.
///
/// IMPORTANT: the mobile network code is two or three digits and its width is part of the identity,
/// so it is matched as text. Trimming `610` to `61` names a different network.
const TABLE: &str = include_str!("../data/e212.tsv");

#[must_use]
pub fn operator(plmn: &Plmn) -> Option<Operator> {
    let identity = format!("{}\t{}\t", plmn.mcc, plmn.mnc);
    let entry = TABLE.lines().find(|line| line.starts_with(&identity))?;
    let mut fields = entry[identity.len()..].split('\t');
    let country = fields.next()?;
    Some(Operator {
        country: (!country.is_empty()).then_some(country),
        name: fields.next()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plmn(mcc: &str, mnc: &str) -> Plmn {
        Plmn {
            mcc: mcc.to_string(),
            mnc: mnc.to_string(),
            country: None,
            operator: None,
        }
    }

    #[test]
    fn every_entry_is_a_named_identity_a_cell_could_broadcast() {
        for line in TABLE.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 4, "{line}");
            assert!(
                fields[0].len() == 3 && fields[0].bytes().all(|byte| byte.is_ascii_digit()),
                "{line}"
            );
            assert!(
                (2..=3).contains(&fields[1].len())
                    && fields[1].bytes().all(|byte| byte.is_ascii_digit()),
                "{line}"
            );
            assert!(!fields[3].is_empty(), "{line}");
        }
    }

    /// The lookup takes the first match, so a repeated identity would silently hide one of its names.
    #[test]
    fn no_identity_is_listed_twice() {
        let mut identities: Vec<&str> = TABLE
            .lines()
            .map(|line| &line[..line.rmatch_indices('\t').nth(1).unwrap().0])
            .collect();
        let total = identities.len();
        identities.sort_unstable();
        identities.dedup();
        assert_eq!(identities.len(), total);
    }

    #[test]
    fn the_carriers_the_fleet_parks_beside_resolve_to_their_brands() {
        assert_eq!(
            operator(&plmn("302", "610")),
            Some(Operator {
                country: Some("CA"),
                name: "Bell Mobility",
            })
        );
        assert_eq!(
            operator(&plmn("302", "220")),
            Some(Operator {
                country: Some("CA"),
                name: "Telus Mobility",
            })
        );
        assert_eq!(
            operator(&plmn("302", "720")),
            Some(Operator {
                country: Some("CA"),
                name: "Rogers Wireless",
            })
        );
    }

    /// A two-digit code is not the three-digit code that shares its digits, and the reference capture
    /// broadcasts the test identity that would collide if it were.
    #[test]
    fn code_width_is_part_of_the_identity() {
        assert_eq!(operator(&plmn("001", "01")).unwrap().name, "TEST");
        assert_eq!(operator(&plmn("302", "61")), None);
        assert_eq!(operator(&plmn("302", "22")), None);
    }

    #[test]
    fn an_unassigned_identity_is_absent_rather_than_named() {
        assert_eq!(operator(&plmn("799", "99")), None);
    }
}
