//! Shared parser-independent structural fact classification.

use std::{borrow::Cow, cmp::Ordering};

use rootlight_ir::{EntityKind, FactDomain};

use crate::{SyntaxFact, SyntaxFactKind};

#[path = "structural_assembly.rs"]
mod assembly;

#[path = "structural_common_lisp.rs"]
mod common_lisp;

#[path = "structural_fsharp.rs"]
mod fsharp;

#[path = "structural_glsl.rs"]
mod glsl;

#[path = "structural_hlsl.rs"]
mod hlsl;

#[path = "structural_ocaml.rs"]
mod ocaml;

#[path = "structural_scheme.rs"]
mod scheme;

#[path = "structural_elixir.rs"]
mod elixir;
#[path = "structural_protobuf.rs"]
mod protobuf;
#[path = "structural_thrift.rs"]
mod thrift;
#[path = "structural_wgsl.rs"]
mod wgsl;

#[path = "structural_ada.rs"]
mod ada;
#[path = "structural_clojure.rs"]
mod clojure;
#[path = "structural_crystal.rs"]
mod crystal;
#[path = "structural_cue.rs"]
mod cue;
#[path = "structural_d.rs"]
mod d;
#[path = "structural_fish.rs"]
mod fish;
#[path = "structural_haxe.rs"]
mod haxe;
#[path = "structural_hcl.rs"]
mod hcl;
#[path = "structural_json_schema.rs"]
mod json_schema;
#[path = "structural_jsx.rs"]
mod jsx;
#[path = "structural_modelica.rs"]
mod modelica;
#[path = "structural_mojo.rs"]
mod mojo;
#[path = "structural_nim.rs"]
mod nim;
#[path = "structural_pony.rs"]
mod pony;

#[path = "structural_chapel.rs"]
mod chapel;
#[path = "structural_coffeescript.rs"]
mod coffeescript;
#[path = "structural_dockerfile.rs"]
mod dockerfile;
#[path = "structural_flux.rs"]
mod flux;
#[path = "structural_gdscript.rs"]
mod gdscript;
#[path = "structural_hare.rs"]
mod hare;
#[path = "structural_idris.rs"]
mod idris;
#[path = "structural_lean.rs"]
mod lean;
#[path = "structural_less.rs"]
mod less;
#[path = "structural_meson.rs"]
mod meson;
#[path = "structural_pug.rs"]
mod pug;
#[path = "structural_qml.rs"]
mod qml;
#[path = "structural_qsharp.rs"]
mod qsharp;
#[path = "structural_racket.rs"]
mod racket;
#[path = "structural_ring.rs"]
mod ring;
#[path = "structural_sass.rs"]
mod sass;
#[path = "structural_sed.rs"]
mod sed;
#[path = "structural_sml_wat.rs"]
mod sml_wat;
#[path = "structural_wolfram.rs"]
mod wolfram;
#[path = "structural_zig.rs"]
mod zig;

#[path = "structural_cmake.rs"]
mod cmake;
#[path = "structural_coq.rs"]
mod coq;
#[path = "structural_eiffel.rs"]
mod eiffel;
#[path = "structural_hdl.rs"]
mod hdl;
#[path = "structural_julia.rs"]
mod julia;
#[path = "structural_v.rs"]
mod v;
#[path = "structural_vhdl.rs"]
mod vhdl;
#[path = "structural_xml.rs"]
mod xml;

pub use fsharp::StructuralClassificationGap;

/// Returns whether a C-family signature capture is preprocessor context.
///
/// Conditional and unsupported replacement markers remain coverage evidence;
/// they must never compete with a declaration's written signature discriminator.
/// Authored macro headers and parameter lists return `false`.
#[must_use]
pub fn structural_signature_is_preprocessor_context(fact: &SyntaxFact) -> bool {
    !structural_preprocessor_context_gaps(fact).is_empty()
}

/// Returns reviewed C-family context gaps for both structural and project lowering.
///
/// Each entry requires an exact-span `UnsupportedConstruct` skipped region in
/// its domain. Context facts remain mandatory coverage evidence even though
/// they never contribute a written declaration signature or symbol identity.
#[must_use]
pub fn structural_preprocessor_context_gaps(
    fact: &SyntaxFact,
) -> &'static [(FactDomain, &'static str)] {
    if fact.kind() != SyntaxFactKind::Signature {
        return &[];
    }
    match fact.syntax_kind().as_str() {
        "c.preprocessor_condition.signature" => &[
            (
                FactDomain::Entities,
                "c-preprocessor-conditional-evaluation-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "c-preprocessor-conditional-evaluation-unavailable",
            ),
            (
                FactDomain::Relations,
                "c-preprocessor-conditional-evaluation-unavailable",
            ),
        ],
        "cpp.preprocessor_condition.signature" => &[
            (
                FactDomain::Entities,
                "cpp-preprocessor-conditional-evaluation-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "cpp-preprocessor-conditional-evaluation-unavailable",
            ),
            (
                FactDomain::Relations,
                "cpp-preprocessor-conditional-evaluation-unavailable",
            ),
        ],
        "c.preprocessor_lexical_unavailable.signature" => &[(
            FactDomain::Occurrences,
            "c-preprocessor-replacement-lexical-unavailable",
        )],
        "cpp.preprocessor_lexical_unavailable.signature" => &[
            (
                FactDomain::Entities,
                "cpp-preprocessor-replacement-lexical-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "cpp-preprocessor-replacement-lexical-unavailable",
            ),
            (
                FactDomain::Relations,
                "cpp-preprocessor-replacement-lexical-unavailable",
            ),
        ],
        "cpp.preprocessor_parameters.signature" => &[
            (
                FactDomain::Entities,
                "cpp-macro-parameter-bindings-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "cpp-macro-parameter-bindings-unavailable",
            ),
            (
                FactDomain::Relations,
                "cpp-macro-parameter-bindings-unavailable",
            ),
        ],
        "c.preprocessor_projection_unavailable.signature" => &[
            (
                FactDomain::Entities,
                "c-preprocessor-replacement-not-analyzed",
            ),
            (
                FactDomain::Occurrences,
                "c-preprocessor-replacement-not-analyzed",
            ),
            (
                FactDomain::Relations,
                "c-preprocessor-replacement-not-analyzed",
            ),
        ],
        "cpp.preprocessor_projection_unavailable.signature" => &[
            (
                FactDomain::Entities,
                "cpp-preprocessor-replacement-not-analyzed",
            ),
            (
                FactDomain::Occurrences,
                "cpp-preprocessor-replacement-not-analyzed",
            ),
            (
                FactDomain::Relations,
                "cpp-preprocessor-replacement-not-analyzed",
            ),
        ],
        _ => &[],
    }
}

