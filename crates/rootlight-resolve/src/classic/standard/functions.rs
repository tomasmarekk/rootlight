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
    // MS-VBAL 6.1.2.11.1.17-21, 23-24 and 30-42 define these string contracts.
    // Omitted declaration types mean Variant; dollar variants change only the result.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/f87cc0fb-f0ca-4875-9691-34d80e2933b0
    Function {
        name: "LCase",
        signature: "Function LCase(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        name: "LCase$",
        signature: "Function LCase$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        name: "UCase",
        signature: "Function UCase(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        name: "UCase$",
        signature: "Function UCase$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        name: "Left",
        signature: "Function Left(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "LeftB",
        signature: "Function LeftB(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "Left$",
        signature: "Function Left$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "LeftB$",
        signature: "Function LeftB$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "Right",
        signature: "Function Right(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "RightB",
        signature: "Function RightB(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "Right$",
        signature: "Function Right$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "RightB$",
        signature: "Function RightB$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        name: "LTrim",
        signature: "Function LTrim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        name: "LTrim$",
        signature: "Function LTrim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        name: "RTrim",
        signature: "Function RTrim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        name: "RTrim$",
        signature: "Function RTrim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        name: "Trim",
        signature: "Function Trim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        name: "Trim$",
        signature: "Function Trim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        name: "Space",
        signature: "Function Space(Number As Long) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Space$",
        signature: "Function Space$(Number As Long) As String",
        parameters: &[("number", false)],
    },
    Function {
        name: "String",
        signature: "Function String(Number As Long, Character As Variant) As Variant",
        parameters: &[("number", false), ("character", false)],
    },
    Function {
        name: "String$",
        signature: "Function String$(Number As Long, Character As Variant) As String",
        parameters: &[("number", false), ("character", false)],
    },
    Function {
        name: "StrReverse",
        signature: "Function StrReverse(Expression As String) As String",
        parameters: &[("expression", false)],
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
