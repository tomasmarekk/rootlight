//! Native identity-query refusals retain their actual operation and units.
//! A boolean helper/cursor refusal has no invented count; this private evidence
//! becomes a source-only certificate only after the admitted runtime transaction.

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusalOrigin {
    CaptureSlots,
    GeneratedSlots,
    Candidates,
    Cursor,
    YieldedMatches,
    ApexCaptures,
    AssemblyCaptures,
    BashReads,
    CCalls,
    CFormalReads,
    CVariables,
    ChapelCaptures,
    ClojureCaptures,
    CmakeCaptures,
    CommonLispCaptures,
    CoqCaptures,
    CppConstFormals,
    CrystalCaptures,
    CsharpFormals,
    PowerShellFormals,
    DockerCaptures,
    EiffelCaptures,
    FishCaptures,
    FsharpCaptures,
    GdscriptCaptures,
    GlslCaptures,
    GoCalls,
    PhpThisCalls,
    GoFormalReads,
    HareCaptures,
    HaxeCaptures,
    HdlCaptures,
    HlslCaptures,
    JavaLocalReceivers,
    JavaParameterReads,
    JsonSchemaCaptures,
    JuliaCaptures,
    CoffeescriptCaptures,
    PugCaptures,
    LeanCaptures,
    WolframCaptures,
    SedCaptures,
    IdrisCaptures,
    MesonCaptures,
    ModelicaCaptures,
    MojoCaptures,
    OcamlCaptures,
    PascalCaptures,
    PlsqlCaptures,
    PonyCaptures,
    PythonParameterReads,
    PythonModuleVariables,
    QmlCaptures,
    QsharpCaptures,
    RacketCaptures,
    RingCaptures,
    SasCaptures,
    ScalaFormals,
    SchemeCaptures,
    SmlCaptures,
    TypescriptCalls,
    VCaptures,
    VhdlCaptures,
    WatCaptures,
    WgslCaptures,
    XmlCaptures,
}

