//! Enumerate native syntax outcomes for an explicit physical Eiffel input list.
//! A parse without errors does not prove declaration completeness or semantics.

use std::{
    env, fs,
    io::{self, Write},
    path::Path,
    time::Instant,
};

use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err("expected source root and JSON relative-path list".into());
    }
    let root = Path::new(&arguments[0]);
    let paths: Vec<String> = serde_json::from_slice(&fs::read(&arguments[1])?)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())?;
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    for relative in paths {
        let path = Path::new(&relative);
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err("input paths must contain only relative normal components".into());
        }
        let source = fs::read(root.join(path))?;
        let started = Instant::now();
        let tree = parser
            .parse(&source, None)
            .ok_or("native parse returned no tree")?;
        let elapsed = started.elapsed();
        let mut pending = vec![(tree.root_node(), 0)];
        let mut errors: Vec<Value> = Vec::new();
        let mut declarations: Vec<Value> = Vec::new();
        let mut multiline_basic_strings = Vec::new();
        let mut count = 0;
        let mut depth = 0;
        while let Some((node, level)) = pending.pop() {
            count += 1;
            depth = depth.max(level);
            let bytes = source
                .get(node.byte_range())
                .ok_or("native range exceeds physical source")?;
            if node.is_error() || node.is_missing() {
                errors.push(json!({"kind":node.kind(),"missing":node.is_missing(),"start":node.start_byte(),"end":node.end_byte(),"line":node.start_position().row + 1}));
            }
            if node.kind() == "new_feature"
                || (node.kind() == "class_name"
                    && node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "class_declaration"))
            {
                declarations.push(json!({"kind":node.kind(),"start":node.start_byte(),"end":node.end_byte(),"utf8_text":std::str::from_utf8(bytes).ok(),"has_error":node.has_error(),"missing":node.is_missing()}));
            }
            if node.kind() == "basic_manifest_string" && bytes.contains(&b'\n') {
                multiline_basic_strings
                    .push(json!({"start":node.start_byte(),"end":node.end_byte()}));
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                pending.push((child, level + 1));
            }
        }
        errors.sort_by_key(|row| (row["start"].as_u64(), row["end"].as_u64()));
        declarations.sort_by_key(|row| (row["start"].as_u64(), row["end"].as_u64()));
        let row = json!({"path":relative,"bytes":source.len(),"utf8_valid":std::str::from_utf8(&source).is_ok(),"has_error":tree.root_node().has_error(),"node_count":count,"maximum_depth":depth,"parse_micros":elapsed.as_micros(),"errors":errors,"native_declarations":declarations,"multiline_basic_strings":multiline_basic_strings,"semantic_support_qualified":false});
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
