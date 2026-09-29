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
    // MS-VBAL 6.1.2.3.1 and 6.1.2.7.1 distinguish conversion and inspection contracts.
    // The model does not assume an implementation's LongLong/LongPtr availability.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/1951c2a7-7527-4d8d-b4a5-3b212d2442d5
    Function {
        name: "CBool",
        signature: "Function CBool(Expression As Variant) As Boolean",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CByte",
        signature: "Function CByte(Expression As Variant) As Byte",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CCur",
        signature: "Function CCur(Expression As Variant) As Currency",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CDate",
        signature: "Function CDate(Expression As Variant) As Date",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CVDate",
        signature: "Function CVDate(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CDbl",
        signature: "Function CDbl(Expression As Variant) As Double",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CDec",
        signature: "Function CDec(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CInt",
        signature: "Function CInt(Expression As Variant) As Integer",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CLng",
        signature: "Function CLng(Expression As Variant) As Long",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CSng",
        signature: "Function CSng(Expression As Variant) As Single",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CStr",
        signature: "Function CStr(Expression As Variant) As String",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CVar",
        signature: "Function CVar(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "CVErr",
        signature: "Function CVErr(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        name: "Error",
        signature: "Function Error(Optional ErrorNumber As Variant) As Variant",
        parameters: &[("errornumber", true)],
    },
    Function {
        name: "Error$",
        signature: "Function Error$(Optional ErrorNumber As Variant) As String",
        parameters: &[("errornumber", true)],
    },
    Function {
        name: "Fix",
        signature: "Function Fix(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Hex",
        signature: "Function Hex(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Hex$",
        signature: "Function Hex$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        name: "Int",
        signature: "Function Int(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Oct",
        signature: "Function Oct(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Oct$",
        signature: "Function Oct$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        name: "Str",
        signature: "Function Str(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        name: "Str$",
        signature: "Function Str$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        name: "Val",
        signature: "Function Val(Value As String) As Double",
        parameters: &[("value", false)],
    },
    Function {
        name: "IsArray",
        signature: "Function IsArray(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsDate",
        signature: "Function IsDate(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsEmpty",
        signature: "Function IsEmpty(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsError",
        signature: "Function IsError(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsMissing",
        signature: "Function IsMissing(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsNull",
        signature: "Function IsNull(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsNumeric",
        signature: "Function IsNumeric(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "IsObject",
        signature: "Function IsObject(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        name: "TypeName",
        signature: "Function TypeName(Arg As Variant) As String",
        parameters: &[("arg", false)],
    },
    Function {
        name: "VarType",
        signature: "Function VarType(VarName As Variant) As VbVarType",
        parameters: &[("varname", false)],
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