impl RefusalOrigin {
    const fn code(self) -> &'static str {
        match self {
            Self::CaptureSlots => "query-identity-capture-slots-limit",
            Self::GeneratedSlots => "query-identity-generated-slots-limit",
            Self::Candidates => "query-identity-candidate-limit",
            Self::Cursor => "query-identity-cursor-active-match-limit",
            Self::YieldedMatches => "query-identity-yielded-match-limit",
            Self::ApexCaptures => "query-identity-apex-captures-limit",
            Self::AssemblyCaptures => "query-identity-assembly-captures-limit",
            Self::BashReads => "query-identity-bash-reads-limit",
            Self::CCalls => "query-identity-c-calls-limit",
            Self::CFormalReads => "query-identity-c-formal-reads-limit",
            Self::CVariables => "query-identity-c-written-variables-limit",
            Self::ChapelCaptures => "query-identity-chapel-captures-limit",
            Self::ClojureCaptures => "query-identity-clojure-captures-limit",
            Self::CmakeCaptures => "query-identity-cmake-captures-limit",
            Self::CommonLispCaptures => "query-identity-common-lisp-captures-limit",
            Self::CoqCaptures => "query-identity-coq-captures-limit",
            Self::CppConstFormals => "query-identity-cpp-const-formals-limit",
            Self::CrystalCaptures => "query-identity-crystal-captures-limit",
            Self::CsharpFormals => "query-identity-csharp-formals-limit",
            Self::PowerShellFormals => "query-identity-powershell-formals-limit",
            Self::DockerCaptures => "query-identity-dockerfile-captures-limit",
            Self::EiffelCaptures => "query-identity-eiffel-captures-limit",
            Self::FishCaptures => "query-identity-fish-captures-limit",
            Self::FsharpCaptures => "query-identity-fsharp-captures-limit",
            Self::GdscriptCaptures => "query-identity-gdscript-captures-limit",
            Self::GlslCaptures => "query-identity-glsl-captures-limit",
            Self::GoCalls => "query-identity-go-calls-limit",
            Self::PhpThisCalls => "query-identity-php-this-calls-limit",
            Self::GoFormalReads => "query-identity-go-formal-reads-limit",
            Self::HareCaptures => "query-identity-hare-captures-limit",
            Self::HaxeCaptures => "query-identity-haxe-captures-limit",
            Self::HdlCaptures => "query-identity-hdl-captures-limit",
            Self::HlslCaptures => "query-identity-hlsl-captures-limit",
            Self::JavaLocalReceivers => "query-identity-java-local-receivers-limit",
            Self::JavaParameterReads => "query-identity-java-parameter-reads-limit",
            Self::JsonSchemaCaptures => "query-identity-json-schema-captures-limit",
            Self::JuliaCaptures => "query-identity-julia-captures-limit",
            Self::CoffeescriptCaptures => "query-identity-coffeescript-captures-limit",
            Self::PugCaptures => "query-identity-pug-captures-limit",
            Self::LeanCaptures => "query-identity-lean-captures-limit",
            Self::WolframCaptures => "query-identity-wolfram-captures-limit",
            Self::SedCaptures => "query-identity-sed-captures-limit",
            Self::IdrisCaptures => "query-identity-idris-captures-limit",
            Self::MesonCaptures => "query-identity-meson-captures-limit",
            Self::ModelicaCaptures => "query-identity-modelica-captures-limit",
            Self::MojoCaptures => "query-identity-mojo-captures-limit",
            Self::OcamlCaptures => "query-identity-ocaml-captures-limit",
            Self::PascalCaptures => "query-identity-pascal-captures-limit",
            Self::PlsqlCaptures => "query-identity-plsql-captures-limit",
            Self::PonyCaptures => "query-identity-pony-captures-limit",
            Self::PythonParameterReads => "query-identity-python-parameter-reads-limit",
            Self::PythonModuleVariables => "query-identity-python-module-variables-limit",
            Self::QmlCaptures => "query-identity-qml-captures-limit",
            Self::QsharpCaptures => "query-identity-qsharp-captures-limit",
            Self::RacketCaptures => "query-identity-racket-captures-limit",
            Self::RingCaptures => "query-identity-ring-captures-limit",
            Self::SasCaptures => "query-identity-sas-captures-limit",
            Self::ScalaFormals => "query-identity-scala-formals-limit",
            Self::SchemeCaptures => "query-identity-scheme-captures-limit",
            Self::SmlCaptures => "query-identity-sml-captures-limit",
            Self::TypescriptCalls => "query-identity-typescript-calls-limit",
            Self::VCaptures => "query-identity-v-captures-limit",
            Self::VhdlCaptures => "query-identity-vhdl-captures-limit",
            Self::WatCaptures => "query-identity-wat-captures-limit",
            Self::WgslCaptures => "query-identity-wgsl-captures-limit",
            Self::XmlCaptures => "query-identity-xml-captures-limit",
        }
    }

    fn recognizes(code: &str) -> bool {
        matches!(
            code,
            "query-identity-capture-slots-limit"
                | "query-identity-generated-slots-limit"
                | "query-identity-candidate-limit"
                | "query-identity-cursor-active-match-limit"
                | "query-identity-yielded-match-limit"
                | "query-identity-apex-captures-limit"
                | "query-identity-assembly-captures-limit"
                | "query-identity-bash-reads-limit"
                | "query-identity-c-calls-limit"
                | "query-identity-c-formal-reads-limit"
                | "query-identity-chapel-captures-limit"
                | "query-identity-clojure-captures-limit"
                | "query-identity-cmake-captures-limit"
                | "query-identity-common-lisp-captures-limit"
                | "query-identity-coq-captures-limit"
                | "query-identity-cpp-const-formals-limit"
                | "query-identity-crystal-captures-limit"
                | "query-identity-csharp-formals-limit"
                | "query-identity-powershell-formals-limit"
                | "query-identity-dockerfile-captures-limit"
                | "query-identity-eiffel-captures-limit"
                | "query-identity-fish-captures-limit"
                | "query-identity-fsharp-captures-limit"
                | "query-identity-gdscript-captures-limit"
                | "query-identity-glsl-captures-limit"
                | "query-identity-go-calls-limit"
                | "query-identity-php-this-calls-limit"
                | "query-identity-go-formal-reads-limit"
                | "query-identity-hare-captures-limit"
                | "query-identity-haxe-captures-limit"
                | "query-identity-hdl-captures-limit"
                | "query-identity-hlsl-captures-limit"
                | "query-identity-java-local-receivers-limit"
                | "query-identity-java-parameter-reads-limit"
                | "query-identity-json-schema-captures-limit"
                | "query-identity-julia-captures-limit"
                | "query-identity-coffeescript-captures-limit"
                | "query-identity-pug-captures-limit"
                | "query-identity-wolfram-captures-limit"
                | "query-identity-sed-captures-limit"
                | "query-identity-idris-captures-limit"
                | "query-identity-meson-captures-limit"
                | "query-identity-modelica-captures-limit"
                | "query-identity-mojo-captures-limit"
                | "query-identity-ocaml-captures-limit"
                | "query-identity-pascal-captures-limit"
                | "query-identity-plsql-captures-limit"
                | "query-identity-pony-captures-limit"
                | "query-identity-python-parameter-reads-limit"
                | "query-identity-python-module-variables-limit"
                | "query-identity-qml-captures-limit"
                | "query-identity-qsharp-captures-limit"
                | "query-identity-racket-captures-limit"
                | "query-identity-ring-captures-limit"
                | "query-identity-sas-captures-limit"
                | "query-identity-scala-formals-limit"
                | "query-identity-scheme-captures-limit"
                | "query-identity-sml-captures-limit"
                | "query-identity-typescript-calls-limit"
                | "query-identity-v-captures-limit"
                | "query-identity-vhdl-captures-limit"
                | "query-identity-wat-captures-limit"
                | "query-identity-wgsl-captures-limit"
                | "query-identity-xml-captures-limit"
        )
    }
}

