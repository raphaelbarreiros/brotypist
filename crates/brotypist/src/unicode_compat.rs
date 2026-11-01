//! Baseline-compatible Unicode 15 default-full, non-Turkic case folding.
//!
//! The pinned dependency has Unicode 9 tables. All differences through Unicode
//! 15 are single-scalar additions left unchanged by those tables, so supplement
//! their output without another buffer, allocation, or normalization pass.
//! Keep this source shared by the application, offline updater and tools.

use std::{iter::Once, str::Chars};

const _: () = match unicode_casefold::UNICODE_VERSION {
    (9, 0, 0) => (),
    _ => panic!("Revalidate the Unicode 15 supplement when casefold tables change"),
};

#[derive(Clone, Debug)]
pub struct CaseFold<I>(unicode_casefold::CaseFold<I>);

impl<I: Iterator<Item = char>> Iterator for CaseFold<I> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        self.0.next().map(supplement)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

pub trait UnicodeCaseFold<I: Iterator<Item = char>>: Sized {
    fn case_fold(self) -> CaseFold<I>;
}

impl<I: Iterator<Item = char>> UnicodeCaseFold<I> for I {
    fn case_fold(self) -> CaseFold<I> {
        CaseFold(unicode_casefold::UnicodeCaseFold::case_fold(self))
    }
}

impl<'a> UnicodeCaseFold<Chars<'a>> for &'a str {
    fn case_fold(self) -> CaseFold<Chars<'a>> {
        CaseFold(unicode_casefold::UnicodeCaseFold::case_fold(self))
    }
}

impl UnicodeCaseFold<Once<char>> for char {
    fn case_fold(self) -> CaseFold<Once<char>> {
        CaseFold(unicode_casefold::UnicodeCaseFold::case_fold(self))
    }
}

fn supplement(value: char) -> char {
    match value {
        '\u{1c90}'..='\u{1cba}' => {
            char::from_u32(value as u32 - 0xbc0).expect("Unicode 15 scalar mapping")
        }
        '\u{1cbd}'..='\u{1cbf}' => {
            char::from_u32(value as u32 - 0xbc0).expect("Unicode 15 scalar mapping")
        }
        '\u{2c2f}' => '\u{2c5f}',
        '\u{a7b8}' => '\u{a7b9}',
        '\u{a7ba}' => '\u{a7bb}',
        '\u{a7bc}' => '\u{a7bd}',
        '\u{a7be}' => '\u{a7bf}',
        '\u{a7c0}' => '\u{a7c1}',
        '\u{a7c2}' => '\u{a7c3}',
        '\u{a7c4}' => '\u{a794}',
        '\u{a7c5}' => '\u{282}',
        '\u{a7c6}' => '\u{1d8e}',
        '\u{a7c7}' => '\u{a7c8}',
        '\u{a7c9}' => '\u{a7ca}',
        '\u{a7d0}' => '\u{a7d1}',
        '\u{a7d6}' => '\u{a7d7}',
        '\u{a7d8}' => '\u{a7d9}',
        '\u{a7f5}' => '\u{a7f6}',
        '\u{10570}'..='\u{1057a}' => {
            char::from_u32(value as u32 + 0x27).expect("Unicode 15 scalar mapping")
        }
        '\u{1057c}'..='\u{1058a}' => {
            char::from_u32(value as u32 + 0x27).expect("Unicode 15 scalar mapping")
        }
        '\u{1058c}'..='\u{10592}' => {
            char::from_u32(value as u32 + 0x27).expect("Unicode 15 scalar mapping")
        }
        '\u{10594}' => '\u{105bb}',
        '\u{10595}' => '\u{105bc}',
        '\u{16e40}'..='\u{16e5f}' => {
            char::from_u32(value as u32 + 0x20).expect("Unicode 15 scalar mapping")
        }
        _ => value,
    }
}
