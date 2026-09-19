use super::lexer::Lexeme;
use crate::compilation::Buffer;
use std::ops::{Index, Range};

// Keep a movable gap at the latest edit so disambiguating repeated modulo
// expressions does not shift the remaining source for every percent token.
pub(super) struct Tokens {
    before: Buffer<Lexeme>,
    after: Buffer<Lexeme>,
}

impl Tokens {
    pub fn new(
        mut tokens: Buffer<Lexeme>,
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

    pub fn get(&self, index: usize) -> Option<&Lexeme> {
        (index < self.len()).then(|| &self[index])
    }

    pub fn last(&self) -> Option<&Lexeme> {
        self.len().checked_sub(1).map(|index| &self[index])
    }

    pub fn range(&self, range: Range<usize>) -> impl DoubleEndedIterator<Item = &Lexeme> {
        range.map(|index| &self[index])
    }

    pub fn from(&self, start: usize) -> impl DoubleEndedIterator<Item = &Lexeme> {
        self.range(start..self.len())
    }

    pub fn find(
        &self,
        positions: impl Iterator<Item = usize>,
        work: &dyn crate::compilation::Work,
        predicate: impl Fn(&Lexeme) -> bool,
    ) -> crate::Result<Option<&Lexeme>> {
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
        replacement: Buffer<Lexeme>,
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

impl Index<usize> for Tokens {
    type Output = Lexeme;

    fn index(&self, index: usize) -> &Lexeme {
        if index < self.before.len() {
            &self.before[index]
        } else {
            &self.after[self.after.len() - 1 - (index - self.before.len())]
        }
    }
}
