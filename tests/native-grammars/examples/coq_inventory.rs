//! Enumerates every native error, missing node and command for explicit sources.
//! The caller joins source hashes before and after; recovery is not semantic support.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Component, Path},
};

use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err("expected source root and JSON relative-path list".into());
    }
    let root = Path::new(&arguments[0]).canonicalize()?;
    let paths: Vec<String> = serde_json::from_slice(&fs::read(&arguments[1])?)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rocq::LANGUAGE.into())?;
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    for relative in paths {
        let path = Path::new(&relative);
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("input paths must contain only relative normal components".into());
        }
        let physical = root.join(path).canonicalize()?;
        if !physical.starts_with(&root)
            || fs::symlink_metadata(root.join(path))?
                .file_type()
                .is_symlink()
        {
            return Err("input escapes its source root or is a link".into());
        }
        let source = fs::read(physical)?;
        let checksum = source
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |state, byte| {
                (state ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        let tree = parser
            .parse(&source, None)
            .ok_or("native parser returned no tree")?;
        let root_node = tree.root_node();
        let mut pending = vec![(root_node, 0usize)];
        let mut errors: Vec<Value> = Vec::new();
        let mut commands = Vec::new();
        let mut fallbacks = Vec::new();
        let mut counts = BTreeMap::new();
        let mut node_count = 0usize;
        let mut maximum_depth = 0usize;
        while let Some((node, depth)) = pending.pop() {
            node_count += 1;
            maximum_depth = maximum_depth.max(depth);
            if source.get(node.byte_range()).is_none() {
                return Err("native node range exceeds physical source".into());
            }
            *counts.entry(node.kind().to_owned()).or_insert(0usize) += 1;
            if node.is_error() || node.is_missing() {
                errors.push(
                    json!({"kind":node.kind(), "start":node.start_byte(), "end":node.end_byte(),
                    "row":node.start_position().row, "column":node.start_position().column,
                    "error":node.is_error(), "missing":node.is_missing()}),
                );
            }
            if node.kind().ends_with("_command") || node.kind() == "ltac_definition" {
                commands.push(
                    json!({"kind":node.kind(), "start":node.start_byte(), "end":node.end_byte(),
                    "has_error":node.has_error(), "missing":node.is_missing()}),
                );
            }
            if matches!(node.kind(), "generic_tactic_body" | "custom_notation_block") {
                fallbacks.push(
                    json!({"kind":node.kind(), "start":node.start_byte(), "end":node.end_byte(),
                    "has_error":node.has_error(), "missing":node.is_missing()}),
                );
            }
            let mut cursor = node.walk();
            pending.extend(node.children(&mut cursor).map(|child| (child, depth + 1)));
        }
        errors.sort_by_key(|row| (row["start"].as_u64(), row["end"].as_u64()));
        commands.sort_by_key(|row| (row["start"].as_u64(), row["end"].as_u64()));
        fallbacks.sort_by_key(|row| (row["start"].as_u64(), row["end"].as_u64()));
        let row = json!({"path":relative, "bytes":source.len(),
            "source_fnv1a64_checksum":format!("{checksum:016x}"),
            "utf8_valid":std::str::from_utf8(&source).is_ok(),
            "root":{"kind":root_node.kind(), "start":root_node.start_byte(), "end":root_node.end_byte(),
                "has_error":root_node.has_error(), "missing":root_node.is_missing()},
            "node_count":node_count, "maximum_depth":maximum_depth, "node_kind_counts":counts,
            "errors_and_missing":errors, "native_command_nodes":commands, "native_fallback_nodes":fallbacks,
            "complete_structural_support_qualified":false, "semantic_support_qualified":false});
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
