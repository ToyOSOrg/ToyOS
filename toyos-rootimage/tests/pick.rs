//! The pick: one candidate carrying the name is the answer, and every other
//! count, or a table the loader did not look at whole, is a refusal saying which.

use toyos_rootimage::pick::{pick, Refused};

const ROOT: u32 = 0xA;
const OTHER: u32 = 0xB;

#[test]
fn no_candidate_names_it() {
    let candidates = [(1, Some(OTHER)), (2, None)];
    assert_eq!(pick(&ROOT, &candidates, 2), Err(Refused::Matches { matches: 0, candidates: 2 }));
}

#[test]
fn an_empty_table_names_nothing() {
    let candidates: [(u32, Option<u32>); 0] = [];
    assert_eq!(pick(&ROOT, &candidates, 0), Err(Refused::Matches { matches: 0, candidates: 0 }));
}

#[test]
fn one_candidate_names_it_among_others() {
    let candidates = [(1, Some(OTHER)), (2, None), (3, Some(ROOT))];
    assert_eq!(pick(&ROOT, &candidates, 3), Ok(3));
}

#[test]
fn two_candidates_name_it_and_neither_is_picked() {
    let candidates = [(1, Some(ROOT)), (2, Some(OTHER)), (3, Some(ROOT))];
    assert_eq!(pick(&ROOT, &candidates, 3), Err(Refused::Matches { matches: 2, candidates: 3 }));
}

#[test]
fn a_table_with_more_candidates_than_were_looked_at_is_refused() {
    let candidates = [(1, Some(ROOT)), (2, Some(OTHER))];
    assert_eq!(pick(&ROOT, &candidates, 3), Err(Refused::Unlisted { matched: 3, listed: 2 }));
}
