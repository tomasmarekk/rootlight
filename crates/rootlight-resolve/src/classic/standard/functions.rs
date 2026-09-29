//! Declaration contracts for source-qualified VBA standard function calls.
//! These models establish static dependencies, not runtime values or successful
//! coercions. String-returning variants keep distinct identities from Variant results.

pub(super) struct Function {
    pub(super) name: &'static str,
    pub(super) signature: &'static str,
    pub(super) parameters: &'static [(&'static str, bool)],
}

// MS-VBAL 6.1.2.11.1.22 and 6.1.2.11.1.25 specify these parameter contracts.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/a4d9985a-e2cb-489c-a20a-5da09d77a59e
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/4a65ee4e-b6b9-45d3-a3f6-576fed4bb227
// Dollar variants have the same call syntax but return String rather than Variant.
// https://learn.microsoft.com/en-us/office/vba/language/concepts/getting-started/returning-strings-from-functions
pub(super) const FUNCTIONS: &[Function] = &[
    Function {
        name: "Len",
        signature: "Function Len(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "LenB",
        signature: "Function LenB(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "Mid",
        signature: "Function Mid(String As Variant, Start As Long, Optional Length As Variant) As Variant",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        name: "MidB",
        signature: "Function MidB(String As Variant, Start As Long, Optional Length As Variant) As Variant",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        name: "Mid$",
        signature: "Function Mid$(String As Variant, Start As Long, Optional Length As Variant) As String",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        name: "MidB$",
        signature: "Function MidB$(String As Variant, Start As Long, Optional Length As Variant) As String",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
];

pub(super) fn find(name: &str) -> Option<&'static Function> {
    FUNCTIONS
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case(name))
}

pub(super) fn qualified(name: &str) -> Option<&'static Function> {
    find(name.strip_prefix("VBA.")?)
}
