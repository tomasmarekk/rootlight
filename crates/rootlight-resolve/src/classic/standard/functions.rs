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
    // MS-VBAL 6.1.2.11.1 defines these search, replacement and array contracts.
    // Optional argument values and element coercions remain runtime concerns.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/6af9457c-70ea-4691-80d6-3dca9f152aee
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/d9c2417c-2d36-45b1-b3d7-1876064e6085
    Function {
        name: "Replace",
        signature: "Function Replace(Expression As String, Find As String, Replace As String, Optional Start As Long = 1, Optional Count As Long = -1, Optional Compare As VbCompareMethod = vbBinaryCompare) As String",
        parameters: &[
            ("expression", false),
            ("find", false),
            ("replace", false),
            ("start", true),
            ("count", true),
            ("compare", true),
        ],
    },
    Function {
        name: "Split",
        signature: "Function Split(Expression As String, Optional Delimiter As Variant, Optional Limit As Long = -1, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[
            ("expression", false),
            ("delimiter", true),
            ("limit", true),
            ("compare", true),
        ],
    },
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/1c276c6f-c3b0-4de7-ade5-7803f99eea44
    Function {
        name: "Join",
        signature: "Function Join(SourceArray() As Variant, Optional Delimiter As Variant) As String",
        parameters: &[("sourcearray", false), ("delimiter", true)],
    },
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/22ca3cbf-f56b-4795-8f9c-f0a59414eaad
    Function {
        name: "Filter",
        signature: "Function Filter(SourceArray() As Variant, Match As String, Optional Include As Boolean = True, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[
            ("sourcearray", false),
            ("match", false),
            ("include", true),
            ("compare", true),
        ],
    },
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/9d45a100-3a6d-4cb6-aa09-772ccffe9706
    Function {
        name: "InStrRev",
        signature: "Function InStrRev(StringCheck As String, StringMatch As String, Optional Start As Long = -1, Optional Compare As VbCompareMethod = vbBinaryCompare) As Long",
        parameters: &[
            ("stringcheck", false),
            ("stringmatch", false),
            ("start", true),
            ("compare", true),
        ],
    },
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/9541f02a-e173-47a7-8c0f-4d91813c1c88
    Function {
        name: "StrComp",
        signature: "Function StrComp(String1 As Variant, String2 As Variant, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[("string1", false), ("string2", false), ("compare", true)],
    },
    // IIf is a function, not a short-circuit branch: both argument expressions
    // retain their dependencies regardless of the condition's apparent value.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/3593c073-c770-4060-8416-aaf7545ea5fe
    Function {
        name: "IIf",
        signature: "Function IIf(Expression As Variant, TruePart As Variant, FalsePart As Variant) As Variant",
        parameters: &[
            ("expression", false),
            ("truepart", false),
            ("falsepart", false),
        ],
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