/// Returns the grammar-reviewed base entity kind for one structural syntax fact.
///
/// File and project lowering share this base classifier and refine direct
/// type members with [`structural_type_member_kind`]. Rust `impl` ownership
/// additionally requires the caller's language-specific scope evidence.
/// Unsupported F# declaration shapes return `None`; callers must retain the
/// corresponding [`structural_entity_classification_gap`] in entity coverage.
/// Assembly, Common Lisp, Eiffel, GLSL, HLSL, OCaml, Scheme, WGSL, XML and DTD accept
/// only exact reviewed labels; unknown labels never use the substring classifier.
/// HLSL buffer labels and contexts are not declarations, types or named scopes.
/// Physical patch headers and hunks are document sections, never target-language declarations.
/// Cap'n Proto accepts exact schema-owner labels; aliases and generic binding
/// never fall through to the generic substring classifier.
#[must_use]
pub fn structural_entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    let label = fact.syntax_kind().as_str();
    if jsx::owns(label) {
        return jsx::entity_kind(fact);
    }
    if label.starts_with("hcl.") {
        return hcl::entity_kind(fact);
    }
    if label.starts_with("ada.") {
        return ada::entity_kind(fact);
    }
    if label.starts_with("json_schema.") {
        return json_schema::entity_kind(fact);
    }
    if label.starts_with("flux.") {
        return flux::entity_kind(fact);
    }
    if label.starts_with("less.") {
        return less::entity_kind(fact);
    }
    if label.starts_with("sass.") {
        return sass::entity_kind(fact);
    }
    if label.starts_with("dockerfile.") {
        return dockerfile::entity_kind(fact);
    }
    if label.starts_with("wolfram.") {
        return wolfram::entity_kind(fact);
    }
    if label.starts_with("sed.") {
        return sed::entity_kind(fact);
    }
    if label.starts_with("idris.") {
        return idris::entity_kind(fact);
    }
    if label.starts_with("lean.") {
        return lean::entity_kind(fact);
    }
    if label.starts_with("pug.") {
        return pug::entity_kind(fact);
    }
    if label.starts_with("coffeescript.") {
        return coffeescript::entity_kind(fact);
    }
    if label.starts_with("meson.") {
        return meson::entity_kind(fact);
    }
    if label.starts_with("gdscript.") {
        return gdscript::entity_kind(fact);
    }
    if label.starts_with("qsharp.") {
        return qsharp::entity_kind(fact);
    }
    if label.starts_with("chapel.") {
        return chapel::entity_kind(fact);
    }
    if label.starts_with("sml.") || label.starts_with("wat.") {
        return sml_wat::entity_kind(fact);
    }
    if label.starts_with("mojo.") {
        return mojo::entity_kind(fact);
    }
    if label.starts_with("haxe.") {
        return haxe::entity_kind(fact);
    }
    if label.starts_with("qml.") {
        return qml::entity_kind(fact);
    }
    if label.starts_with("hare.") {
        return hare::entity_kind(fact);
    }
    if label.starts_with("ring.") {
        return ring::entity_kind(fact);
    }
    if label.starts_with("modelica.") {
        return modelica::entity_kind(fact);
    }
    if label.starts_with("d.") {
        return d::entity_kind(fact);
    }
    if label.starts_with("zig.") {
        return zig::entity_kind(fact);
    }
    if label.starts_with("crystal.") {
        return crystal::entity_kind(fact);
    }
    if label.starts_with("cue.") {
        return cue::entity_kind(fact);
    }
    if label.starts_with("pony.") {
        return pony::entity_kind(fact);
    }
    if label.starts_with("fish.") {
        return fish::entity_kind(fact);
    }
    if label.starts_with("nim.") {
        return nim::entity_kind(fact);
    }
    if label.starts_with("clojure.") {
        return clojure::entity_kind(fact);
    }
    if label.starts_with("racket.") {
        return racket::entity_kind(fact);
    }
    if label.starts_with("graphql.") {
        return match (fact.kind(), label) {
            (SyntaxFactKind::Module, "graphql.file.module") => Some(EntityKind::Module),
            (
                SyntaxFactKind::Declaration,
                "graphql.object.declaration" | "graphql.input.declaration",
            ) => Some(EntityKind::Struct),
            (SyntaxFactKind::Declaration, "graphql.interface.declaration") => {
                Some(EntityKind::Interface)
            }
            (SyntaxFactKind::Declaration, "graphql.union.declaration") => Some(EntityKind::Union),
            (SyntaxFactKind::Declaration, "graphql.enum.declaration") => Some(EntityKind::Enum),
            (SyntaxFactKind::Declaration, "graphql.scalar.declaration") => {
                Some(EntityKind::TypeAlias)
            }
            (
                SyntaxFactKind::Declaration,
                "graphql.field.declaration" | "graphql.input_field.declaration",
            ) => Some(EntityKind::Field),
            (SyntaxFactKind::Declaration, "graphql.argument.declaration") => {
                Some(EntityKind::Parameter)
            }
            (SyntaxFactKind::Declaration, "graphql.enum_value.declaration") => {
                Some(EntityKind::Constant)
            }
            (SyntaxFactKind::Declaration, "graphql.variable.declaration") => {
                Some(EntityKind::Variable)
            }
            (
                SyntaxFactKind::Declaration,
                "graphql.operation.declaration"
                | "graphql.fragment.declaration"
                | "graphql.directive.declaration",
            ) => Some(EntityKind::Function),
            _ => None,
        };
    }
    if label.starts_with("elixir.") {
        return elixir::entity_kind(fact);
    }
    if label.starts_with("protobuf.") {
        return protobuf::entity_kind(fact);
    }
    if label.starts_with("thrift.") {
        return thrift::entity_kind(fact);
    }
    if label.starts_with("capnp.") {
        return match (fact.kind(), label) {
            (SyntaxFactKind::Module, "capnp.file.module") => Some(EntityKind::Module),
            (SyntaxFactKind::Declaration, "capnp.struct.declaration") => Some(EntityKind::Struct),
            (SyntaxFactKind::Declaration, "capnp.interface.declaration") => {
                Some(EntityKind::Interface)
            }
            (SyntaxFactKind::Declaration, "capnp.enum.declaration") => Some(EntityKind::Enum),
            (SyntaxFactKind::Declaration, "capnp.union.declaration") => Some(EntityKind::Union),
            (
                SyntaxFactKind::Declaration,
                "capnp.field.declaration" | "capnp.group.declaration",
            ) => Some(EntityKind::Field),
            (
                SyntaxFactKind::Declaration,
                "capnp.parameter.declaration" | "capnp.return_parameter.declaration",
            ) => Some(EntityKind::Parameter),
            (
                SyntaxFactKind::Declaration,
                "capnp.enum_value.declaration" | "capnp.const.declaration",
            ) => Some(EntityKind::Constant),
            (SyntaxFactKind::Declaration, "capnp.method.declaration") => Some(EntityKind::Method),
            (SyntaxFactKind::Declaration, "capnp.annotation.declaration") => {
                Some(EntityKind::Function)
            }
            _ => None,
        };
    }
    if label.starts_with("patch.") {
        return match (fact.kind(), label) {
            (SyntaxFactKind::Module, "patch.file.module") => Some(EntityKind::Module),
            (
                SyntaxFactKind::Declaration,
                "patch.file_section.declaration" | "patch.hunk.declaration",
            ) => Some(EntityKind::DocumentSection),
            _ => None,
        };
    }
    if label.starts_with("thinkscript.") {
        return match (fact.kind(), label) {
            (SyntaxFactKind::Declaration, "thinkscript.script.declaration") => {
                Some(EntityKind::Function)
            }
            (SyntaxFactKind::Declaration, "thinkscript.input.declaration") => {
                Some(EntityKind::Parameter)
            }
            _ => None,
        };
    }
    if label.starts_with("coq.") {
        return coq::entity_kind(fact);
    }
    if fact.kind() == SyntaxFactKind::Declaration {
        match label {
            "rust.local_variable.declaration" => return Some(EntityKind::Variable),
            "rust.parameter.declaration" => return Some(EntityKind::Parameter),
            "rust.anonymous_function.declaration" => return Some(EntityKind::Function),
            _ => {}
        }
    }
    if label.starts_with("v.") {
        return v::entity_kind(fact);
    }
    if label.starts_with("vhdl.") {
        return vhdl::entity_kind(fact);
    }
    if label.starts_with("verilog.") || label.starts_with("systemverilog.") {
        return hdl::entity_kind(fact);
    }
    if label.starts_with("eiffel.") {
        return eiffel::entity_kind(fact);
    }
    if label.starts_with("julia.") {
        return julia::entity_kind(fact);
    }
    if label.starts_with("cmake.") {
        return cmake::entity_kind(fact);
    }
    if label.starts_with("ocaml.") || label.starts_with("ocaml_interface.") {
        return ocaml::entity_kind(fact);
    }
    if label.starts_with("xml.") || label.starts_with("dtd.") {
        return xml::entity_kind(fact);
    }
    if label.starts_with("assembly.") {
        return assembly::entity_kind(fact);
    }
    if label.starts_with("fsharp.") {
        return fsharp::entity_kind(fact).ok().flatten();
    }
    if label.starts_with("common_lisp.") {
        return common_lisp::entity_kind(fact);
    }
    if label.starts_with("glsl.") {
        return glsl::entity_kind(fact);
    }
    if label.starts_with("hlsl.") {
        return hlsl::entity_kind(fact);
    }
    if label.starts_with("scheme.") {
        return scheme::entity_kind(fact);
    }
    if label.starts_with("wgsl.") {
        return wgsl::entity_kind(fact);
    }
    match fact.kind() {
        SyntaxFactKind::Module if label == "vba.class.module" => Some(EntityKind::Class),
        SyntaxFactKind::Module => Some(EntityKind::Module),
        SyntaxFactKind::Declaration if label == "haskell.module.declaration" => {
            Some(EntityKind::Module)
        }
        SyntaxFactKind::Declaration if label == "cobol.program.declaration" => {
            Some(EntityKind::Module)
        }
        SyntaxFactKind::Declaration if label == "fortran.program.declaration" => {
            Some(EntityKind::Module)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "plsql.package.declaration" | "plsql.package_body.declaration"
            ) =>
        {
            Some(EntityKind::Module)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "plsql.procedure.declaration" | "plsql.function.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration if label == "plsql.record_type.declaration" => {
            Some(EntityKind::Struct)
        }
        SyntaxFactKind::Declaration if label == "plsql.field.declaration" => {
            Some(EntityKind::Field)
        }
        SyntaxFactKind::Declaration if label == "plsql.parameter.declaration" => {
            Some(EntityKind::Parameter)
        }
        SyntaxFactKind::Declaration if label == "plsql.variable.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label == "plsql.trigger.declaration" => {
            Some(EntityKind::DatabaseObject)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "sas.fcmp_function.declaration"
                    | "sas.fcmp_subroutine.declaration"
                    | "sas.macro.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "sas.fcmp_parameter.declaration" | "sas.macro_parameter.declaration"
            ) =>
        {
            Some(EntityKind::Parameter)
        }
        SyntaxFactKind::Declaration if label == "sas.data_target.declaration" => {
            Some(EntityKind::DatabaseObject)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "pascal.unit.declaration"
                    | "pascal.program.declaration"
                    | "pascal.library.declaration"
            ) =>
        {
            Some(EntityKind::Module)
        }
        SyntaxFactKind::Declaration if label == "pascal.object.declaration" => {
            Some(EntityKind::Struct)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "pascal.class_variable.declaration" | "pascal.class_variable_fragment.declaration"
            ) =>
        {
            Some(EntityKind::Field)
        }
        SyntaxFactKind::Declaration if label == "pascal.procedure.declaration" => {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration if label == "pascal.destructor.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "fortran.subroutine.declaration"
                    | "fortran.separate_module_procedure.declaration"
                    | "fortran.interface_function.declaration"
                    | "fortran.interface_subroutine.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration if label == "fortran.derived_type.declaration" => {
            Some(EntityKind::Struct)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "cobol.data_item.declaration" | "cobol.file_descriptor.declaration"
            ) =>
        {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "cobol.section.declaration" | "cobol.paragraph.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "javascript.import_binding.declaration"
                    | "typescript.import_binding.declaration"
                    | "javascript.type_import_binding.declaration"
                    | "typescript.type_import_binding.declaration"
                    | "groovy.import_alias.declaration"
            ) =>
        {
            Some(EntityKind::Import)
        }
        SyntaxFactKind::Declaration if label == "markdown.section.declaration" => {
            Some(EntityKind::DocumentSection)
        }
        SyntaxFactKind::Declaration if label == "ini.section.declaration" => {
            Some(EntityKind::DocumentSection)
        }
        SyntaxFactKind::Declaration if label == "ini.key.declaration" => Some(EntityKind::Field),
        SyntaxFactKind::Declaration if label == "make.target.declaration" => {
            Some(EntityKind::BuildTarget)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "typescript.type_parameter.declaration"
                    | "typescript.mapped_type_parameter.declaration"
                    | "typescript.infer_parameter.declaration"
                    | "objective_c.type_parameter.declaration"
                    | "groovy.type_parameter.declaration"
            ) =>
        {
            Some(EntityKind::TypeParameter)
        }
        SyntaxFactKind::Declaration if label == "markdown.link_definition.declaration" => {
            Some(EntityKind::LinkDefinition)
        }
        SyntaxFactKind::Declaration if label == "vba.statement_label.declaration" => {
            Some(EntityKind::StatementLabel)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "dart.extension.declaration"
                    | "powershell.hashtable.declaration"
                    | "scala.object.declaration"
                    | "scala.package.declaration"
                    | "scala.package_object.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "scala.enum_value.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "solidity.event.declaration" | "vba.event.declaration"
            ) =>
        {
            Some(EntityKind::Event)
        }
        SyntaxFactKind::Declaration if label == "solidity.error.declaration" => {
            Some(EntityKind::ErrorDeclaration)
        }
        SyntaxFactKind::Declaration if label == "solidity.modifier.declaration" => {
            Some(EntityKind::Modifier)
        }
        SyntaxFactKind::Declaration if label == "solidity.enum_value.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "sql.table.declaration"
                    | "sql.view.declaration"
                    | "sql.materialized_view.declaration"
                    | "sql.index.declaration"
                    | "sql.sequence.declaration"
                    | "sql.trigger.declaration"
                    | "sql.database.declaration"
                    | "sql.role.declaration"
                    | "sql.extension.declaration"
                    | "sql.type.declaration"
            ) =>
        {
            Some(EntityKind::DatabaseObject)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "sql.schema.declaration" | "tcl.namespace.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "sql.column.declaration" => Some(EntityKind::Field),
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "html.element.declaration" | "astro.element.declaration"
            ) =>
        {
            Some(EntityKind::MarkupElement)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "html.attribute.declaration" | "astro.attribute.declaration"
            ) =>
        {
            Some(EntityKind::MarkupAttribute)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "toml.table.declaration" | "toml.table_array_element.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "css.style_rule.declaration" | "scss.style_rule.declaration"
            ) =>
        {
            Some(EntityKind::StyleRule)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "css.keyframes.declaration" | "scss.keyframes.declaration"
            ) =>
        {
            Some(EntityKind::Keyframes)
        }
        SyntaxFactKind::Declaration if label == "css.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "yaml.anchor.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label == "scss.variable.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label == "scss.mixin.declaration" => {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "json.property.declaration"
                    | "jsonc.property.declaration"
                    | "csharp.property.declaration"
                    | "groovy.property.declaration"
                    | "matlab.property.declaration"
                    | "objective_c.property.declaration"
                    | "powershell.property.declaration"
                    | "powershell.dynamic_property.declaration"
                    | "toml.property.declaration"
                    | "yaml.property.declaration"
                    | "vba.get_property.declaration"
                    | "vba.let_property.declaration"
                    | "vba.set_property.declaration"
                    | "vba.conditional_property.declaration"
                    | "pascal.property.declaration"
            ) =>
        {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "swift.protocol.declaration"
                    | "objective_c.protocol.declaration"
                    | "objective_c.forward_protocol.declaration"
            ) =>
        {
            Some(EntityKind::Protocol)
        }
        // Actors are reference types; the syntax label retains their concurrency distinction.
        SyntaxFactKind::Declaration if label == "swift.actor.declaration" => {
            Some(EntityKind::Class)
        }
        SyntaxFactKind::Declaration if label == "swift.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "ruby.namespace.declaration" | "perl.package.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "java.annotation.declaration" => {
            Some(EntityKind::Interface)
        }
        SyntaxFactKind::Declaration if label == "java.annotation_element.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration if label == "elm.port.declaration" => {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "haskell.data_type.declaration" | "haskell.newtype.declaration"
            ) =>
        {
            Some(EntityKind::Enum)
        }
        SyntaxFactKind::Declaration if label == "haskell.class.declaration" => {
            Some(EntityKind::Trait)
        }
        SyntaxFactKind::Declaration if label == "haskell.type_synonym.declaration" => {
            Some(EntityKind::TypeAlias)
        }
        SyntaxFactKind::Declaration if label == "agda.data.declaration" => Some(EntityKind::Enum),
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "prolog.predicate.declaration" | "prolog.nonterminal.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration if label.contains("constructor") => {
            Some(EntityKind::Constructor)
        }
        SyntaxFactKind::Declaration if label.contains("record") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("method") => Some(EntityKind::Method),
        SyntaxFactKind::Declaration if label.contains("function") => Some(EntityKind::Function),
        SyntaxFactKind::Declaration if label.contains("class") => Some(EntityKind::Class),
        SyntaxFactKind::Declaration if label.contains("struct") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label == "vba.enum_constant.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration if label.contains("enum") => Some(EntityKind::Enum),
        SyntaxFactKind::Declaration if label.contains("trait") => Some(EntityKind::Trait),
        SyntaxFactKind::Declaration if label.contains("interface") => Some(EntityKind::Interface),
        SyntaxFactKind::Declaration
            if label.contains("type_alias")
                || label.contains("type_item")
                || label.contains("type.declaration") =>
        {
            Some(EntityKind::TypeAlias)
        }
        SyntaxFactKind::Declaration
            if label.contains("constant")
                || label.contains("const_item")
                || label.contains("const.declaration") =>
        {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if label.contains("static_item") || label.contains("static.declaration") =>
        {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label.contains("field") => Some(EntityKind::Field),
        SyntaxFactKind::Declaration if label.contains("parameter") => Some(EntityKind::Parameter),
        SyntaxFactKind::Declaration if label.contains("variable") => Some(EntityKind::Variable),
        _ => None,
    }
}

/// Returns the source-backed gap for a declaration whose entity kind is unknown.
///
/// Reviewed F# ambiguous type representations, unrefined member specifications
/// and unsupported declaration labels retain their exact span as an Entities
/// [`rootlight_ir::SkippedRegionReason::UnsupportedConstruct`] region. Callers
/// must account for the gap before omitting an unclassified declaration.
/// Non-declarations, reviewed F# kinds and facts outside the exact `fsharp.`
/// label namespace return `None`; unrelated language policies are unchanged.
#[must_use]
pub fn structural_entity_classification_gap(
    fact: &SyntaxFact,
) -> Option<StructuralClassificationGap> {
    fsharp::entity_kind(fact).err()
}

/// Refines a structural kind when the caller has proven a direct type owner.
///
/// Both file and project lowering use this rule. External VBA declarations
/// describe imported procedures, not methods implemented by an instance.
/// Fortran interface bodies likewise describe procedure contracts, not methods.
#[must_use]
pub fn structural_type_member_kind(fact: &SyntaxFact, kind: EntityKind) -> EntityKind {
    match (kind, fact.syntax_kind().as_str()) {
        (
            EntityKind::Function,
            "vba.external_function.declaration"
            | "vba.external_sub_function.declaration"
            | "fortran.interface_function.declaration"
            | "fortran.interface_subroutine.declaration",
        ) => kind,
        (EntityKind::Function, _) => EntityKind::Method,
        (EntityKind::Variable, "vba.variable.declaration") => EntityKind::Field,
        _ => kind,
    }
}

