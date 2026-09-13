use super::lexer::Lexeme;
use std::ops::{Index, Range};

// Keep a movable gap at the latest edit so disambiguating repeated modulo
// expressions does not shift the remaining source for every percent token.
pub(super) struct Tokens {
    before: Vec<Lexeme>,
    after: Vec<Lexeme>,
}

impl Tokens {
    pub fn new(mut tokens: Vec<Lexeme>) -> Self {
        tokens.reverse();
        Self {
            before: Vec::new(),
            after: tokens,
        }
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

    pub fn replace(&mut self, range: Range<usize>, replacement: Vec<Lexeme>) {
        while self.before.len() < range.start {
            self.before.push(self.after.pop().unwrap());
        }
        while self.before.len() > range.start {
            self.after.push(self.before.pop().unwrap());
        }
        self.after.truncate(self.after.len() - range.len());
        self.before.extend(replacement);
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
