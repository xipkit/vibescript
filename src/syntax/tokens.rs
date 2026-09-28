use super::lexer::Lexeme;
use crate::compilation::Buffer;
use std::ops::{Index, Range};

// Keep a movable gap at the latest edit so disambiguating repeated modulo
// expressions does not shift the remaining source for every percent token.
pub(super) struct Tokens<'a> {
    before: Buffer<Lexeme<'a>>,
    after: Buffer<Lexeme<'a>>,
}

impl<'a> Tokens<'a> {
    pub fn new(
        mut tokens: Buffer<Lexeme<'a>>,
        work: &dyn crate::compilation::Work,
    ) -> crate::Result<Self> {
        for index in 0..tokens.len() / 2 {
            work.charge(1)?;
            let end = tokens.len() - 1 - index;
            tokens.swap(index, end);
        }
        Ok(Self {
            before: Buffer::new(),
            after: tokens,
        })
    }

    pub fn len(&self) -> usize {
        self.before.len() + self.after.len()
    }

    // Reuse only untouched, complete lexing. Every token replacement writes
    // a nonempty prefix; a lexical failure consumes the remaining source.
    pub fn original(self) -> Option<Self> {
        (self.before.is_empty()
            && !matches!(
                self.get(self.len().saturating_sub(2)).map(|t| &t.token),
                Some(super::Token::Invalid(_))
            ))
        .then_some(self)
    }

    pub fn get(&self, index: usize) -> Option<&Lexeme<'a>> {
        (index < self.len()).then(|| &self[index])
    }

    pub fn last(&self) -> Option<&Lexeme<'a>> {
        self.len().checked_sub(1).map(|index| &self[index])
    }

    pub fn range(&self, range: Range<usize>) -> impl DoubleEndedIterator<Item = &Lexeme<'a>> {
        range.map(|index| &self[index])
    }

    pub fn from(&self, start: usize) -> impl DoubleEndedIterator<Item = &Lexeme<'a>> {
        self.range(start..self.len())
    }

    pub fn find(
        &self,
        positions: impl Iterator<Item = usize>,
        work: &dyn crate::compilation::Work,
        predicate: impl Fn(&Lexeme<'a>) -> bool,
    ) -> crate::Result<Option<&Lexeme<'a>>> {
        for index in positions {
            work.charge(1)?;
            let token = &self[index];
            if predicate(token) {
                return Ok(Some(token));
            }
        }
        Ok(None)
    }

    pub fn replace(
        &mut self,
        range: Range<usize>,
        replacement: Buffer<Lexeme<'a>>,
        work: &dyn crate::compilation::Work,
    ) -> crate::Result<()> {
        while self.before.len() < range.start {
            work.charge(1)?;
            self.before.push(work, self.after.pop().unwrap())?;
        }
        while self.before.len() > range.start {
            work.charge(1)?;
            self.after.push(work, self.before.pop().unwrap())?;
        }
        self.after.truncate(self.after.len() - range.len());
        for token in replacement {
            work.charge(1)?;
            self.before.push(work, token)?;
        }
        Ok(())
    }
}

impl<'a> Index<usize> for Tokens<'a> {
    type Output = Lexeme<'a>;

    fn index(&self, index: usize) -> &Lexeme<'a> {
        if index < self.before.len() {
            &self.before[index]
        } else {
            &self.after[self.after.len() - 1 - (index - self.before.len())]
        }
    }
}
