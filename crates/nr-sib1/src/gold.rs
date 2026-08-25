const OFFSET: usize = 1600;

#[must_use]
pub fn sequence(initial: u32, length: usize) -> Vec<u8> {
    let end = OFFSET + length;
    let mut first = vec![0_u8; end + 31];
    let mut second = vec![0_u8; end + 31];
    first[0] = 1;
    for (index, bit) in second.iter_mut().take(31).enumerate() {
        *bit = ((initial >> index) & 1) as u8;
    }
    for index in 0..end {
        first[index + 31] = first[index + 3] ^ first[index];
        second[index + 31] =
            second[index + 3] ^ second[index + 2] ^ second[index + 1] ^ second[index];
    }
    (OFFSET..end)
        .map(|index| first[index] ^ second[index])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sequence;

    #[test]
    fn gold_sequence_is_binary_and_repeatable() {
        let first = sequence(0x4a3_1c2, 4096);
        let second = sequence(0x4a3_1c2, 4096);
        assert_eq!(first, second);
        assert!(first.iter().all(|bit| *bit <= 1));
        assert!(first.contains(&0));
        assert!(first.contains(&1));
    }

    #[test]
    fn initialization_changes_the_sequence() {
        assert_ne!(sequence(0, 256), sequence(1, 256));
        assert_ne!(sequence(1, 256), sequence(2, 256));
    }
}
