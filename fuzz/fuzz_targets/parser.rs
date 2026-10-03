#![no_main]

use libfuzzer_sys::fuzz_target;
use vibescript::tooling::{self, TokenKind};

fuzz_target!(|source: &str| {
    if source.len() > 16_384 {
        return;
    }
    if let Ok(tokens) = tooling::tokens(source) {
        assert!(matches!(
            tokens.last().map(|token| &token.kind),
            Some(TokenKind::Eof)
        ));
        let mut end = 0;
        for token in tokens {
            assert!(token.span.start >= end);
            assert!(source.get(token.span.clone()).is_some());
            if let TokenKind::Template(spans) = token.kind {
                for span in spans {
                    assert!(span.start >= token.span.start && span.end <= token.span.end);
                    assert!(source.get(span).is_some());
                }
            }
            end = token.span.end;
        }
    }
    let _ = tooling::outline(source);
    let _ = tooling::unreachable(source);
});
