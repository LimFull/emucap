use super::pattern_matches;
use std::cell::Cell;

#[test]
fn repeated_prefix_work_is_linear() {
    struct Byte<'a>(u8, &'a Cell<usize>);
    impl PartialEq for Byte<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.1.set(self.1.get() + 1);
            self.0 == other.0
        }
    }
    let comparisons = Cell::new(0);
    let data: Vec<_> = (0..1024 * 1024).map(|_| Byte(0, &comparisons)).collect();
    let mut pattern: Vec<_> = (0..65536).map(|_| Byte(0, &comparisons)).collect();
    pattern.last_mut().unwrap().0 = 1;
    pattern_matches(&data, &pattern, |_| panic!("unexpected match"));
    assert!(comparisons.get() <= 4 * (data.len() + pattern.len()));
}

#[test]
fn overlap_alignment_and_early_stop_match_the_specification() {
    for n in 0..=7 {
        for bits in 0..(1u32 << n) {
            let data: Vec<_> = (0..n).map(|i| (bits >> i) & 1).collect();
            for m in 1..=4 {
                for pb in 0..(1u32 << m) {
                    let pattern: Vec<_> = (0..m).map(|i| (pb >> i) & 1).collect();
                    for align in 1..=3 {
                        let expected: Vec<_> = data
                            .windows(m)
                            .enumerate()
                            .filter(|(i, window)| i % align == 0 && *window == pattern)
                            .map(|(i, _)| i)
                            .take(2)
                            .collect();
                        let mut actual = Vec::new();
                        pattern_matches(&data, &pattern, |i| {
                            if i % align == 0 {
                                actual.push(i);
                            }
                            actual.len() < 2
                        });
                        assert_eq!(actual, expected);
                    }
                }
            }
        }
    }
}