/// Source-free measurements of one genuine native identity-query refusal.
///
/// This value is descriptive metadata, not authority to publish source or facts.
/// Missing counts are explicitly unavailable; configured scan allowances are not
/// substituted for a helper's unreported count or the native cursor's peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeIdentityRefusalDetails {
    origin: RefusalOrigin,
    unit: &'static str,
    observed: Option<usize>,
    limit: Option<usize>,
    native_matches: Option<usize>,
    native_captures: Option<usize>,
    generated_capture_slots: Option<usize>,
    cursor_exceeded: bool,
}

impl NativeIdentityRefusalDetails {
    /// Returns the closed operation code, without physical paths or source text.
    #[must_use]
    pub const fn origin(self) -> &'static str {
        self.origin.code()
    }

    /// Returns the unit of the observed/limit pair, not an inferred Records unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        self.unit
    }

    /// Returns the exact rejected ordinal when checked, otherwise `None`.
    #[must_use]
    pub const fn observed(self) -> Option<usize> {
        self.observed
    }

    /// Returns the actual checked allowance when exposed by that operation.
    #[must_use]
    pub const fn limit(self) -> Option<usize> {
        self.limit
    }

    /// Returns accepted native matches, captures and generated capture slots.
    /// A pre-emission failure can lack a completed query progress snapshot.
    #[must_use]
    pub const fn progress(self) -> (Option<usize>, Option<usize>, Option<usize>) {
        (
            self.native_matches,
            self.native_captures,
            self.generated_capture_slots,
        )
    }

    /// Reports the native cursor's boolean overflow, without claiming its peak.
    #[must_use]
    pub const fn cursor_exceeded(self) -> bool {
        self.cursor_exceeded
    }

    /// Formats a compact persistent gap explanation from source-free metadata.
    /// Unknown counts remain explicit rather than appearing as measured values.
    #[must_use]
    pub fn diagnostic(self, message: &str) -> String {
        let count =
            |value: Option<usize>| value.map_or_else(|| "unknown".to_owned(), |n| n.to_string());
        format!(
            "{message}; origin={} unit={} observed={} limit={} native_matches={} native_captures={} generated_capture_slots={} cursor_exceeded={}",
            self.origin(),
            self.unit,
            count(self.observed),
            count(self.limit),
            count(self.native_matches),
            count(self.native_captures),
            count(self.generated_capture_slots),
            self.cursor_exceeded
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdentityRefusal {
    details: NativeIdentityRefusalDetails,
}

impl IdentityRefusal {
    fn new(
        origin: RefusalOrigin,
        unit: &'static str,
        observed: Option<usize>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            details: NativeIdentityRefusalDetails {
                origin,
                unit,
                observed,
                limit,
                native_matches: None,
                native_captures: None,
                generated_capture_slots: None,
                cursor_exceeded: false,
            },
        }
    }

    pub(crate) fn helper(origin: RefusalOrigin) -> Self {
        Self::new(origin, "helper-allowance", None, None)
    }

    pub(crate) fn checked_helper(origin: RefusalOrigin, initial: usize, remaining: usize) -> Self {
        let observed = initial
            .checked_sub(remaining)
            .and_then(|done| done.checked_add(1));
        Self::new(origin, "checked-native-work", observed, Some(initial))
    }

    pub(crate) fn capture_slots(completed: usize, limit: usize) -> Self {
        Self::new(
            RefusalOrigin::CaptureSlots,
            "accounted-capture-slots",
            completed.checked_add(1),
            Some(limit),
        )
    }

    pub(crate) fn generated_slots(completed: usize, limit: usize) -> Self {
        Self::new(
            RefusalOrigin::GeneratedSlots,
            "accounted-capture-slots",
            completed.checked_add(1),
            Some(limit),
        )
    }

    pub(crate) fn candidates(completed: usize, limit: usize) -> Self {
        Self::new(
            RefusalOrigin::Candidates,
            "identity-candidates",
            completed.checked_add(1),
            Some(limit),
        )
    }

    pub(crate) fn yielded_matches(completed: usize, limit: usize) -> Self {
        Self::new(
            RefusalOrigin::YieldedMatches,
            "yielded-native-matches",
            completed.checked_add(1),
            Some(limit),
        )
    }

    pub(crate) fn cursor() -> Self {
        Self::new(
            RefusalOrigin::Cursor,
            "active-native-matches",
            None,
            usize::try_from(super::QUERY_CURSOR_MATCH_LIMIT).ok(),
        )
    }

    pub(crate) fn progress(
        &mut self,
        matches: usize,
        captures: usize,
        generated: Option<usize>,
        cursor_exceeded: bool,
    ) {
        self.details.native_matches = Some(matches);
        self.details.native_captures = Some(captures);
        self.details.generated_capture_slots = generated;
        self.details.cursor_exceeded = cursor_exceeded;
    }

    pub(crate) const fn details(self) -> NativeIdentityRefusalDetails {
        self.details
    }

    pub(crate) fn recognizes(error: &AdapterError) -> bool {
        matches!(error, AdapterError::ProviderFailed { code } if RefusalOrigin::recognizes(code.as_str()))
    }

    pub(crate) fn error(self) -> AdapterError {
        AdapterError::ProviderFailed {
            code: DiagnosticCode::new(self.details.origin()).expect("closed refusal code is valid"),
        }
    }

    pub(crate) fn matches_error(self, error: &AdapterError) -> bool {
        matches!(error, AdapterError::ProviderFailed { code } if code.as_str() == self.details.origin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_helper_refusal_preserves_origin_without_scanner_or_capture_counts() {
        let event = IdentityRefusal::helper(RefusalOrigin::DockerCaptures);
        assert!(IdentityRefusal::recognizes(&event.error()));
        assert!(event.matches_error(&event.error()));
        assert_eq!(event.details().unit(), "helper-allowance");
        assert_eq!(
            (event.details().observed(), event.details().limit()),
            (None, None)
        );
        assert!(
            event
                .details()
                .diagnostic("source remains readable")
                .contains("observed=unknown limit=unknown")
        );
        let scanner = AdapterError::ProviderFailed {
            code: DiagnosticCode::new("dockerfile-scanner-capacity").unwrap(),
        };
        assert!(!IdentityRefusal::recognizes(&scanner));
        assert!(!event.matches_error(&scanner));
        assert!(!event.matches_error(&IdentityRefusal::cursor().error()));
    }

    #[test]
    fn unavailable_helper_count_is_not_the_global_capture_ceiling() {
        let event = IdentityRefusal::helper(RefusalOrigin::CppConstFormals);
        assert_eq!(
            (event.details().observed(), event.details().limit()),
            (None, None)
        );
        let message = event.details().diagnostic("source remains available");
        assert!(message.contains("observed=unknown limit=unknown"));
        assert!(!message.contains("2097153"));
        assert!(event.matches_error(&event.error()));
        assert!(!event.matches_error(&IdentityRefusal::cursor().error()));
    }

    #[test]
    fn cursor_flag_does_not_replace_helper_origin_or_invent_peak() {
        let mut event = IdentityRefusal::helper(RefusalOrigin::CppConstFormals);
        event.progress(7, 9, Some(2), true);
        assert_eq!(
            event.details().origin(),
            "query-identity-cpp-const-formals-limit"
        );
        assert!(event.details().cursor_exceeded());
        assert_eq!(event.details().progress(), (Some(7), Some(9), Some(2)));
        let cursor = IdentityRefusal::cursor().details();
        assert_eq!((cursor.observed(), cursor.limit()), (None, Some(4096)));
    }

    #[test]
    fn checked_work_counts_only_the_refused_step_after_exact_allowance() {
        let event = IdentityRefusal::checked_helper(RefusalOrigin::CFormalReads, 8, 0);
        assert_eq!(
            (event.details().observed(), event.details().limit()),
            (Some(9), Some(8))
        );
        assert_eq!(event.details().unit(), "checked-native-work");
        let slots = IdentityRefusal::capture_slots(8, 8).details();
        assert_eq!(slots.unit(), "accounted-capture-slots");
        assert_eq!((slots.observed(), slots.limit()), (Some(9), Some(8)));
    }
}
