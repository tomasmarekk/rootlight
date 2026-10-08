#[test]
fn frontier_adjacent_string_and_name_lf_eof() {
    check_frontier(0);
}

#[test]
fn frontier_adjacent_string_and_name_crlf_eof() {
    check_frontier(1);
}

#[test]
fn frontier_adjacent_parenthesized_callee_lf_eof() {
    check_frontier(2);
}

#[test]
fn frontier_adjacent_parenthesized_callee_crlf_eof() {
    check_frontier(3);
}

#[test]
fn frontier_adjacent_array_argument_lf_eof() {
    check_frontier(4);
}

#[test]
fn frontier_adjacent_array_argument_crlf_eof() {
    check_frontier(5);
}

#[test]
fn frontier_adjacent_record_argument_lf_eof() {
    check_frontier(6);
}

#[test]
fn frontier_adjacent_record_argument_crlf_eof() {
    check_frontier(7);
}

#[test]
fn frontier_plus_prefix_left_space_lf_eof() {
    check_frontier(8);
}

#[test]
fn frontier_plus_prefix_left_space_crlf_eof() {
    check_frontier(9);
}

#[test]
fn frontier_plus_infix_no_spaces_lf_eof() {
    check_frontier(10);
}

#[test]
fn frontier_plus_infix_no_spaces_crlf_eof() {
    check_frontier(11);
}

#[test]
fn frontier_plus_infix_both_spaces_lf_eof() {
    check_frontier(12);
}

#[test]
fn frontier_plus_infix_both_spaces_crlf_eof() {
    check_frontier(13);
}

#[test]
fn frontier_plus_before_parenthesized_right_lf_eof() {
    check_frontier(14);
}

#[test]
fn frontier_plus_before_parenthesized_right_crlf_eof() {
    check_frontier(15);
}

#[test]
fn frontier_curry_same_line_lf_eof() {
    check_frontier(16);
}

#[test]
fn frontier_curry_same_line_crlf_eof() {
    check_frontier(17);
}

#[test]
fn frontier_curry_multiline_lf_eof() {
    check_frontier(18);
}

#[test]
fn frontier_curry_multiline_crlf_eof() {
    check_frontier(19);
}

#[test]
fn frontier_infix_same_line_lf_eof() {
    check_frontier(20);
}

#[test]
fn frontier_infix_same_line_crlf_eof() {
    check_frontier(21);
}

#[test]
fn frontier_infix_continuation_lf_eof() {
    check_frontier(22);
}

#[test]
fn frontier_infix_continuation_crlf_eof() {
    check_frontier(23);
}

#[test]
fn frontier_minus_adjacent_argument_lf_eof() {
    check_frontier(24);
}

#[test]
fn frontier_minus_adjacent_argument_crlf_eof() {
    check_frontier(25);
}

#[test]
fn frontier_minus_spaced_argument_lf_eof() {
    check_frontier(26);
}

#[test]
fn frontier_minus_spaced_argument_crlf_eof() {
    check_frontier(27);
}

#[test]
fn frontier_minus_no_spaces_lf_eof() {
    check_frontier(28);
}

#[test]
fn frontier_minus_no_spaces_crlf_eof() {
    check_frontier(29);
}

#[test]
fn frontier_plus_adjacent_argument_lf_eof() {
    check_frontier(30);
}

#[test]
fn frontier_plus_adjacent_argument_crlf_eof() {
    check_frontier(31);
}

#[test]
fn frontier_plus_spaced_argument_lf_eof() {
    check_frontier(32);
}

#[test]
fn frontier_plus_spaced_argument_crlf_eof() {
    check_frontier(33);
}

#[test]
fn frontier_plus_continuation_lf_eof() {
    check_frontier(34);
}

#[test]
fn frontier_plus_continuation_crlf_eof() {
    check_frontier(35);
}

#[test]
fn frontier_bitwise_prefix_argument_lf_eof() {
    check_frontier(36);
}

#[test]
fn frontier_bitwise_prefix_argument_crlf_eof() {
    check_frontier(37);
}

#[test]
fn frontier_dereference_prefix_argument_lf_eof() {
    check_frontier(38);
}

#[test]
fn frontier_dereference_prefix_argument_crlf_eof() {
    check_frontier(39);
}

#[test]
fn frontier_application_valued_builder_lf_eof() {
    check_frontier(40);
}

#[test]
fn frontier_application_valued_builder_crlf_eof() {
    check_frontier(41);
}

#[test]
fn frontier_completed_computation_callee_lf_eof() {
    check_frontier(42);
}

#[test]
fn frontier_completed_computation_callee_crlf_eof() {
    check_frontier(43);
}

#[test]
fn frontier_parenthesized_computation_argument_lf_eof() {
    check_frontier(44);
}

#[test]
fn frontier_parenthesized_computation_argument_crlf_eof() {
    check_frontier(45);
}

#[test]
fn frontier_nested_computation_argument_lf_eof() {
    check_frontier(46);
}

#[test]
fn frontier_nested_computation_argument_crlf_eof() {
    check_frontier(47);
}
