// One individually named native control per exact compiler-qualified file.
// A fixture failure cannot hide the remaining LF/CRLF cases behind an early loop.

#[test]
fn prefix_trivia_adjacent_comment_prefix_lf_eof() {
    check_trivia(0);
}

#[test]
fn prefix_trivia_adjacent_comment_prefix_crlf_eof() {
    check_trivia(1);
}

#[test]
fn prefix_trivia_adjacent_comment_infix_lf_eof() {
    check_trivia(2);
}

#[test]
fn prefix_trivia_adjacent_comment_infix_crlf_eof() {
    check_trivia(3);
}

#[test]
fn prefix_trivia_nested_comment_prefix_lf_eof() {
    check_trivia(4);
}

#[test]
fn prefix_trivia_nested_comment_prefix_crlf_eof() {
    check_trivia(5);
}

#[test]
fn prefix_trivia_line_comment_deeper_argument_lf_eof() {
    check_trivia(6);
}

#[test]
fn prefix_trivia_line_comment_deeper_argument_crlf_eof() {
    check_trivia(7);
}

#[test]
fn prefix_trivia_line_comment_sibling_lf_eof() {
    check_trivia(8);
}

#[test]
fn prefix_trivia_line_comment_sibling_crlf_eof() {
    check_trivia(9);
}

#[test]
fn prefix_trivia_deeper_prefix_lf_eof() {
    check_trivia(10);
}

#[test]
fn prefix_trivia_deeper_prefix_crlf_eof() {
    check_trivia(11);
}

#[test]
fn prefix_trivia_deeper_spaced_minus_lf_eof() {
    check_trivia(12);
}

#[test]
fn prefix_trivia_deeper_spaced_minus_crlf_eof() {
    check_trivia(13);
}

#[test]
fn prefix_trivia_equal_indent_prefix_sibling_lf_eof() {
    check_trivia(14);
}

#[test]
fn prefix_trivia_equal_indent_prefix_sibling_crlf_eof() {
    check_trivia(15);
}