/// Refines a base structural kind using only the captured declaration source.
///
/// The Go grammar represents aliases, structs, and interfaces with one
/// `type_spec` node. Its declaration text is therefore the only reviewed local
/// evidence that can distinguish those kinds without repository execution.
#[must_use]
pub fn structural_entity_kind_from_source(
    fact: &SyntaxFact,
    declaration: &str,
) -> Option<EntityKind> {
    let kind = structural_entity_kind(fact)?;
    if fact.syntax_kind().as_str() != "go.type.declaration" {
        return Some(kind);
    }
    let header = declaration.trim_start();
    if header.contains("interface") {
        Some(EntityKind::Interface)
    } else if header.contains("struct") {
        Some(EntityKind::Struct)
    } else {
        Some(kind)
    }
}

/// Returns the bounded name accepted from a reviewed structural capture.
///
/// Project analyzers use the same boundary when preserving structural
/// declarations so grammar-valid scoped names cannot disappear during
/// semantic refinement.
#[must_use]
pub fn structural_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let candidate = text.trim();
    (!candidate.is_empty()
        && candidate.len() <= maximum_bytes
        && candidate.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '/' | '\\' | '(' | ')' | '{' | '}' | '[' | ']')
        }))
    .then_some(candidate)
}

/// Returns a bounded canonical name from a grammar-reviewed language capture.
///
/// Lua member captures may contain formatting or comments between qualifiers.
/// Their canonical identity removes only that trivia; the caller must preserve
/// the original source span. Ruby additionally admits its bracket and division
/// method operators, including a static singleton receiver. CSS preserves raw
/// grammar-reviewed selectors and identifiers: whitespace, escapes and Unicode
/// normalization can change their meaning. This boundary bounds those captures;
/// it does not validate arbitrary CSS text. JSON keys retain a canonical quoted
/// JSON string, so empty keys and controls cannot collide with ordinary names.
/// Equivalent escapes have the same identity; unpaired UTF-16 units remain
/// escaped rather than becoming replacement characters. TOML key paths quote
/// each decoded segment independently: `a.b` differs from `"a.b"`, while bare,
/// basic and literal spellings of the same segment agree. TOML rejects non-scalar
/// escapes; this validates key syntax, not table ownership or document semantics.
/// YAML untagged flow scalars use YAML 1.2 Core resolution and a type-prefixed
/// canonical identity. Plain, single-quoted and double-quoted strings fold lines
/// and decode their own escapes; numeric identities do not round through floats.
/// This scalar boundary does not resolve document directives, tags, anchors,
/// aliases, block scalars or collection keys; those require document context.
/// HTML preserves grammar-reviewed source names verbatim, including foreign
/// markup spelling; it does not perform browser case or namespace adjustments.
/// SQL removes only trivia between qualified name components, retaining case,
/// delimiters and escape spelling until a dialect-aware resolver is available.
/// R decodes backticks and quoted assignment targets into UTF-8 names without
/// case folding or Unicode normalization. Invalid or non-UTF-8 escapes remain
/// unavailable; canonical spelling does not infer runtime binding equivalence.
/// Scala removes enclosing identifier backticks without changing their contents;
/// grammar-reviewed symbolic names retain their exact spelling, including `/`.
/// Haskell terminal names retain exact identifier and operator spelling. Captures
/// exclude prefix/infix wrappers; this boundary does not infer lexical binding.
/// Dart removes trivia between qualified name components and additionally retains
/// its grammar-reviewed division and bracket operators, without resolving receivers.
/// PowerShell preserves written names, including braced variables, without runtime
/// scope expansion, escape evaluation or case-insensitive binding equivalence.
/// Nix retains reviewed static attribute paths exactly, including quotes and trivia;
/// canonical source identity is not evaluated attribute-name equivalence.
/// Markdown preserves authored heading and label text; rendered text, reference
/// case folding and generated fragment identifiers require separate interpretation.
/// Make retains reviewed static relative paths in targets and prerequisites;
/// expansions and escaped names remain outside that borrowed-name contract.
/// Tcl retains exact literal command names, including `/` and parentheses;
/// substitution and namespace lookup remain outside that source identity.
/// ECMAScript identifiers decode Unicode escapes without Unicode normalization;
/// other grammar-reviewed names keep the shared borrowed-name contract.
/// Groovy constant member strings decode their own delimiters and ordered escapes.
/// Empty, control-bearing, edge-whitespace and quote-prefixed names retain an
/// injective JSON-quoted identity, including unpaired UTF-16 units. Interpolated
/// names are unavailable rather than evaluated; source spans retain all quoting.
/// PL/SQL retains exact ordinary or double-quoted identifier spelling; source
/// identity does not establish case folding or database name-resolution equivalence.
/// SAS retains exact ASCII identifiers or static one- or two-part dataset names;
/// macro expansion, name literals and runtime dataset identity remain unavailable.
/// Pascal retains raw ASCII identifiers, including written `&` escapes and dotted
/// module names, without case folding, de-escaping or qualifier binding.
/// Common Lisp retains complete written symbol captures, including bars, single
/// escapes and package punctuation; caller-owned native evidence must prove the
/// capture is a symbol, not an arbitrary form. Reader and package identities
/// remain unavailable; neither case nor escape spelling is normalized.
/// Assembly's language-only fallback accepts complete MASM identifiers. Other
/// reviewed source labels require [`structural_captured_name_for_fact`]; this
/// lexical fallback does not establish a dialect or assembled binding.
/// GLSL borrows complete ASCII identifiers; native capture context, shader
/// reserved words and linked targets remain the caller's separate evidence.
/// HLSL likewise borrows complete ASCII spelling without reserved-word or
/// shader-binding proof; native callers distinguish names from buffer labels.
/// Scheme borrows complete written default-reader symbols without decoding
/// escapes or applying reader directives. Native proof owns Unicode categories
/// and the symbol's source context; bars are not Common Lisp token fragments.
/// WGSL borrows complete Unicode 16 XID names without normalization or folding;
/// native context owns reserved words, shader validity and binding evidence.
/// XML and DTD borrow complete XML 1.0 fifth-edition Names without resolving
/// QName prefixes, namespaces or expanded entities.
/// OCaml profiles borrow raw bounded identifier spelling; native capture owns
/// letter-category, keyword and declaration-context proof. Paths use exact fact labels.
/// Cap'n Proto declaration names preserve bounded ASCII spelling and case;
/// qualified type-use paths are admitted separately by the fact classifier.
/// VHDL folds ASCII basic identifiers to lowercase and preserves delimited extended
/// spelling as a distinct exact identity; native context owns declaration proof.
/// Other languages retain the shared borrowed-name contract. Source and canonical
/// output must both fit `maximum_bytes`. Invalid names, excess bytes or allocation
/// failure return `None`.
#[must_use]
pub fn structural_captured_name_for_language<'a>(
    language: &str,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if language == "hcl" {
        return hcl::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "ada" {
        return ada::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "json-schema" {
        return json_schema::captured_body(text, maximum_bytes);
    }
    if language == "dockerfile" {
        return dockerfile::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "wolfram" {
        return wolfram::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "sed" {
        return sed::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "idris" {
        return idris::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "lean" {
        return lean::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "pug" {
        return pug::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "coffeescript" {
        return coffeescript::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "meson" {
        return meson::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "gdscript" {
        return gdscript::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "qsharp" {
        return qsharp::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "chapel" {
        return chapel::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if matches!(language, "standard-ml" | "webassembly") {
        return sml_wat::captured_name(language, text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "mojo" {
        return mojo::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "haxe" {
        return haxe::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "qml" {
        return qml::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "hare" {
        return hare::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "ring" {
        return ring::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "modelica" {
        return modelica::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "d" {
        return d::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "zig" {
        return zig::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "crystal" {
        return crystal::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "cue" {
        return cue::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "pony" {
        return pony::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "fish" {
        return fish::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "nim" {
        return nim::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "clojure" {
        return clojure::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "racket" {
        return racket::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "vhdl" {
        return vhdl::captured_name(text, maximum_bytes);
    }
    if language == "graphql" {
        return (text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if language == "elixir" {
        return elixir::captured_name(text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "protobuf" {
        protobuf::captured_name(text, maximum_bytes)
    } else if language == "thrift" {
        thrift::captured_name(text, maximum_bytes)
    } else if language == "capnp" {
        (text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text))
    } else if language == "coq" {
        coq::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "v" {
        v::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "assembly" {
        assembly::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if matches!(language, "ocaml" | "ocaml-interface") {
        ocaml::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "glsl" {
        glsl::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "julia" {
        julia::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "cmake" {
        cmake::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if matches!(language, "verilog" | "systemverilog") {
        hdl::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "eiffel" {
        eiffel::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if matches!(language, "xml" | "dtd") {
        xml::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "hlsl" {
        hlsl::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "scheme" {
        scheme::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "wgsl" {
        wgsl::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "fortran" {
        (text.len() <= maximum_bytes
            && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text))
    } else if language == "plsql" {
        plsql_captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "sas" {
        sas_captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "pascal" {
        pascal_captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "common-lisp" {
        common_lisp::captured_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if matches!(language, "javascript" | "typescript") {
        crate::canonical_ecmascript_identifier(text, maximum_bytes)
            .or_else(|| structural_captured_name(text, maximum_bytes).map(Cow::Borrowed))
    } else if language == "groovy" {
        crate::groovy_names::canonical_groovy_name(text, maximum_bytes)
    } else if language == "haskell" {
        (!text.is_empty()
            && text.len() <= maximum_bytes
            && text.chars().all(|ch| {
                !ch.is_control()
                    && !ch.is_whitespace()
                    && !matches!(ch, '(' | ')' | '{' | '}' | '[' | ']' | '`' | '"')
            }))
        .then_some(Cow::Borrowed(text))
    } else if language == "scala" {
        let candidate = text.trim();
        if candidate.is_empty() || candidate.len() > maximum_bytes {
            return None;
        }
        if let Some(quoted) = candidate
            .strip_prefix('`')
            .and_then(|name| name.strip_suffix('`'))
        {
            return (!quoted.is_empty() && !quoted.chars().any(|ch| ch.is_control() || ch == '`'))
                .then_some(Cow::Borrowed(quoted));
        }
        candidate
            .chars()
            .all(|ch| {
                !ch.is_control()
                    && !ch.is_whitespace()
                    && !matches!(ch, '(' | ')' | '{' | '}' | '[' | ']' | '`')
            })
            .then_some(Cow::Borrowed(candidate))
    } else if language == "sql" {
        crate::sql_names::canonical_sql_name(text, maximum_bytes)
    } else if matches!(language, "json" | "jsonc") {
        crate::json_names::canonical_json_key(text, maximum_bytes)
    } else if language == "toml" {
        crate::toml_names::canonical_toml_key_path(text, maximum_bytes)
    } else if language == "yaml" {
        crate::yaml_names::canonical_flow_key(text, maximum_bytes).map(Cow::Owned)
    } else if language == "r" {
        crate::r_names::canonical_r_name(text, maximum_bytes)
    } else if language == "agda" {
        (!text.is_empty()
            && text.len() <= maximum_bytes
            && text.chars().all(|character| {
                !character.is_control()
                    && !character.is_whitespace()
                    && !matches!(character, ';' | '{' | '}' | '(' | ')' | '@' | '"')
            }))
        .then_some(Cow::Borrowed(text))
    } else if language == "tcl" {
        (!text.is_empty()
            && text.len() <= maximum_bytes
            && text.chars().all(|character| {
                !character.is_control()
                    && !character.is_whitespace()
                    && !matches!(character, '\\' | '$' | '[' | ']' | '{' | '}' | ';' | '"')
            }))
        .then_some(Cow::Borrowed(text))
    } else if matches!(language, "css" | "html" | "powershell" | "markdown" | "nix") {
        (!text.is_empty() && text.len() <= maximum_bytes && !text.contains('\0'))
            .then_some(Cow::Borrowed(text))
    } else if language == "lua" {
        crate::lua_names::canonical_lua_name(text, maximum_bytes)
    } else if language == "dart" {
        crate::dart_names::canonical_dart_name(text, maximum_bytes)
    } else if language == "ruby" {
        let candidate = text.trim();
        let operator = candidate
            .rsplit_once('.')
            .map_or(candidate, |(_, name)| name);
        let static_receiver = candidate.rsplit_once('.').is_none_or(|(receiver, _)| {
            structural_captured_name(receiver, maximum_bytes) == Some(receiver)
        });
        if candidate.len() <= maximum_bytes
            && static_receiver
            && matches!(operator, "/" | "[]" | "[]=")
        {
            Some(Cow::Borrowed(candidate))
        } else {
            structural_captured_name(candidate, maximum_bytes).map(Cow::Borrowed)
        }
    } else {
        structural_captured_name(text, maximum_bytes).map(Cow::Borrowed)
    }
}

fn pascal_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    text.split('.')
        .all(|part| {
            let identifier = part.strip_prefix('&').unwrap_or(part);
            identifier
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .then_some(text)
}

fn sas_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    let mut components = text.split('.');
    let valid = |part: &str| {
        part.as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    let first = components.next()?;
    (valid(first) && components.next().is_none_or(valid) && components.next().is_none())
        .then_some(text)
}

fn plsql_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes || text.contains('\0') {
        return None;
    }
    if let Some(quoted) = text
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
    {
        if quoted.is_empty() {
            return None;
        }
        let mut characters = quoted.chars();
        while let Some(character) = characters.next() {
            if character == '"' && characters.next() != Some('"') {
                return None;
            }
        }
        return Some(text);
    }
    (text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'#')))
    .then_some(text)
}

/// Interprets a reviewed name capture with its structural role and language.
///
/// YAML anchor definitions and alias references are serialization names, not
/// scalar values: `true`, `11` and quote characters retain their exact spelling.
/// Their captures exclude the `&` or `*` indicator. No trimming, escape decoding
/// or Unicode normalization occurs; malformed or oversized names return `None`.
/// Groovy qualified identifiers remove inter-component spaces and block comments
/// without changing component spelling. VBA identifiers remove enclosing brackets
/// or a trailing type character, retaining original case and source token spans.
/// This spelling projection does not establish case-insensitive binding identity.
/// SCSS style-rule selectors retain authored lists and whitespace without
/// claiming that interpolation has been expanded.
/// COBOL names preserve written ASCII word or numeric-header spelling; quoted
/// program names remain unsupported rather than being decoded into bindings.
/// F# definition occurrences retain exact quoted, qualified and operator source
/// spelling only for reviewed capture labels in `fsharp` or `fsharp-signature`.
/// All source bytes, including delimiters and spaces, count against the limit;
/// this does not infer resolved name equivalence or admit other capture roles.
/// Common Lisp admits only `common_lisp.name.definition` occurrence facts in
/// `common-lisp`. Exact written spelling is preserved, and other labels, roles
/// or languages cannot acquire a shared-name fallback for that namespace.
/// Assembly likewise requires exact definition labels, separating MASM names
/// from reviewed native label/constant spelling. Numeric directional labels and
/// assembler binding are not inferred; the original capture remains caller-owned.
/// GLSL admits only exact reviewed definition and unresolved source-use labels;
/// other roles or foreign languages cannot acquire a name through its namespace.
/// HLSL uses its own exact occurrence labels with the same namespace fence;
/// buffer labels and contexts are not admitted as definition or reference names.
/// Scheme admits exact `scheme.name.definition`, `scheme.symbol.reference` and
/// `scheme.direct_file_call.reference` occurrences in `scheme`, preserving raw
/// spelling without reader equivalence or generic call-target inference.
/// Unknown labels, other roles and foreign languages never use a generic fallback.
/// WGSL uses exact definition and unresolved-use labels in its own namespace;
/// accepted source spelling does not establish shader or call-target bindings.
/// XML and DTD require exact occurrence labels in their own namespaces and retain
/// raw Names without inferring namespace or entity-expansion bindings.
/// OCaml separates implementation and interface namespaces. Written value paths
/// retain all qualifiers without inferring module or application targets.
/// Other roles use [`structural_captured_name_for_language`]. The caller retains
/// the original source span and validates its association with its declaration.
#[must_use]
pub fn structural_captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    let label = fact.syntax_kind().as_str();
    if label.starts_with("python.module_read") {
        return (language == "python"
            && fact.kind() == SyntaxFactKind::Occurrence
            && label == "python.module_read.reference"
            && !text.is_empty()
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("thinkscript.") {
        if language != "thinkscript"
            || fact.kind() != SyntaxFactKind::Occurrence
            || !matches!(
                label,
                "thinkscript.script.definition"
                    | "thinkscript.input.definition"
                    | "thinkscript.input_value.read"
            )
            || text.is_empty()
            || text.len() > maximum_bytes
            || !(text.as_bytes()[0].is_ascii_alphabetic() || text.as_bytes()[0] == b'_')
        {
            return None;
        }
        return text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("agda.rhs") {
        if language != "agda"
            || fact.kind() != SyntaxFactKind::Occurrence
            || label != "agda.rhs.reference"
            || text.is_empty()
            || text.len() > maximum_bytes
            || !text.as_bytes()[0].is_ascii_alphabetic()
        {
            return None;
        }
        return text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'%' | b'\''))
            .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("powershell.formal_") || label.starts_with("powershell.unproven_formal") {
        if language != "powershell"
            || fact.kind() != SyntaxFactKind::Occurrence
            || !matches!(
                label,
                "powershell.formal_value.reference" | "powershell.unproven_formal.reference"
            )
            || text.is_empty()
            || text.len() > maximum_bytes
        {
            return None;
        }
        if text.contains('\0') {
            return None;
        }
        let ordinary = text.strip_prefix('$').is_some_and(|name| {
            name.as_bytes()
                .first()
                .is_some_and(|first| *first == b'_' || first.is_ascii_alphabetic())
                && name
                    .bytes()
                    .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        });
        return (label == "powershell.unproven_formal.reference" || ordinary)
            .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("java.local_receiver") || label.starts_with("java.unproven_local_receiver")
    {
        return (language == "java"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "java.local_receiver.reference" | "java.unproven_local_receiver.reference"
            )
            && text != "_"
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("cpp.const_formal_") || label.starts_with("cpp.unproven_const_formal") {
        return (language == "cpp"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "cpp.const_formal_receiver.reference" | "cpp.unproven_const_formal.reference"
            )
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("php.this_method") || label.starts_with("php.unproven_this_method") {
        return (language == "php"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "php.this_method.reference"
                    | "php.unproven_this_method.reference"
                    | "php.unproven_this_method.call_name"
            )
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("go.direct_file_function")
        || label.starts_with("go.unproven_direct_function")
        || label.starts_with("go.function_value")
        || label.starts_with("go.package_function_candidate")
    {
        return (language == "go"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "go.direct_file_function.reference"
                    | "go.function_value.reference"
                    | "go.package_function_candidate.reference"
                    | "go.unproven_direct_function.reference"
                    | "go.unproven_direct_function.call_name"
            )
            && text.len() <= maximum_bytes
            && text != "_"
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("go.call_formal") || label.starts_with("go.unproven_call_formal") {
        return (language == "go"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "go.call_formal.reference" | "go.unproven_call_formal.reference"
            )
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("c.written_variable")
        || label.starts_with("c.unproven_variable")
        || label.starts_with("c.variable_name")
    {
        return (language == "c"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "c.written_variable.reference"
                    | "c.unproven_variable.reference"
                    | "c.variable_name.definition"
            )
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("c.formal_") || label.starts_with("c.unproven_formal_") {
        return (language == "c"
            && fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                label,
                "c.formal_value.reference" | "c.unproven_formal_value.reference"
            )
            && text.len() <= maximum_bytes
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if label.starts_with("prolog.direct_goal.") || label.starts_with("prolog.goal_") {
        return (language == "prolog"
            && fact.kind() == SyntaxFactKind::Occurrence
            && label == "prolog.direct_goal.reference"
            && text.len() <= maximum_bytes
            && text.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(Cow::Borrowed(text));
    }
    if jsx::owns(label) {
        return jsx::captured_name(language, fact, text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "hcl" || label.starts_with("hcl.") {
        return (language == "hcl")
            .then(|| hcl::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "ada" || label.starts_with("ada.") {
        return (language == "ada")
            .then(|| ada::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "json-schema" || label.starts_with("json_schema.") {
        return (language == "json-schema")
            .then(|| json_schema::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten();
    }
    if language == "flux" || label.starts_with("flux.") {
        return (language == "flux")
            .then(|| flux::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "less" || label.starts_with("less.") {
        return (language == "less")
            .then(|| less::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "sass" || label.starts_with("sass.") {
        return (language == "sass")
            .then(|| sass::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "chapel" || label.starts_with("chapel.") {
        return (language == "chapel")
            .then(|| chapel::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if matches!(language, "standard-ml" | "webassembly")
        || label.starts_with("sml.")
        || label.starts_with("wat.")
    {
        return sml_wat::captured_name_for_fact(language, fact, text, maximum_bytes)
            .map(Cow::Borrowed);
    }
    if language == "dockerfile" || label.starts_with("dockerfile.") {
        return (language == "dockerfile")
            .then(|| dockerfile::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "wolfram" || label.starts_with("wolfram.") {
        return (language == "wolfram")
            .then(|| wolfram::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "idris" || label.starts_with("idris.") {
        return (language == "idris")
            .then(|| idris::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "sed" || label.starts_with("sed.") {
        return (language == "sed")
            .then(|| sed::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "lean" || label.starts_with("lean.") {
        return (language == "lean")
            .then(|| lean::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "pug" || label.starts_with("pug.") {
        return (language == "pug")
            .then(|| pug::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "coffeescript" || label.starts_with("coffeescript.") {
        return (language == "coffeescript")
            .then(|| coffeescript::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "meson" || label.starts_with("meson.") {
        return (language == "meson")
            .then(|| meson::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "gdscript" || label.starts_with("gdscript.") {
        return (language == "gdscript")
            .then(|| gdscript::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "qsharp" || label.starts_with("qsharp.") {
        return (language == "qsharp")
            .then(|| qsharp::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "mojo" || label.starts_with("mojo.") {
        return (language == "mojo")
            .then(|| mojo::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "haxe" || label.starts_with("haxe.") {
        return (language == "haxe")
            .then(|| haxe::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "qml" || label.starts_with("qml.") {
        return (language == "qml")
            .then(|| qml::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "hare" || label.starts_with("hare.") {
        return (language == "hare")
            .then(|| hare::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "ring" || label.starts_with("ring.") {
        return (language == "ring")
            .then(|| ring::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "modelica" || label.starts_with("modelica.") {
        return (language == "modelica")
            .then(|| modelica::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "d" || label.starts_with("d.") {
        return (language == "d")
            .then(|| d::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "zig" || label.starts_with("zig.") {
        return (language == "zig")
            .then(|| zig::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "crystal" || label.starts_with("crystal.") {
        return (language == "crystal")
            .then(|| crystal::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "cue" || label.starts_with("cue.") {
        return (language == "cue")
            .then(|| cue::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "pony" || label.starts_with("pony.") {
        return (language == "pony")
            .then(|| pony::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "fish" || label.starts_with("fish.") {
        return (language == "fish")
            .then(|| fish::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "nim" || label.starts_with("nim.") {
        return (language == "nim")
            .then(|| nim::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "clojure" || label.starts_with("clojure.") {
        return (language == "clojure")
            .then(|| clojure::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "racket" || label.starts_with("racket.") {
        return (language == "racket")
            .then(|| racket::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "vhdl" || label.starts_with("vhdl.") {
        return (language == "vhdl"
            && fact.kind() == SyntaxFactKind::Occurrence
            && label == "vhdl.name.definition")
            .then(|| vhdl::captured_name(text, maximum_bytes))
            .flatten();
    }
    if label.starts_with("capnp.") {
        return match (fact.kind(), label) {
            (SyntaxFactKind::Occurrence, "capnp.name.definition") => {
                structural_captured_name_for_language(language, text, maximum_bytes)
                    .filter(|_| language == "capnp")
            }
            (
                SyntaxFactKind::Occurrence,
                "capnp.named_type.reference"
                | "capnp.unavailable_type.reference"
                | "capnp.written_name.reference",
            ) if language == "capnp" => (text.len() <= maximum_bytes
                && text.split('.').all(|part| {
                    structural_captured_name_for_language("capnp", part, maximum_bytes).is_some()
                }))
            .then_some(Cow::Borrowed(text)),
            _ => None,
        };
    }
    if label.starts_with("patch.") {
        return (language == "diff"
            && fact.kind() == SyntaxFactKind::Occurrence
            && label == "patch.heading.definition"
            && !text.is_empty()
            && text.len() <= maximum_bytes
            && !text.contains(['\0', '\r', '\n']))
        .then_some(Cow::Borrowed(text));
    }
    if language == "elixir" || label.starts_with("elixir.") {
        return (language == "elixir"
            && fact.kind() == SyntaxFactKind::Occurrence
            && label == "elixir.name.definition")
            .then(|| elixir::captured_name(text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "protobuf" || label.starts_with("protobuf.") {
        return (language == "protobuf")
            .then(|| protobuf::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten();
    }
    if language == "thrift" || label.starts_with("thrift.") {
        return (language == "thrift")
            .then(|| thrift::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten();
    }
    if language == "coq" || label.starts_with("coq.") {
        return (language == "coq")
            .then(|| coq::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "rust" && label.starts_with("rust.") && fact.kind() == SyntaxFactKind::Occurrence
    {
        return crate::canonical_rust_identifier(text, maximum_bytes);
    }
    if language == "v" || label.starts_with("v.") {
        return (language == "v")
            .then(|| v::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if matches!(language, "verilog" | "systemverilog")
        || label.starts_with("verilog.")
        || label.starts_with("systemverilog.")
    {
        return label
            .strip_prefix(language)
            .filter(|suffix| suffix.starts_with('.'))
            .and_then(|_| hdl::captured_name_for_fact(fact, text, maximum_bytes))
            .map(Cow::Borrowed);
    }
    if language == "eiffel" || fact.syntax_kind().as_str().starts_with("eiffel.") {
        return (language == "eiffel")
            .then(|| eiffel::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if matches!(language, "ocaml" | "ocaml-interface")
        || fact.syntax_kind().as_str().starts_with("ocaml.")
        || fact.syntax_kind().as_str().starts_with("ocaml_interface.")
    {
        return ocaml::captured_name_for_fact(language, fact, text, maximum_bytes)
            .map(Cow::Borrowed);
    }
    if language == "julia" || fact.syntax_kind().as_str().starts_with("julia.") {
        return (language == "julia")
            .then(|| julia::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten();
    }
    if language == "cmake" || fact.syntax_kind().as_str().starts_with("cmake.") {
        return (language == "cmake")
            .then(|| cmake::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if matches!(language, "xml" | "dtd")
        || fact.syntax_kind().as_str().starts_with("xml.")
        || fact.syntax_kind().as_str().starts_with("dtd.")
    {
        return xml::captured_name_for_fact(language, fact, text, maximum_bytes).map(Cow::Borrowed);
    }
    if language == "scheme" || fact.syntax_kind().as_str().starts_with("scheme.") {
        return (language == "scheme")
            .then(|| scheme::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "wgsl" || fact.syntax_kind().as_str().starts_with("wgsl.") {
        return (language == "wgsl")
            .then(|| wgsl::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "hlsl" || fact.syntax_kind().as_str().starts_with("hlsl.") {
        return (language == "hlsl")
            .then(|| hlsl::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "glsl" || fact.syntax_kind().as_str().starts_with("glsl.") {
        return (language == "glsl")
            .then(|| glsl::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "assembly" || fact.syntax_kind().as_str().starts_with("assembly.") {
        return (language == "assembly" && fact.kind() == SyntaxFactKind::Occurrence)
            .then(|| assembly::captured_name_for_fact(fact, text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if language == "common-lisp" || fact.syntax_kind().as_str().starts_with("common_lisp.") {
        return (language == "common-lisp"
            && fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str() == "common_lisp.name.definition")
            .then(|| common_lisp::captured_name(text, maximum_bytes))
            .flatten()
            .map(Cow::Borrowed);
    }
    if matches!(language, "fsharp" | "fsharp-signature") {
        if fact.kind() != SyntaxFactKind::Occurrence {
            return None;
        }
        return fsharp::captured_name(language, fact.syntax_kind().as_str(), text, maximum_bytes);
    }
    if matches!(language, "cobol-fixed" | "cobol-free") {
        return (!text.is_empty()
            && text.len() <= maximum_bytes
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
        .then_some(Cow::Borrowed(text));
    }
    if language == "ini"
        && matches!(
            fact.syntax_kind().as_str(),
            "ini.section.definition" | "ini.key.definition"
        )
    {
        (!text.is_empty()
            && text.len() <= maximum_bytes
            && text.chars().all(|character| {
                !character.is_control()
                    && !matches!(
                        character,
                        '[' | ']' | '=' | ';' | '#' | '%' | '$' | '{' | '}'
                    )
            }))
        .then_some(Cow::Borrowed(text))
    } else if language == "make"
        && matches!(
            fact.syntax_kind().as_str(),
            "make.name.definition" | "make.target.reference" | "make.variable.reference"
        )
    {
        let bytes = text.as_bytes();
        (bytes.len() <= maximum_bytes
            && bytes
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
            && bytes.iter().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
            }))
        .then_some(Cow::Borrowed(text))
    } else if language == "scss"
        && matches!(
            fact.syntax_kind().as_str(),
            "scss.selector.definition" | "scss.selector.signature"
        )
    {
        (!text.is_empty() && text.len() <= maximum_bytes && !text.contains('\0'))
            .then_some(Cow::Borrowed(text))
    } else if language == "erlang"
        && fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "erlang.function.scoped_call"
    {
        crate::erlang_names::canonical_qualified_function_call(text, maximum_bytes).map(Cow::Owned)
    } else if language == "erlang"
        && matches!(
            fact.syntax_kind().as_str(),
            "erlang.function.definition" | "erlang.function.call"
        )
    {
        crate::erlang_names::canonical_function_head(text, maximum_bytes).map(Cow::Owned)
    } else if language == "erlang" && fact.syntax_kind().as_str() == "erlang.module.definition" {
        crate::erlang_names::canonical_atom(text, maximum_bytes).map(Cow::Owned)
    } else if language == "prolog" && fact.syntax_kind().as_str() == "prolog.predicate.definition" {
        crate::prolog_names::canonical_predicate_head(text, maximum_bytes).map(Cow::Owned)
    } else if language == "prolog" && fact.syntax_kind().as_str() == "prolog.nonterminal.definition"
    {
        crate::prolog_names::canonical_nonterminal_head(text, maximum_bytes).map(Cow::Owned)
    } else if matches!(language, "vba" | "visual-basic-classic")
        && fact.syntax_kind().as_str() == "vba.module_name.definition"
    {
        (text.len() <= maximum_bytes)
            .then(|| vba_module_name(text))
            .flatten()
            .map(Cow::Borrowed)
    } else if matches!(language, "vba" | "visual-basic-classic")
        && matches!(
            fact.syntax_kind().as_str(),
            "vba.identifier.definition"
                | "vba.identifier.reference"
                | "vba.value_identifier.reference"
                | "vba.let_identifier.reference"
                | "vba.set_identifier.reference"
                | "vba.invoked_get_identifier.reference"
                | "vba.invoked_let_identifier.reference"
                | "vba.invoked_set_identifier.reference"
                | "vba.default_constant_identifier.reference"
                | "vba.compilation_identifier.reference"
                | "vba.direct_type_identifier.reference"
                | "vba.constructed_type_identifier.reference"
                | "vba.tested_type_identifier.reference"
                | "vba.procedure_pointer.reference"
                | "vba.type_identifier.reference"
                | "vba.invoked_identifier.reference"
                | "vba.self_member.reference"
                | "vba.self_invoked_member.reference"
                | "vba.self_get_member.reference"
                | "vba.self_let_member.reference"
                | "vba.self_set_member.reference"
                | "vba.self_invoked_get_member.reference"
                | "vba.self_invoked_let_member.reference"
                | "vba.self_invoked_set_member.reference"
        )
    {
        if text.is_empty() || text.len() > maximum_bytes {
            return None;
        }
        if let Some(name) = text
            .strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
        {
            return (!name.is_empty()
                && !name
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '[' | ']')))
            .then_some(Cow::Borrowed(name));
        }
        let name = text
            .strip_suffix(['$', '%', '&', '#', '@', '^', '!'])
            .unwrap_or(text);
        structural_captured_name(name, maximum_bytes).map(Cow::Borrowed)
    } else if language == "yaml"
        && fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "yaml.anchor.definition" | "yaml.alias.reference"
        )
    {
        crate::yaml_names::anchor_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "groovy"
        && matches!(
            fact.syntax_kind().as_str(),
            "groovy.qualified_identifier.definition"
                | "groovy.qualified_type.reference"
                | "groovy.extends_type.reference"
                | "groovy.implements_type.reference"
                | "groovy.type_import.reference"
                | "groovy.package_star_import.reference"
                | "groovy.static_import.reference"
                | "groovy.static_star_import.reference"
        )
    {
        crate::groovy_names::canonical_qualified_name(text, maximum_bytes)
    } else {
        structural_captured_name_for_language(language, text, maximum_bytes)
    }
}

/// Returns the unchanged Latin name from a quoted VBA module-header value.
///
/// MS-VBAL section 4.2 limits module names to 31 characters. This projection
/// accepts ASCII letters followed by ASCII letters, digits or underscores;
/// other code-page-dependent names remain unproven, not normalized or replaced.
/// Invalid, unquoted or unsupported values return `None`. Callers preserve the
/// original quoted source span and account for unavailable identities separately.
#[must_use]
pub fn vba_module_name(text: &str) -> Option<&str> {
    let name = text.strip_prefix('"')?.strip_suffix('"')?;
    (name.len() <= 31
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(name)
}

/// Returns readable display text for a canonical structural identity.
///
/// JSON scalar keys omit their enclosing quotes and decode quote/backslash
/// escapes. Empty, boundary-whitespace, control-bearing and unpaired-UTF-16 keys
/// retain the canonical quoted identity. TOML uses the same display rule for a
/// single segment; multi-segment paths stay quoted to preserve boundaries.
/// YAML string identities use the same readable-string rule; other scalar types
/// keep their type prefix so a boolean or number cannot masquerade as a string.
/// PowerShell literal keys decode quoting and escapes for readable display only;
/// their canonical identity remains the exact written spelling. Unreadable or
/// interpolated strings retain that spelling instead of inventing a value.
/// Nix static keys decode escapes for display, and paths omit trivia while
/// retaining quoted nonidentifier segments. A dotted single key stays quoted;
/// ambiguous or expanding display text retains the original written name.
/// Groovy's quoted exceptional identities use the same readable-string rule;
/// controls, empty names and unpaired UTF-16 units remain visibly escaped.
/// Other languages are unchanged. The input must already be a
/// bounded canonical name from [`structural_captured_name_for_language`]; display
/// text is no longer than that input and must never replace the durable identity.
#[must_use]
pub fn structural_display_name_for_language<'a>(
    language: &str,
    canonical: &'a str,
) -> Cow<'a, str> {
    if language == "powershell" {
        crate::powershell_names::display_key(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if language == "nix" {
        crate::nix_names::display_path(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if matches!(language, "json" | "jsonc" | "toml")
        || (language == "groovy" && canonical.starts_with('"'))
    {
        crate::json_names::display_json_key(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if language == "yaml" {
        canonical
            .strip_prefix("str:")
            .and_then(crate::json_names::display_json_key)
            .unwrap_or(Cow::Borrowed(canonical))
    } else {
        Cow::Borrowed(canonical)
    }
}

/// Orders syntax facts exactly as structural declaration association consumes them.
///
/// Whole-project analyzers must use this order before resolving nearest
/// declarations. Parser-local identifiers are intentionally excluded because
/// their assignment order is not part of the structural identity contract.
#[must_use]
pub fn structural_syntax_fact_order(left: &SyntaxFact, right: &SyntaxFact) -> Ordering {
    (
        left.depth(),
        left.span().start_byte(),
        left.span().end_byte(),
        syntax_fact_kind_tag(left.kind()),
        left.syntax_kind().as_str(),
    )
        .cmp(&(
            right.depth(),
            right.span().start_byte(),
            right.span().end_byte(),
            syntax_fact_kind_tag(right.kind()),
            right.syntax_kind().as_str(),
        ))
}

const fn syntax_fact_kind_tag(kind: SyntaxFactKind) -> u8 {
    match kind {
        SyntaxFactKind::Root => 1,
        SyntaxFactKind::Module => 2,
        SyntaxFactKind::Declaration => 3,
        SyntaxFactKind::Signature => 4,
        SyntaxFactKind::Import => 5,
        SyntaxFactKind::Scope => 6,
        SyntaxFactKind::Occurrence => 7,
        SyntaxFactKind::Comment => 8,
        SyntaxFactKind::StringLiteral => 9,
        SyntaxFactKind::EmbeddedRegion => 10,
        SyntaxFactKind::ErrorRecovery => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_family_preprocessor_context_never_replaces_written_headers() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 32).unwrap();
        let fact = |kind, label| {
            SyntaxFact::new(1, None, kind, span, 0, SyntaxKindLabel::new(label).unwrap())
        };
        for label in [
            "c.preprocessor_condition.signature",
            "cpp.preprocessor_condition.signature",
            "c.preprocessor_lexical_unavailable.signature",
            "cpp.preprocessor_lexical_unavailable.signature",
            "cpp.preprocessor_parameters.signature",
            "c.preprocessor_projection_unavailable.signature",
            "cpp.preprocessor_projection_unavailable.signature",
        ] {
            assert!(structural_signature_is_preprocessor_context(&fact(
                SyntaxFactKind::Signature,
                label
            )));
            assert!(!structural_signature_is_preprocessor_context(&fact(
                SyntaxFactKind::Declaration,
                label
            )));
        }
        for label in [
            "c.parameters.signature",
            "cpp.parameters.signature",
            "c.written_macro.signature",
            "cpp.written_macro.signature",
            "python.parameters.signature",
        ] {
            assert!(!structural_signature_is_preprocessor_context(&fact(
                SyntaxFactKind::Signature,
                label
            )));
        }
    }

    #[test]
    fn patch_headings_are_document_sections_without_target_language_bindings() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 32).unwrap();
        for label in ["patch.file_section.declaration", "patch.hunk.declaration"] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_entity_kind(&fact),
                Some(EntityKind::DocumentSection)
            );
        }
        let heading = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            span,
            0,
            SyntaxKindLabel::new("patch.heading.definition").unwrap(),
        );
        for text in [
            "diff --git a/source b/source",
            "@@ -1 +1 @@",
            "--- a/name with spaces",
        ] {
            assert_eq!(
                structural_captured_name_for_fact("diff", &heading, text, text.len()).as_deref(),
                Some(text)
            );
            assert!(
                structural_captured_name_for_fact("diff", &heading, text, text.len() - 1).is_none()
            );
            assert!(
                structural_captured_name_for_fact("cmake", &heading, text, text.len()).is_none()
            );
        }
        for text in ["", "header\nbody", "header\rbody", "header\0body"] {
            assert!(structural_captured_name_for_fact("diff", &heading, text, 64).is_none());
        }
        for label in [
            "patch.function.declaration",
            "patch.variable.declaration",
            "patch.deleted_row.documentation",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), None);
        }
    }

    #[test]
    fn vba_class_and_type_member_kinds_use_reviewed_fact_labels() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, fact_kind, base, member) in [
            (
                "vba.class.module",
                SyntaxFactKind::Module,
                EntityKind::Class,
                EntityKind::Class,
            ),
            (
                "vba.file.module",
                SyntaxFactKind::Module,
                EntityKind::Module,
                EntityKind::Module,
            ),
            (
                "vba.variable.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
                EntityKind::Field,
            ),
            (
                "vba.function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Method,
            ),
            (
                "vba.sub_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Method,
            ),
            (
                "vba.external_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Function,
            ),
            (
                "vba.external_sub_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Function,
            ),
            (
                "vba.get_property.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Property,
                EntityKind::Property,
            ),
            (
                "vba.constant.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Constant,
                EntityKind::Constant,
            ),
            (
                "javascript.variable.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
                EntityKind::Variable,
            ),
            (
                "scss.style_rule.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::StyleRule,
                EntityKind::StyleRule,
            ),
            (
                "scss.keyframes.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Keyframes,
                EntityKind::Keyframes,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                fact_kind,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(base), "{label}");
            assert_eq!(structural_type_member_kind(&fact, base), member, "{label}");
        }
    }

    #[test]
    fn fortran_written_kinds_and_names_do_not_imply_binding_equivalence() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, kind) in [
            ("fortran.program.declaration", EntityKind::Module),
            ("fortran.function.declaration", EntityKind::Function),
            ("fortran.subroutine.declaration", EntityKind::Function),
            (
                "fortran.interface_function.declaration",
                EntityKind::Function,
            ),
            (
                "fortran.interface_subroutine.declaration",
                EntityKind::Function,
            ),
            (
                "fortran.separate_module_procedure.declaration",
                EntityKind::Function,
            ),
            ("fortran.derived_type.declaration", EntityKind::Struct),
            ("fortran.field.declaration", EntityKind::Field),
            ("fortran.parameter.declaration", EntityKind::Parameter),
            ("fortran.variable.declaration", EntityKind::Variable),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(kind), "{label}");
            if label.starts_with("fortran.interface_") {
                assert_eq!(structural_type_member_kind(&fact, kind), kind);
            }
        }
        for name in ["MixedName", "value_2"] {
            assert_eq!(
                structural_captured_name_for_language("fortran", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(
                structural_captured_name_for_language("fortran", name, name.len() - 1).is_none()
            );
        }
        for name in [
            "",
            "_value",
            "1value",
            "value$",
            "value_${KIND}$",
            "value member",
            "caf\u{e9}",
        ] {
            assert!(structural_captured_name_for_language("fortran", name, 64).is_none());
        }
    }

    #[test]
    fn plsql_source_kinds_and_names_do_not_infer_database_binding() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, kind) in [
            ("plsql.package.declaration", EntityKind::Module),
            ("plsql.package_body.declaration", EntityKind::Module),
            ("plsql.procedure.declaration", EntityKind::Function),
            ("plsql.function.declaration", EntityKind::Function),
            ("plsql.record_type.declaration", EntityKind::Struct),
            ("plsql.field.declaration", EntityKind::Field),
            ("plsql.parameter.declaration", EntityKind::Parameter),
            ("plsql.variable.declaration", EntityKind::Variable),
            ("plsql.trigger.declaration", EntityKind::DatabaseObject),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(kind), "{label}");
        }
        for name in [
            "MixedName",
            "Value_2$#",
            "\"Case Name\"",
            "\"a\"\"b\"",
            "\"π\"",
        ] {
            assert_eq!(
                structural_captured_name_for_language("plsql", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(structural_captured_name_for_language("plsql", name, name.len() - 1).is_none());
        }
        for name in [
            "",
            "_value",
            "1value",
            "value.member",
            "value()",
            "value member",
            "caf\u{e9}",
            "\"\"",
            "\"open",
            "\"a\"b\"",
            "\"a\0b\"",
            "`value`",
            "'value'",
        ] {
            assert!(
                structural_captured_name_for_language("plsql", name, 64).is_none(),
                "{name:?}"
            );
        }
        assert_ne!(
            structural_captured_name_for_language("plsql", "CaseName", 64),
            structural_captured_name_for_language("plsql", "casename", 64)
        );
        assert_ne!(
            structural_captured_name_for_language("plsql", "\"CaseName\"", 64),
            structural_captured_name_for_language("plsql", "CaseName", 64)
        );
    }

    #[test]
    fn sas_written_kinds_and_names_do_not_expand_macros_or_fold_case() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, kind) in [
            ("sas.fcmp_function.declaration", EntityKind::Function),
            ("sas.fcmp_subroutine.declaration", EntityKind::Function),
            ("sas.macro.declaration", EntityKind::Function),
            ("sas.fcmp_parameter.declaration", EntityKind::Parameter),
            ("sas.macro_parameter.declaration", EntityKind::Parameter),
            ("sas.data_target.declaration", EntityKind::DatabaseObject),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(kind), "{label}");
        }
        for name in ["MixedName", "_Value2", "Work.Output", "Output"] {
            assert_eq!(
                structural_captured_name_for_language("sas", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(structural_captured_name_for_language("sas", name, name.len() - 1).is_none());
        }
        for name in [
            "",
            "1Value",
            "Work.&Output.",
            "%Factory",
            "&Value",
            "Work.Output.More",
            "Work..Output",
            "Work.",
            ".Output",
            "Value$",
            "'Name'n",
            "Work . Output",
            "caf\u{e9}",
        ] {
            assert!(
                structural_captured_name_for_language("sas", name, 64).is_none(),
                "{name:?}"
            );
        }
        assert_ne!(
            structural_captured_name_for_language("sas", "Work.Output", 64),
            structural_captured_name_for_language("sas", "work.output", 64)
        );
    }

    #[test]
    fn pascal_written_kinds_and_names_do_not_bind_or_normalize_identifiers() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, kind) in [
            ("pascal.unit.declaration", EntityKind::Module),
            ("pascal.program.declaration", EntityKind::Module),
            ("pascal.library.declaration", EntityKind::Module),
            ("pascal.class.declaration", EntityKind::Class),
            ("pascal.interface.declaration", EntityKind::Interface),
            ("pascal.record.declaration", EntityKind::Struct),
            ("pascal.object.declaration", EntityKind::Struct),
            ("pascal.function.declaration", EntityKind::Function),
            ("pascal.procedure.declaration", EntityKind::Function),
            ("pascal.constructor.declaration", EntityKind::Constructor),
            ("pascal.destructor.declaration", EntityKind::Method),
            ("pascal.property.declaration", EntityKind::Property),
            ("pascal.field_fragment.declaration", EntityKind::Field),
            ("pascal.class_variable.declaration", EntityKind::Field),
            (
                "pascal.class_variable_fragment.declaration",
                EntityKind::Field,
            ),
            ("pascal.variable_fragment.declaration", EntityKind::Variable),
            (
                "pascal.parameter_fragment.declaration",
                EntityKind::Parameter,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(kind), "{label}");
        }
        for name in [
            "MixedName",
            "_Value2",
            "&type",
            "System.Types",
            "&Unit.&Type",
        ] {
            assert_eq!(
                structural_captured_name_for_language("pascal", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(
                structural_captured_name_for_language("pascal", name, name.len() - 1).is_none()
            );
        }
        for name in [
            "",
            "1Value",
            "&",
            "&&Value",
            "A..B",
            ".A",
            "A.",
            " A",
            "A ",
            "A . B",
            "A{comment}.B",
            "A<T>",
            "A/B",
            "A\0B",
            "caf\u{e9}",
        ] {
            assert!(
                structural_captured_name_for_language("pascal", name, 64).is_none(),
                "{name:?}"
            );
        }
        assert_ne!(
            structural_captured_name_for_language("pascal", "&Type", 64),
            structural_captured_name_for_language("pascal", "Type", 64)
        );
        assert_ne!(
            structural_captured_name_for_language("pascal", "Value", 64),
            structural_captured_name_for_language("pascal", "value", 64)
        );
    }

    #[test]
    fn cobol_written_declarations_and_names_preserve_source_spelling() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, expected) in [
            ("cobol.program.declaration", EntityKind::Module),
            ("cobol.data_item.declaration", EntityKind::Variable),
            ("cobol.file_descriptor.declaration", EntityKind::Variable),
            ("cobol.section.declaration", EntityKind::Function),
            ("cobol.paragraph.declaration", EntityKind::Function),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(expected), "{label}");
        }
        let definition = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new("cobol.program_name.definition").unwrap(),
        );
        for language in ["cobol-fixed", "cobol-free"] {
            for written in ["Mixed-Name", "1Entry", "100"] {
                assert_eq!(
                    structural_captured_name_for_fact(language, &definition, written, 64)
                        .as_deref(),
                    Some(written)
                );
            }
            for unsupported in [
                "",
                "'Quoted'",
                "\"Quoted\"",
                "two names",
                "name.member",
                "é",
                "x\0y",
            ] {
                assert!(
                    structural_captured_name_for_fact(language, &definition, unsupported, 64)
                        .is_none()
                );
            }
            assert!(
                structural_captured_name_for_fact(language, &definition, "Mixed-Name", 4).is_none()
            );
        }
    }

    #[test]
    fn haskell_declaration_kinds_do_not_infer_signature_callability() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (syntax, expected) in [
            ("haskell.module.declaration", Some(EntityKind::Module)),
            ("haskell.data_type.declaration", Some(EntityKind::Enum)),
            ("haskell.newtype.declaration", Some(EntityKind::Enum)),
            ("haskell.class.declaration", Some(EntityKind::Trait)),
            (
                "haskell.type_synonym.declaration",
                Some(EntityKind::TypeAlias),
            ),
            ("haskell.function.declaration", Some(EntityKind::Function)),
            ("haskell.variable.declaration", Some(EntityKind::Variable)),
            (
                "haskell.constructor.declaration",
                Some(EntityKind::Constructor),
            ),
            ("haskell.field.declaration", Some(EntityKind::Field)),
            ("haskell.parameter.declaration", Some(EntityKind::Parameter)),
            ("haskell.signature_name.declaration", None),
            ("haskell.signature_member_name.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(syntax).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), expected, "{syntax}");
        }
    }

    #[test]
    fn elm_port_declarations_retain_function_identity() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new("elm.port.declaration").unwrap(),
        );
        assert_eq!(structural_entity_kind(&fact), Some(EntityKind::Function));
    }

    #[test]
    fn capnp_methods_and_annotations_keep_distinct_kinds() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (kind, syntax, expected) in [
            (
                SyntaxFactKind::Declaration,
                "capnp.method.declaration",
                Some(EntityKind::Method),
            ),
            (
                SyntaxFactKind::Declaration,
                "capnp.annotation.declaration",
                Some(EntityKind::Function),
            ),
            (
                SyntaxFactKind::Declaration,
                "capnp.method.declaration.extra",
                None,
            ),
            (SyntaxFactKind::Occurrence, "capnp.method.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(syntax).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), expected, "{syntax}");
            if expected == Some(EntityKind::Method) {
                assert_eq!(
                    structural_type_member_kind(&fact, EntityKind::Method),
                    EntityKind::Method
                );
            }
        }
    }

    #[test]
    fn tcl_declarations_retain_namespace_function_and_parameter_identity() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (syntax, expected) in [
            ("tcl.namespace.declaration", EntityKind::Namespace),
            ("tcl.function.declaration", EntityKind::Function),
            ("tcl.parameter.declaration", EntityKind::Parameter),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(syntax).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(expected));
        }
    }

    #[test]
    fn tcl_literal_command_names_keep_source_spelling() {
        assert_eq!(
            structural_captured_name_for_language("tcl", "route/(edit)", 256).as_deref(),
            Some("route/(edit)")
        );
        for dynamic in ["$name", "[makeName]", "a\\b", "a b"] {
            assert!(structural_captured_name_for_language("tcl", dynamic, 256).is_none());
        }
    }

    #[test]
    fn scss_selector_capture_preserves_lists_without_loosening_identifiers() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        let selector = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            span,
            0,
            SyntaxKindLabel::new("scss.selector.definition").unwrap(),
        );
        let identifier = SyntaxFact::new(
            2,
            None,
            SyntaxFactKind::Occurrence,
            span,
            0,
            SyntaxKindLabel::new("scss.identifier.definition").unwrap(),
        );
        assert_eq!(
            structural_captured_name_for_fact("scss", &selector, ".card, .tile", 32),
            Some(Cow::Borrowed(".card, .tile"))
        );
        assert!(structural_captured_name_for_fact("scss", &selector, ".card, .tile", 4).is_none());
        assert!(structural_captured_name_for_fact("scss", &selector, "a\0b", 32).is_none());
        assert!(
            structural_captured_name_for_fact("scss", &identifier, ".card, .tile", 32).is_none()
        );
    }

    #[test]
    fn make_static_paths_keep_authored_identity_without_relaxing_other_names() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        for label in [
            "make.name.definition",
            "make.target.reference",
            "make.variable.reference",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("make", &fact, "src/main.c", 32).as_deref(),
                Some("src/main.c")
            );
            for name in ["/root", "src/$(NAME)", "src\\name", "src name"] {
                assert!(structural_captured_name_for_fact("make", &fact, name, 32).is_none());
            }
            assert!(structural_captured_name_for_fact("make", &fact, "src/main.c", 4).is_none());
            assert!(structural_captured_name_for_fact("rust", &fact, "src/main.c", 32).is_none());
        }
    }

    #[test]
    fn ini_names_keep_authored_spacing_with_explicit_capture_bounds() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        for label in ["ini.section.definition", "ini.key.definition"] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("ini", &fact, "Unit Name", 32).as_deref(),
                Some("Unit Name")
            );
            assert!(structural_captured_name_for_fact("ini", &fact, "Unit Name", 4).is_none());
            assert!(structural_captured_name_for_fact("ini", &fact, "bad\0name", 32).is_none());
            assert!(structural_captured_name_for_fact("ini", &fact, "bad=key", 32).is_none());
            assert!(structural_captured_name_for_fact("rust", &fact, "Unit Name", 32).is_none());
        }
    }

    #[test]
    fn vba_module_names_preserve_case_and_enforce_latin_identifier_bounds() {
        for name in ["A", "Metrics_2", "abcdefghijklmnopqrstuvwxyzABCDE"] {
            let quoted = format!("\"{name}\"");
            assert_eq!(vba_module_name(&quoted), Some(name));
        }
        for text in [
            "",
            "Metrics",
            "\"\"",
            "\"_Name\"",
            "\"9Name\"",
            "\"Name$\"",
            "\"A.B\"",
            "\"A B\"",
            "\"A\"\"B\"",
            "\"Δelta\"",
            "\"abcdefghijklmnopqrstuvwxyzABCDEF\"",
        ] {
            assert_eq!(vba_module_name(text), None, "{text}");
        }
    }

    #[test]
    fn nix_display_names_decode_static_paths_without_changing_authored_identity() {
        for (written, display) in [
            (r#""identity""#, "identity"),
            (r#""\identity""#, "identity"),
            (r#""λ😀""#, "λ😀"),
            (r#""two words""#, "two words"),
            (r#""\u0078""#, "u0078"),
            (r#"a."\b""#, "a.b"),
            ("a /* path trivia */ . b", "a.b"),
            ("a # path trivia\r\n . b", "a.b"),
            (r#""a.b""#, r#""a.b""#),
            (r#""a.b" . c"#, r#""a.b".c"#),
            (r#"a . "b c""#, r#"a."b c""#),
            (r#""""#, r#""""#),
            (r#""\n""#, r#""\n""#),
        ] {
            assert_eq!(
                structural_display_name_for_language("nix", written),
                display,
                "{written}"
            );
            assert_eq!(
                structural_captured_name_for_language("nix", written, written.len()).as_deref(),
                Some(written)
            );
            assert!(display.len() <= written.len());
        }
    }

    #[test]
    fn haskell_terminal_names_preserve_exact_bounded_spelling() {
        for source in ["name'", "Data.Map.lookup", "/", "/=", ":+", "λ", "⊕"] {
            assert_eq!(
                structural_captured_name_for_language("haskell", source, source.len()).as_deref(),
                Some(source)
            );
            assert!(
                structural_captured_name_for_language("haskell", source, source.len() - 1)
                    .is_none()
            );
        }
        for source in [
            "", "`name`", "(+)", "()", "[]", "a b", " name", "name\n", "\"name\"",
        ] {
            assert!(
                structural_captured_name_for_language("haskell", source, 64).is_none(),
                "{source:?}"
            );
        }
        assert!(structural_captured_name_for_language("rust", "/", 1).is_none());
    }

    #[test]
    fn scala_names_preserve_operators_and_bounded_backtick_identity() {
        for (source, canonical) in [
            ("name", "name"),
            ("`name`", "name"),
            ("`odd name`", "odd name"),
            ("/", "/"),
            ("\\", "\\"),
            ("λ", "λ"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("scala", source, source.len()).as_deref(),
                Some(canonical)
            );
            assert!(
                structural_captured_name_for_language("scala", source, source.len() - 1).is_none()
            );
        }
        for invalid in [
            "",
            "``",
            "`name",
            "name`",
            "`a`b`",
            "two names",
            "call()",
            "`line\nname`",
        ] {
            assert!(
                structural_captured_name_for_language("scala", invalid, 64).is_none(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn json_display_names_decode_only_readable_canonical_keys() {
        for (raw, display) in [
            (r#""name""#, "name"),
            (r#""\u006eame""#, "name"),
            (r#""\ud83e\udd80""#, "🦀"),
            (r#""a\"b""#, "a\"b"),
            (r#""a\\b""#, "a\\b"),
            (r#""""#, r#""""#),
            (r#""\u0000""#, r#""\u0000""#),
            (r#""\ud800""#, r#""\ud800""#),
            (r#"" ""#, r#"" ""#),
            (r#"" name ""#, r#"" name ""#),
            ("\"\u{2003}name\"", "\"\u{2003}name\""),
        ] {
            let canonical =
                structural_captured_name_for_language("json", raw, 64).expect("valid key");
            assert_eq!(
                structural_display_name_for_language("json", &canonical),
                display
            );
            assert_eq!(
                structural_display_name_for_language("rust", &canonical),
                canonical
            );
            assert!(display.len() <= canonical.len());
        }
    }

    #[test]
    fn ruby_operator_names_are_bounded_and_do_not_relax_other_languages() {
        for name in ["/", "[]", "[]=", "self.[]", "Store./"] {
            assert_eq!(
                structural_captured_name_for_language("ruby", name, 64),
                Some(Cow::Borrowed(name))
            );
            assert_eq!(
                structural_captured_name_for_language("ruby", name, name.len() - 1),
                None
            );
            assert_eq!(
                structural_captured_name_for_language("rust", name, 64),
                None
            );
        }
        for name in [
            "./",
            "factory().[]",
            "self.[value]",
            "[ ]",
            "Store .[]",
            "self.[]\0",
        ] {
            assert_eq!(
                structural_captured_name_for_language("ruby", name, 64),
                None,
                "{name:?}"
            );
        }
    }
    #[test]
    fn r_name_spellings_decode_without_changing_other_language_names() {
        for (source, expected) in [
            ("identity", "identity"),
            ("`identity`", "identity"),
            ("\"identity\"", "identity"),
            ("'identity'", "identity"),
            (r"`\x69dentity`", "identity"),
            (r#""\151dentity""#, "identity"),
            (r#""\u0069dentity""#, "identity"),
            (r#"r"(identity)""#, "identity"),
            ("`with spaces`", "with spaces"),
            (r#""\u03bb""#, "λ"),
            (r#""\xce\xbb""#, "λ"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("r", source, 128).as_deref(),
                Some(expected),
                "{source}"
            );
            assert!(structural_captured_name_for_language("r", source, source.len() - 1).is_none());
        }
        assert_eq!(
            structural_captured_name_for_language("html", "`identity`", 128).as_deref(),
            Some("`identity`")
        );
    }

    #[test]
    fn powershell_written_names_are_bounded_without_runtime_normalization() {
        for source in [
            "Read-Entry",
            "script:Read-Entry",
            "$Name",
            "${name with space}",
            "${雪}",
        ] {
            assert_eq!(
                structural_captured_name_for_language("powershell", source, source.len())
                    .as_deref(),
                Some(source)
            );
            assert!(
                structural_captured_name_for_language("powershell", source, source.len() - 1)
                    .is_none()
            );
        }
        for source in ["", "name\0tail"] {
            assert!(structural_captured_name_for_language("powershell", source, 128).is_none());
        }
        assert!(structural_captured_name_for_language("rust", "${name with space}", 128).is_none());
    }

    #[test]
    fn markdown_names_preserve_authored_spelling_with_exact_byte_limits() {
        for source in [
            "Storage *rules*",
            "[Guide]",
            "[guide]",
            "雪 &amp; `code`",
            "First\r\nsecond",
        ] {
            assert_eq!(
                structural_captured_name_for_language("markdown", source, source.len()),
                Some(Cow::Borrowed(source))
            );
            assert!(
                structural_captured_name_for_language("markdown", source, source.len() - 1)
                    .is_none()
            );
        }
        for source in ["", "head\0tail"] {
            assert!(structural_captured_name_for_language("markdown", source, 128).is_none());
        }
        assert!(structural_captured_name_for_language("rust", "Storage *rules*", 128).is_none());
    }

    #[test]
    fn wgsl_public_dispatch_preserves_exact_namespace_roles_and_raw_unicode_names() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 1)
                    .unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for (label, kind) in [
            ("wgsl.function.declaration", EntityKind::Function),
            ("wgsl.struct.declaration", EntityKind::Struct),
            ("wgsl.type_alias.declaration", EntityKind::TypeAlias),
            ("wgsl.constant.declaration", EntityKind::Constant),
            ("wgsl.override.declaration", EntityKind::Variable),
            ("wgsl.parameter.declaration", EntityKind::Parameter),
            ("wgsl.field.declaration", EntityKind::Field),
        ] {
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Occurrence, label)),
                None
            );
        }
        for label in [
            "wgsl.unknown.declaration",
            "wgsl.function.declaration.extra",
            "wgsl.shader_resource.declaration",
        ] {
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                None
            );
        }
        for raw in ["Δ", "é", "e\u{301}", "_value"] {
            assert_eq!(
                structural_captured_name_for_language("wgsl", raw, raw.len()),
                Some(Cow::Borrowed(raw))
            );
            assert_eq!(
                structural_captured_name_for_language("wgsl", raw, raw.len() - 1),
                None
            );
            for label in [
                "wgsl.name.definition",
                "wgsl.identifier.reference",
                "wgsl.type.reference",
                "wgsl.member.reference",
                "wgsl.unproven_call.reference",
            ] {
                let occurrence = fact(SyntaxFactKind::Occurrence, label);
                assert_eq!(
                    structural_captured_name_for_fact("wgsl", &occurrence, raw, raw.len()),
                    Some(Cow::Borrowed(raw))
                );
                assert_eq!(
                    structural_captured_name_for_fact("hlsl", &occurrence, raw, raw.len()),
                    None
                );
                assert_eq!(
                    structural_captured_name_for_fact(
                        "wgsl",
                        &fact(SyntaxFactKind::Signature, label),
                        raw,
                        raw.len()
                    ),
                    None
                );
            }
        }
        for raw in ["_", "name.member", " name", "name()", "\u{301}name"] {
            assert_eq!(
                structural_captured_name_for_language("wgsl", raw, usize::MAX),
                None
            );
        }
        assert_eq!(
            structural_captured_name_for_fact(
                "wgsl",
                &fact(SyntaxFactKind::Occurrence, "hlsl.name.definition"),
                "Name",
                16
            ),
            None
        );
        assert_eq!(
            structural_captured_name_for_fact(
                "wgsl",
                &fact(SyntaxFactKind::Occurrence, "wgsl.unknown.reference"),
                "Name",
                16
            ),
            None
        );
    }

    #[test]
    fn r_invalid_name_escapes_do_not_invent_a_canonical_symbol() {
        for source in [
            r"`\u0061`",
            r#""\x61\u0062""#,
            r#""\0""#,
            r#""\400""#,
            r#""\q""#,
            r#""\xff""#,
            r#""\uD800""#,
            "\"\"",
            "`unclosed",
            "`a` trailing",
        ] {
            assert!(
                structural_captured_name_for_language("r", source, 128).is_none(),
                "{source}"
            );
        }
    }
    #[test]
    fn groovy_quoted_names_preserve_constant_identity() {
        for (source, expected) in [
            ("'with space'", "with space"),
            ("\"double space\"", "double space"),
            ("'''triple space'''", "triple space"),
            ("\"\"\"triple double\"\"\"", "triple double"),
            ("/slash name/", "slash name"),
            ("$/dollar name/$", "dollar name"),
            (r"'uni\u0063ode'", "unicode"),
            (r"'oct\141l'", "octal"),
            (r"'space\sname'", "space name"),
            (r"'quote\'name'", "quote'name"),
            (r"'slash\\name'", r"slash\name"),
            (r"'surrogate\uD83D\uDE80'", "surrogate🚀"),
            (r"'line\nname'", r#""line\u000aname""#),
            (r"'lone\uD800'", r#""lone\ud800""#),
            ("''", "\"\""),
            (r"'${name}'", "${name}"),
            (r#""dollar\$name""#, "dollar$name"),
            (r"/literal\n/", r"literal\n"),
            (r"/uni\u0061/", "unia"),
            (r"$/uni\u0061/$", "unia"),
            ("$/literal$$name/$", "literal$name"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("groovy", source, 128).as_deref(),
                Some(expected),
                "{source}"
            );
        }
    }
}

#[cfg(test)]
mod graphql_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn graphql_classifies_exact_declaration_roles_without_selection_guesses() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "graphql.object.declaration",
                Some(EntityKind::Struct),
            ),
            (
                SyntaxFactKind::Declaration,
                "graphql.field.declaration",
                Some(EntityKind::Field),
            ),
            (
                SyntaxFactKind::Declaration,
                "graphql.argument.declaration",
                Some(EntityKind::Parameter),
            ),
            (
                SyntaxFactKind::Occurrence,
                "graphql.field.declaration",
                None,
            ),
            (SyntaxFactKind::Declaration, "graphql.field.reference", None),
            (
                SyntaxFactKind::Declaration,
                "graphql.selection.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "graphql.object.declaration.extra",
                None,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), expected, "{label}");
        }
    }

    #[test]
    fn graphql_names_keep_ascii_case_and_obey_the_complete_byte_limit() {
        for name in ["Query", "viewer", "_private2"] {
            assert!(
                matches!(structural_captured_name_for_language("graphql", name, name.len()),
                Some(Cow::Borrowed(found)) if found == name)
            );
            assert!(
                structural_captured_name_for_language("graphql", name, name.len() - 1).is_none()
            );
        }
        for name in [
            "",
            "two names",
            "1leading",
            "Type.field",
            "@tag",
            "λ",
            "x\0y",
        ] {
            assert!(
                structural_captured_name_for_language("graphql", name, 64).is_none(),
                "{name:?}"
            );
        }
    }
}

#[cfg(test)]
mod prolog_goal_capture_tests {
    use super::*;

    #[test]
    fn direct_goal_names_require_the_exact_language_role_label_and_plain_atom() {
        let file = rootlight_ids::FileId::from_bytes([45; 20]);
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                rootlight_ir::SourceSpan::new(file, 0, 6).unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let goal = fact(SyntaxFactKind::Occurrence, "prolog.direct_goal.reference");
        assert_eq!(
            structural_captured_name_for_fact("prolog", &goal, "target", 6).as_deref(),
            Some("target")
        );
        for (language, site, text, maximum) in [
            ("scheme", goal.clone(), "target", 6),
            (
                "prolog",
                fact(SyntaxFactKind::Signature, "prolog.direct_goal.reference"),
                "target",
                6,
            ),
            (
                "prolog",
                fact(SyntaxFactKind::Occurrence, "prolog.direct_goal.extra"),
                "target",
                6,
            ),
            ("prolog", goal.clone(), "target(X)", 64),
            ("prolog", goal.clone(), "target", 5),
            ("prolog", goal, "Target", 6),
        ] {
            assert!(structural_captured_name_for_fact(language, &site, text, maximum).is_none());
        }
    }
}

#[cfg(test)]
mod c_formal_capture_tests {
    use super::*;
    #[test]
    fn native_formal_names_require_the_exact_language_role_label_and_ascii_source() {
        let file = rootlight_ids::FileId::from_bytes([165; 20]);
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                rootlight_ir::SourceSpan::new(file, 0, 5).unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let read = fact(SyntaxFactKind::Occurrence, "c.formal_value.reference");
        assert_eq!(
            structural_captured_name_for_fact("c", &read, "value", 5).as_deref(),
            Some("value")
        );
        for (language, site, text, maximum) in [
            ("cpp", read.clone(), "value", 5),
            (
                "c",
                fact(SyntaxFactKind::Signature, "c.formal_value.reference"),
                "value",
                5,
            ),
            (
                "c",
                fact(SyntaxFactKind::Occurrence, "c.formal_value.extra"),
                "value",
                5,
            ),
            (
                "c",
                fact(
                    SyntaxFactKind::Signature,
                    "c.formal_binding_context.signature",
                ),
                "value",
                5,
            ),
            ("c", read.clone(), "value", 4),
            ("c", read.clone(), "value()", 64),
            ("c", read, "Î»", 64),
        ] {
            assert!(structural_captured_name_for_fact(language, &site, text, maximum).is_none());
        }
    }
}

#[cfg(test)]
mod go_call_formal_contract_tests {
    use super::*;
    use crate::SyntaxKindLabel;

    #[test]
    fn call_argument_protocol_names_require_exact_go_occurrence_labels() {
        let span = rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([3; 20]), 0, 5)
            .unwrap();
        for label in [
            "go.call_formal.reference",
            "go.unproven_call_formal.reference",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("go", &fact, "value", 5).as_deref(),
                Some("value")
            );
            assert!(structural_captured_name_for_fact("c", &fact, "value", 5).is_none());
            assert!(structural_captured_name_for_fact("go", &fact, "value", 4).is_none());
            let signature = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Signature,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert!(structural_captured_name_for_fact("go", &signature, "value", 5).is_none());
        }
        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            span,
            1,
            SyntaxKindLabel::new("go.call_formal.extra").unwrap(),
        );
        assert!(structural_captured_name_for_fact("go", &fact, "value", 5).is_none());
    }
}

#[cfg(test)]
mod cpp_const_formal_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn exact_const_formal_labels_borrow_only_complete_ascii_names_in_cpp() {
        let name = String::from("Value");
        for label in [
            "cpp.const_formal_receiver.reference",
            "cpp.unproven_const_formal.reference",
        ] {
            for kind in [
                SyntaxFactKind::Occurrence,
                SyntaxFactKind::Declaration,
                SyntaxFactKind::Signature,
            ] {
                let fact = SyntaxFact::new(
                    1,
                    None,
                    kind,
                    SourceSpan::new(FileId::from_bytes([1; 20]), 0, 5).unwrap(),
                    0,
                    SyntaxKindLabel::new(label).unwrap(),
                );
                let result = structural_captured_name_for_fact("cpp", &fact, &name, 5);
                assert_eq!(
                    result.as_deref(),
                    (kind == SyntaxFactKind::Occurrence).then_some("Value")
                );
                if let Some(Cow::Borrowed(text)) = result {
                    assert!(std::ptr::eq(text.as_ptr(), name.as_ptr()));
                }
                for language in ["c", "csharp", "rust", "CPP"] {
                    assert!(
                        structural_captured_name_for_fact(language, &fact, "Value", 5).is_none()
                    );
                }
                for text in [
                    " Value",
                    "Value ",
                    "Value[0]",
                    "Value::Name",
                    "two names",
                    "λ",
                    "",
                    "9Value",
                ] {
                    assert!(
                        structural_captured_name_for_fact("cpp", &fact, text, usize::MAX).is_none()
                    );
                }
                assert!(structural_captured_name_for_fact("cpp", &fact, "Value", 4).is_none());
            }
        }
    }
}

#[cfg(test)]
mod java_local_receiver_label_tests {
    use super::*;
    #[test]
    fn exact_local_receiver_labels_borrow_whole_plain_names_only() {
        let span = rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 6)
            .unwrap();
        for label in [
            "java.local_receiver.reference",
            "java.unproven_local_receiver.reference",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("java", &fact, "script", 6),
                Some(Cow::Borrowed("script"))
            );
            assert_eq!(
                structural_captured_name_for_fact("java", &fact, "script", 5),
                None
            );
            assert_eq!(
                structural_captured_name_for_fact("rust", &fact, "script", 6),
                None
            );
            for text in ["_", "script.member", "script()", "\\u0073cript", "λ"] {
                assert_eq!(
                    structural_captured_name_for_fact("java", &fact, text, 128),
                    None
                );
            }
        }
        for (kind, label) in [
            (SyntaxFactKind::Declaration, "java.local_receiver.reference"),
            (SyntaxFactKind::Signature, "java.local_receiver.reference"),
            (
                SyntaxFactKind::Occurrence,
                "java.local_receiver_context.signature",
            ),
            (
                SyntaxFactKind::Occurrence,
                "java.local_receiver_extra.reference",
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                span,
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("java", &fact, "script", 6),
                None
            );
        }
    }
}
#[cfg(test)]
mod go_written_call_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn only_exact_written_go_callee_labels_admit_complete_literal_names() {
        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 6).unwrap();
        for label in [
            "go.function_value.reference",
            "go.package_function_candidate.reference",
            "go.direct_file_function.reference",
            "go.unproven_direct_function.reference",
            "go.unproven_direct_function.call_name",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("go", &fact, "target", 6).as_deref(),
                Some("target")
            );
            assert!(structural_captured_name_for_fact("php", &fact, "target", 6).is_none());
            assert!(structural_captured_name_for_fact("go", &fact, "target", 5).is_none());
            for invalid in ["_", "target()", "target.extra", "ta\\rget", "λ"] {
                assert!(structural_captured_name_for_fact("go", &fact, invalid, 64).is_none());
            }
            let wrong_kind = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Signature,
                span,
                1,
                fact.syntax_kind().clone(),
            );
            assert!(structural_captured_name_for_fact("go", &wrong_kind, "target", 6).is_none());
        }
        for label in [
            "go.function_value.extra",
            "go.function_value.call",
            "go.function_value_extra.reference",
            "go.package_function_candidate.call",
            "go.package_function_candidate_extra.reference",
            "go.direct_file_function.extra",
            "go.unproven_direct_function.extra",
            "go.direct_file_function.call",
            "go.unproven_direct_function.call",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert!(structural_captured_name_for_fact("go", &fact, "target", 6).is_none());
        }
    }
}

#[cfg(test)]
mod php_written_call_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn only_exact_written_php_callee_labels_admit_complete_literal_names() {
        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 6).unwrap();
        for label in [
            "php.this_method.reference",
            "php.unproven_this_method.reference",
            "php.unproven_this_method.call_name",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("php", &fact, "target", 6).as_deref(),
                Some("target")
            );
            assert!(structural_captured_name_for_fact("java", &fact, "target", 6).is_none());
            assert!(structural_captured_name_for_fact("php", &fact, "target", 5).is_none());
            for invalid in ["target()", "target.extra", "ta\\rget", "λ"] {
                assert!(structural_captured_name_for_fact("php", &fact, invalid, 64).is_none());
            }
            let wrong_kind = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Signature,
                span,
                1,
                fact.syntax_kind().clone(),
            );
            assert!(structural_captured_name_for_fact("php", &wrong_kind, "target", 6).is_none());
        }
        for label in [
            "php.this_method.extra",
            "php.unproven_this_method.extra",
            "php.this_method.call",
            "php.unproven_this_method.call",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert!(structural_captured_name_for_fact("php", &fact, "target", 6).is_none());
        }
    }
}

#[cfg(test)]
mod powershell_formal_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn written_formal_names_require_exact_roles_language_and_complete_source_text() {
        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 6).unwrap();
        let fact = |kind, label| {
            SyntaxFact::new(1, None, kind, span, 0, SyntaxKindLabel::new(label).unwrap())
        };
        for label in [
            "powershell.formal_value.reference",
            "powershell.unproven_formal.reference",
        ] {
            let source = "$Value";
            let native = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                structural_captured_name_for_fact("powershell", &native, source, source.len())
                    .as_deref(),
                Some(source)
            );
            assert!(
                structural_captured_name_for_fact("powershell", &native, source, source.len() - 1)
                    .is_none()
            );
            assert!(
                structural_captured_name_for_fact("csharp", &native, source, source.len())
                    .is_none()
            );
            assert!(
                structural_captured_name_for_fact(
                    "powershell",
                    &fact(SyntaxFactKind::Signature, label),
                    source,
                    source.len()
                )
                .is_none()
            );
        }
        for label in [
            "powershell.formal_value.reference.extra",
            "powershell.formal_unknown.reference",
            "powershell.formal_context.signature",
        ] {
            assert!(
                structural_captured_name_for_fact(
                    "powershell",
                    &fact(SyntaxFactKind::Occurrence, label),
                    "$value",
                    6
                )
                .is_none()
            );
        }
        let certified = fact(
            SyntaxFactKind::Occurrence,
            "powershell.formal_value.reference",
        );
        for source in [
            "value",
            "$script:value",
            "${value}",
            "$value\0",
            "$value member",
        ] {
            assert!(
                structural_captured_name_for_fact("powershell", &certified, source, source.len())
                    .is_none()
            );
        }
        let unproven = fact(
            SyntaxFactKind::Occurrence,
            "powershell.unproven_formal.reference",
        );
        assert_eq!(
            structural_captured_name_for_fact("powershell", &unproven, "$script:value", 32)
                .as_deref(),
            Some("$script:value")
        );
    }
}

#[cfg(test)]
mod agda_rhs_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn written_rhs_names_require_exact_language_occurrence_label_and_complete_spelling() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 4).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let reference = fact(SyntaxFactKind::Occurrence, "agda.rhs.reference");
        for text in ["var%", "chosen", "pick%", "value2"] {
            assert_eq!(
                structural_captured_name_for_fact("agda", &reference, text, text.len()).as_deref(),
                Some(text)
            );
            assert!(
                structural_captured_name_for_fact("agda", &reference, text, text.len() - 1)
                    .is_none()
            );
            assert!(
                structural_captured_name_for_fact("coq", &reference, text, text.len()).is_none()
            );
        }
        for text in [
            "",
            "Other.var%",
            "var% arg",
            "_hole",
            "λ",
            "var%\0",
            "(var%)",
        ] {
            assert!(structural_captured_name_for_fact("agda", &reference, text, 128).is_none());
        }
        for (kind, label) in [
            (SyntaxFactKind::Signature, "agda.rhs.reference"),
            (SyntaxFactKind::Occurrence, "agda.rhs.reference.extra"),
            (SyntaxFactKind::Occurrence, "agda.rhs.target.signature"),
        ] {
            assert!(
                structural_captured_name_for_fact("agda", &fact(kind, label), "var%", 4).is_none()
            );
        }
    }
}

#[cfg(test)]
mod thinkscript_tests;

#[cfg(test)]
mod python_module_read_name_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn certified_module_names_require_closed_language_role_label_and_complete_ascii_spelling() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 5).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let read = fact(SyntaxFactKind::Occurrence, "python.module_read.reference");
        for name in ["total", "amount2", "_written"] {
            assert_eq!(
                structural_captured_name_for_fact("python", &read, name, name.len()).as_deref(),
                Some(name)
            );
            assert!(
                structural_captured_name_for_fact("python", &read, name, name.len() - 1).is_none()
            );
            assert!(structural_captured_name_for_fact("ruby", &read, name, name.len()).is_none());
        }
        for name in [
            "",
            "total\0",
            "total extra",
            "holder.total",
            "λ",
            "1total",
            "(total)",
        ] {
            assert!(structural_captured_name_for_fact("python", &read, name, 128).is_none());
        }
        for (kind, label) in [
            (SyntaxFactKind::Signature, "python.module_read.reference"),
            (
                SyntaxFactKind::Occurrence,
                "python.module_read.reference.extra",
            ),
            (SyntaxFactKind::Occurrence, "python.module_read.call"),
            (
                SyntaxFactKind::Occurrence,
                "python.module_reading.reference",
            ),
        ] {
            assert!(
                structural_captured_name_for_fact("python", &fact(kind, label), "total", 5)
                    .is_none()
            );
        }
    }
}
