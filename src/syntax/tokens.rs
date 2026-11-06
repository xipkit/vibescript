use super::lexer::Lexeme;
use crate::checked::{Access, violation};
use crate::compilation::Buffer;
use std::ops::Range;

// Keep a movable gap at the latest edit so disambiguating repeated modulo
// expressions does not shift the remaining source for every percent token.
pub(super) struct Tokens<'a> {
    before: Buffer<Lexeme<'a>>,
    after: Buffer<Lexeme<'a>>,
}

impl<'a> Tokens<'a> {
    /// Builds a gap buffer from tokens in source order.
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

    /// The number of tokens on both sides of the gap.
    pub fn len(&self) -> usize {
        self.before.len() + self.after.len()
    }

    /// Reuses only untouched, complete lexing.
    /// Every token replacement writes a nonempty prefix; a lexical failure
    /// consumes the remaining source.
    pub fn original(self) -> Option<Self> {
        (self.before.is_empty()
            && !matches!(
                self.get(self.len().saturating_sub(2)).map(|t| &t.token),
                Some(super::Token::Invalid(_))
            ))
        .then_some(self)
    }

    /// The token at `index`, or none beyond the stream.
    #[inline]
    pub fn get(&self, index: usize) -> Option<&Lexeme<'a>> {
        if index < self.before.len() {
            self.before.get(index)
        } else {
            let index = self
                .after
                .len()
                .checked_sub(index - self.before.len())?
                .checked_sub(1)?;
            self.after.get(index)
        }
    }

    /// The token at `index`, or an internal violation when it is absent.
    #[inline]
    #[track_caller]
    pub fn at(&self, index: usize) -> Access<&Lexeme<'a>> {
        self.get(index).ok_or(violation("parser token must exist"))
    }

    /// The final token, if the stream is nonempty.
    pub fn last(&self) -> Option<&Lexeme<'a>> {
        self.len().checked_sub(1).and_then(|index| self.get(index))
    }

    /// The tokens in a range, omitting positions beyond the stream.
    pub fn range(&self, range: Range<usize>) -> impl DoubleEndedIterator<Item = &Lexeme<'a>> {
        (range.start..range.end.min(self.len())).filter_map(|index| self.get(index))
    }

    /// The tokens at or after `start`.
    pub fn from(&self, start: usize) -> impl DoubleEndedIterator<Item = &Lexeme<'a>> {
        self.range(start..self.len())
    }

    /// Finds the first matching token, charging each position visited.
    pub fn find(
        &self,
        positions: impl Iterator<Item = usize>,
        work: &dyn crate::compilation::Work,
        predicate: impl Fn(&Lexeme<'a>) -> bool,
    ) -> crate::Result<Option<&Lexeme<'a>>> {
        for index in positions {
            work.charge(1)?;
            let token = self.at(index)?;
            if predicate(token) {
                return Ok(Some(token));
            }
        }
        Ok(None)
    }

    /// Replaces an existing range, moving the gap to the edit.
    pub fn replace(
        &mut self,
        range: Range<usize>,
        replacement: Buffer<Lexeme<'a>>,
        work: &dyn crate::compilation::Work,
    ) -> crate::Result<()> {
        if range.start > range.end || range.end > self.len() {
            return Err(violation("replaced token range must exist").into());
        }
        while self.before.len() < range.start {
            work.charge(1)?;
            self.before.push(
                work,
                self.after.pop().ok_or(crate::checked::violation(
                    "token gap must have a following token",
                ))?,
            )?;
        }
        while self.before.len() > range.start {
            work.charge(1)?;
            self.after.push(
                work,
                self.before.pop().ok_or(crate::checked::violation(
                    "token gap must have a preceding token",
                ))?,
            )?;
        }
        self.after.truncate(self.after.len() - range.len());
        for token in replacement {
            work.charge(1)?;
            self.before.push(work, token)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn access_and_iteration_follow_the_gap_in_both_directions() {
        let input = super::super::lexer::Input::new("1 2 3", &());
        let mut tokens = Tokens::new(super::super::lexer::lex(input).unwrap(), &()).unwrap();
        for (range, source) in [(1..2, "4 5"), (0..1, "6"), (3..4, "7")] {
            let mut replacement =
                super::super::lexer::lex(super::super::lexer::Input::new(source, &())).unwrap();
            replacement.pop();
            tokens.replace(range, replacement, &()).unwrap();
        }
        let expected = [6, 4, 5, 7];
        for (index, value) in expected.into_iter().enumerate() {
            assert_eq!(
                tokens.at(index).unwrap().token,
                super::super::Token::Int(value)
            );
        }
        assert_eq!(tokens.last().unwrap().token, super::super::Token::Eof);
        let forward: Vec<_> = tokens
            .range(0..tokens.len())
            .map(|token| token.offset)
            .collect();
        let backward: Vec<_> = tokens.from(0).rev().map(|token| token.offset).collect();
        assert!(forward.iter().eq(backward.iter().rev()));
        assert_eq!(tokens.range(1..3).count(), 2);
        assert!(tokens.get(usize::MAX).is_none());
        assert!(tokens.from(usize::MAX).next().is_none());
        assert_eq!(
            crate::Error::from(tokens.at(usize::MAX).unwrap_err()).kind,
            ErrorKind::Internal
        );
    }

    #[test]
    fn invalid_replacement_ranges_leave_the_stream_intact() {
        let mut tokens = Tokens::new(Buffer::new(), &()).unwrap();
        assert!(tokens.get(0).is_none());
        assert!(tokens.last().is_none());
        assert!(tokens.range(0..usize::MAX).next().is_none());
        for range in [
            1..1,
            0..1,
            std::ops::Range { start: 1, end: 0 },
            0..usize::MAX,
        ] {
            let error = tokens.replace(range, Buffer::new(), &()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal);
            assert_eq!(tokens.len(), 0);
        }
        let error = tokens
            .find(std::iter::once(usize::MAX), &(), |_| false)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
    }
}
