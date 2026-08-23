//! Renders an `apex-syntax` CST back to source text.
//!
//! `render(parse(source)) == source` (byte-for-byte) is the core round-trip
//! correctness property this crate exists to make checkable. Because the
//! tree is truly lossless (every raw token, trivia included, is a leaf
//! somewhere in it -- see `apex-parser`'s trivia-attachment sink),
//! rendering is just concatenating every token's text in source order:
//! `SyntaxNode::text()` already does exactly that.

use apex_syntax::SyntaxNode;

pub fn render(node: &SyntaxNode) -> String {
    node.text().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_round_trips_a_parsed_expression() {
        let src = "a + b * (c - d)";
        let parse = apex_parser::parse_expression(src);
        assert!(parse.errors.is_empty());
        assert_eq!(render(&parse.syntax()), src);
    }
}
