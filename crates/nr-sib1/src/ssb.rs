use num_complex::Complex32;

pub const SUBCARRIERS: usize = 240;
pub const SYMBOLS: usize = 4;
/// Coded PBCH bits carried by one SS/PBCH block. TS 38.212 section 7.1.5 rate matches the polar
/// codeword to this length, so it is an independent check on the resource-element layout below.
pub const PBCH_BITS: usize = 864;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Re {
    Pss,
    Sss,
    Dmrs,
    Pbch,
    Empty,
}

pub type Grid = [[Complex32; SUBCARRIERS]; SYMBOLS];

#[must_use]
pub fn extract(symbols: &[Vec<Complex32>], ssb_center: isize) -> Option<Grid> {
    if symbols.len() != SYMBOLS {
        return None;
    }
    let mut grid = [[Complex32::default(); SUBCARRIERS]; SYMBOLS];
    for (output, symbol) in grid.iter_mut().zip(symbols) {
        let centre = isize::try_from(symbol.len() / 2).ok()? + ssb_center;
        let start = usize::try_from(centre - 120).ok()?;
        output.copy_from_slice(symbol.get(start..start + SUBCARRIERS)?);
    }
    Some(grid)
}

/// Resource-element kinds of one SS/PBCH block, indexed by symbol then subcarrier, per TS 38.211
/// Table 7.4.3.1-1. `pci` only shifts the demodulation reference positions.
#[must_use]
pub fn layout(pci: u16) -> [[Re; SUBCARRIERS]; SYMBOLS] {
    let shift = usize::from(pci % 4);
    let broadcast = |subcarrier: usize| {
        if subcarrier % 4 == shift {
            Re::Dmrs
        } else {
            Re::Pbch
        }
    };
    let centre = 56..183;
    let guard =
        |subcarrier: usize| (48..56).contains(&subcarrier) || (183..192).contains(&subcarrier);
    [
        std::array::from_fn(|subcarrier| {
            if centre.contains(&subcarrier) {
                Re::Pss
            } else {
                Re::Empty
            }
        }),
        std::array::from_fn(broadcast),
        std::array::from_fn(|subcarrier| {
            if guard(subcarrier) {
                Re::Empty
            } else if centre.contains(&subcarrier) {
                Re::Sss
            } else {
                broadcast(subcarrier)
            }
        }),
        std::array::from_fn(broadcast),
    ]
}

/// Resource elements of `kind` in the order TS 38.211 section 7.3.3.3 maps them: ascending
/// subcarrier within ascending symbol.
#[must_use]
pub fn positions(pci: u16, kind: Re) -> Vec<(usize, usize)> {
    let grid = layout(pci);
    (0..SYMBOLS)
        .flat_map(|symbol| (0..SUBCARRIERS).map(move |subcarrier| (symbol, subcarrier)))
        .filter(|(symbol, subcarrier)| grid[*symbol][*subcarrier] == kind)
        .collect()
}

#[cfg(test)]
mod tests {
    use num_complex::Complex32;

    use super::{PBCH_BITS, Re, SUBCARRIERS, SYMBOLS, extract, layout, positions};

    #[test]
    fn the_layout_carries_exactly_one_polar_codeword() {
        for pci in [0_u16, 1, 2, 3, 17, 335, 1007] {
            let data = positions(pci, Re::Pbch).len();
            let reference = positions(pci, Re::Dmrs).len();
            assert_eq!(
                reference, 144,
                "pci {pci} must place 144 reference elements"
            );
            assert_eq!(data, 432, "pci {pci} must leave 432 data elements");
            assert_eq!(
                data * 2,
                PBCH_BITS,
                "QPSK over the data elements must equal the rate-matched length"
            );
        }
    }

    #[test]
    fn synchronisation_signals_occupy_the_middle_127_subcarriers() {
        let grid = layout(0);
        for (symbol, kind) in [(0, Re::Pss), (2, Re::Sss)] {
            let found: Vec<usize> = (0..SUBCARRIERS)
                .filter(|k| grid[symbol][*k] == kind)
                .collect();
            assert_eq!(found.len(), 127);
            assert_eq!((found[0], found[126]), (56, 182));
        }
    }

    #[test]
    fn reference_positions_shift_with_the_cell_identity() {
        for pci in 0..4_u16 {
            let first = positions(pci, Re::Dmrs)[0];
            assert_eq!(first, (1, usize::from(pci)));
        }
        assert_eq!(positions(4, Re::Dmrs)[0], positions(0, Re::Dmrs)[0]);
    }

    #[test]
    fn the_symbol_carrying_synchronisation_reserves_its_guard_subcarriers() {
        let grid = layout(0);
        for subcarrier in (48..56).chain(183..192) {
            assert_eq!(grid[2][subcarrier], Re::Empty);
        }
        let accounted: usize = [Re::Pss, Re::Sss, Re::Dmrs, Re::Pbch, Re::Empty]
            .iter()
            .map(|kind| positions(0, *kind).len())
            .sum();
        assert_eq!(accounted, SUBCARRIERS * SYMBOLS);
    }

    #[test]
    fn extraction_centres_each_ssb_symbol() {
        let symbols: Vec<Vec<Complex32>> = (0..SYMBOLS)
            .map(|symbol| {
                (0..512)
                    .map(|subcarrier| Complex32::new(symbol as f32, subcarrier as f32))
                    .collect()
            })
            .collect();
        let grid = extract(&symbols, -3).unwrap();
        assert_eq!(grid[0][0], Complex32::new(0.0, 133.0));
        assert_eq!(grid[3][239], Complex32::new(3.0, 372.0));
    }
}
