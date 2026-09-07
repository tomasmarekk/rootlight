; Node scopes preserve document, sequence and key context independently of names.
(stream) @root @module
[(document) (block_node) (flow_node) (block_mapping) (flow_mapping)
 (block_sequence) (flow_sequence) (block_sequence_item)] @scope
(flow_sequence [(flow_node) (flow_pair)] @scope)
[(block_mapping_pair) (flow_pair)] @declaration
(flow_mapping (flow_node) @declaration @definition)
[(block_mapping_pair key: (_) @definition)
 (flow_pair key: (_) @definition)]
[(block_mapping_pair !key) (flow_pair !key)] @definition

; Anchors annotate complete nodes. Names remain raw serialization tokens.
(anchor) @declaration
(anchor_name) @definition
(alias_name) @reference

; Required context survives optional evidence pressure and cached fact reuse.
[(yaml_directive) (tag_directive) (reserved_directive) (tag)] @signature
[(single_quote_scalar) (double_quote_scalar) (block_scalar)] @string
(comment) @comment @documentation
