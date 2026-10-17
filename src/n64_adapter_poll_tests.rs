use super::*;
#[test]
fn retained_history_reports_gap_and_exclusive_page_cursor() {
    let ring = (9..=12)
        .map(|sequence| Poll {
            sequence,
            word: 0xa6250000,
            frame: 17,
            vi: 34,
        })
        .collect();
    let (first, last, page) = select(&ring, 3, 2).unwrap();
    assert_eq!((first, last), (9, 12));
    assert_eq!(first.saturating_sub(3 + 1), 5);
    assert_eq!(
        page.iter().map(|p| p.sequence).collect::<Vec<_>>(),
        vec![9, 10]
    );
    let (_, _, rest) = select(&ring, 10, 2).unwrap();
    assert_eq!(
        rest.iter().map(|p| p.sequence).collect::<Vec<_>>(),
        vec![11, 12]
    );
    assert!(select(&ring, 13, 1).is_err());
    assert!(select(&ring, 12, 1).unwrap().2.is_empty());
    assert_eq!(page[0].word.to_le_bytes(), [0, 0, 37, 166]);
    assert!(select(&VecDeque::new(), 0, 1).unwrap().2.is_empty());
    assert!(select(&VecDeque::new(), 1, 1).is_err());
}
