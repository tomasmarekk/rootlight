// Quote-padding controls bind to the unchanged private frontier helpers.
// Public anonymous nodes and raw ordered captures remain observable; private
// DEDENT padding and changed-range counts are not normalized into this contract.

const QUOTE_PADDING_CONTEXTUAL_QUERY: &str = "[(_expression) (_atomic_expression) (_arg_expression) \
     (_module_expression) (_application_operand/ce_expression) (application_expression) (sequential_expression) (typecast_expression)] @generic\n";

#[derive(Debug, PartialEq, Eq)]
struct QuotePaddingCapture {
    capture: String,
    walk_index: usize,
    kind: String,
    bytes: Range<usize>,
    start: Point,
    end: Point,
}

fn quote_padding_ordered_captures(tree: &Tree, source: &str) -> Vec<QuotePaddingCapture> {
    let pattern = format!(
        "{QUOTE_PADDING_CONTEXTUAL_QUERY}\
         (application_expression) @explicit_application\n\
         (literal_expression) @explicit_literal\n\
         (long_identifier_or_op) @explicit_identifier_expression\n\
         (const) @explicit_const\n"
    );
    let query = Query::new(&tree_sitter_fsharp::LANGUAGE_FSHARP.into(), &pattern)
        .expect("the unchanged contextual selector and explicit quote queries must compile");
    let walked = nodes(tree);
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let started = Instant::now();
    let mut checks = 0usize;
    let mut aborted = false;
    let mut watchdog = |_: &tree_sitter::QueryCursorState| {
        checks += 1;
        if checks > 20_000 || started.elapsed() > Duration::from_secs(10) {
            aborted = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut events = Vec::new();
    let mut generic = std::collections::BTreeSet::new();
    {
        let options = tree_sitter::QueryCursorOptions::new().progress_callback(&mut watchdog);
        let mut captures =
            cursor.captures_with_options(&query, tree.root_node(), source.as_bytes(), options);
        while let Some((found, index)) = captures.next() {
            assert!(
                events.len() < 20_000,
                "quote query exceeded its raw-event bound"
            );
            let actual = found.captures[*index];
            let capture = query.capture_names()[usize::try_from(actual.index).unwrap()];
            let joins: Vec<_> = walked
                .iter()
                .enumerate()
                .filter(|(_, (node, _))| *node == actual.node)
                .map(|(walk_index, _)| walk_index)
                .collect();
            assert_eq!(joins.len(), 1, "capture must join one live public node");
            if capture == "generic" {
                assert!(
                    generic.insert(actual.node.id()),
                    "duplicate raw generic capture"
                );
            }
            span(actual.node, source, text(actual.node, source));
            events.push(QuotePaddingCapture {
                capture: capture.to_owned(),
                walk_index: joins[0],
                kind: actual.node.kind().to_owned(),
                bytes: actual.node.byte_range(),
                start: actual.node.start_position(),
                end: actual.node.end_position(),
            });
        }
    }
    assert!(
        !aborted,
        "quote query exceeded its cancellation/work watchdog"
    );
    assert!(!cursor.did_exceed_match_limit());
    events
}

fn quote_padding_topology(tree: &Tree, source: &str, padding: &str) {
    clean(tree, source);
    let application = frontier_run_body(tree, source);
    let written = format!("<@ value{padding}@>");
    assert_eq!(source.matches(&written).count(), 1);
    let [callee, quotation] = frontier_application(
        application,
        source,
        ("long_identifier_or_op", "consume"),
        ("literal_expression", written.as_str()),
    );
    assert_eq!(application.child_count(), 2);
    let quoted = frontier_children(quotation, source, &[("long_identifier_or_op", "value")]);
    assert_eq!(quotation.child_count(), 3);
    specialized_argument_punctuation(quotation, source, 0, "<@");
    specialized_argument_punctuation(quotation, source, 2, "@>");
    let closer = quotation.child(2).unwrap();
    assert_eq!(&source[quoted[0].end_byte()..closer.start_byte()], padding);
    let head = exact(tree, source, "function_declaration_left", "run value");
    assert!(head.end_byte() < application.start_byte());
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_body = following.child_by_field_name("body").unwrap();
    let inventory = [application, callee, quotation, quoted[0], following_body];
    assert_eq!(
        expression_queries(tree, source, &inventory),
        inventory.len()
    );
    frontier_explicit_queries(tree, source);
}

fn quote_padding_projection(
    tree: &Tree,
    source: &str,
    padding: &str,
) -> ((String, Vec<Snapshot>), Vec<QuotePaddingCapture>) {
    quote_padding_topology(tree, source, padding);
    (
        snapshot(tree, source),
        quote_padding_ordered_captures(tree, source),
    )
}

fn quote_padding_incremental_case(index: usize) {
    let fixture = &SPECIALIZED_ARGUMENT_SOURCES[index];
    assert!(matches!(
        fixture.family,
        SpecializedArgumentFamily::Quotation
    ));
    let source = fixture.source;
    assert!(!source.ends_with('\n'));
    assert_eq!(source.matches("value @>").count(), 1);
    let mut reused = parser(false);
    let original = parse(&mut reused, source, None);
    let original_projection = quote_padding_projection(&original, source, " ");
    let mut before = source.to_owned();
    let mut current = original;
    for padding in ["  ", " ", " \t", " ", "   ", " "] {
        let after = source.replacen("value @>", &format!("value{padding}@>"), 1);
        let prefix_end = source.find("value @>").unwrap() + "value".len();
        assert_eq!(&before[..prefix_end], &source[..prefix_end]);
        assert_eq!(&after[..prefix_end], &source[..prefix_end]);
        assert_eq!(
            before.rsplit_once("@>").unwrap().1,
            after.rsplit_once("@>").unwrap().1
        );
        edit(&mut current, &before, &after);
        current = parse(&mut reused, &after, Some(&current));
        let fresh = parse(&mut parser(false), &after, None);
        assert_eq!(
            quote_padding_projection(&current, &after, padding),
            quote_padding_projection(&fresh, &after, padding),
            "{}: padding {padding:?}",
            fixture.id,
        );
        if after == source {
            assert_eq!(
                quote_padding_projection(&current, source, " "),
                original_projection
            );
        }
        before = after;
    }
    assert_eq!(before, source);
    assert_eq!(
        quote_padding_projection(&current, source, " "),
        original_projection
    );
}

#[test]
fn typed_quote_padding_edits_match_fresh_public_nodes_and_ordered_queries_lf() {
    quote_padding_incremental_case(4);
}

#[test]
fn typed_quote_padding_edits_match_fresh_public_nodes_and_ordered_queries_crlf() {
    quote_padding_incremental_case(5);
}
