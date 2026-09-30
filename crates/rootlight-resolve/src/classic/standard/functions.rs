//! Declaration contracts for source-qualified VBA standard function calls.
//! These models establish static dependencies, not runtime values or successful
//! coercions. String-returning variants keep distinct identities from Variant results.

// MS-VBAL 6.1.2 declares these procedural namespaces; 5.6.12 requires lookup
// within the selected module, not an interchangeable qualification alias.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/e74eaaeb-e2e5-4ec5-9fb0-f3c739c53403
/// A supported standard procedural module, independent of flattened symbol identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Module {
    /// String inspection, transformation and formatting declarations.
    Strings,
    /// Value conversion declarations, including numeric/string conversions.
    Conversion,
    /// Type and value-state inspection declarations.
    Information,
    /// Application interaction declarations.
    Interaction,
}

impl Module {
    /// Lists only modules with explicitly modeled function declarations.
    pub(super) const ALL: [Self; 4] = [
        Self::Strings,
        Self::Conversion,
        Self::Information,
        Self::Interaction,
    ];

    /// Returns the declared procedural module name, without the VBA qualifier.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Strings => "Strings",
            Self::Conversion => "Conversion",
            Self::Information => "Information",
            Self::Interaction => "Interaction",
        }
    }

    /// Looks up a logical module name using VBA's case-insensitive name matching.
    pub(super) fn find(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|module| module.name().eq_ignore_ascii_case(name))
    }
}

/// A declaration whose module qualification preserves its existing symbol identity.
pub(super) struct Function {
    /// The sole declared procedural module, not an alias inferred from return type.
    pub(super) module: Module,
    pub(super) name: &'static str,
    pub(super) signature: &'static str,
    pub(super) parameters: &'static [(&'static str, bool)],
}

// MS-VBAL 6.1.2.11.1.22 and 6.1.2.11.1.25 specify the Len/Mid contracts.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/a4d9985a-e2cb-489c-a20a-5da09d77a59e
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/4a65ee4e-b6b9-45d3-a3f6-576fed4bb227
// Dollar variants have the same call syntax but return String rather than Variant.
// https://learn.microsoft.com/en-us/office/vba/language/concepts/getting-started/returning-strings-from-functions
pub(super) const FUNCTIONS: &[Function] = &[
    // MS-VBAL declares four optional parameters, not two overloads named
    // Start/String1/String2. Presence-dependent meanings and runtime errors do
    // not change the static declaration or prove a successful search.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/33e81067-4b43-4c45-a501-0acf1616f2f9
    Function {
        module: Module::Strings,
        name: "InStr",
        signature: "Function InStr(Optional Arg1 As Variant, Optional Arg2 As Variant, Optional Arg3 As Variant, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[
            ("arg1", true),
            ("arg2", true),
            ("arg3", true),
            ("compare", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "InStrB",
        signature: "Function InStrB(Optional Arg1 As Variant, Optional Arg2 As Variant, Optional Arg3 As Variant, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[
            ("arg1", true),
            ("arg2", true),
            ("arg3", true),
            ("compare", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "Len",
        signature: "Function Len(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Strings,
        name: "LenB",
        signature: "Function LenB(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Strings,
        name: "Mid",
        signature: "Function Mid(String As Variant, Start As Long, Optional Length As Variant) As Variant",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        module: Module::Strings,
        name: "MidB",
        signature: "Function MidB(String As Variant, Start As Long, Optional Length As Variant) As Variant",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        module: Module::Strings,
        name: "Mid$",
        signature: "Function Mid$(String As Variant, Start As Long, Optional Length As Variant) As String",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    Function {
        module: Module::Strings,
        name: "MidB$",
        signature: "Function MidB$(String As Variant, Start As Long, Optional Length As Variant) As String",
        parameters: &[("string", false), ("start", false), ("length", true)],
    },
    // MS-VBAL 6.1.2.11.1.17-21, 23-24 and 30-42 define these string contracts.
    // Omitted declaration types mean Variant; dollar variants change only the result.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/f87cc0fb-f0ca-4875-9691-34d80e2933b0
    Function {
        module: Module::Strings,
        name: "LCase",
        signature: "Function LCase(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "LCase$",
        signature: "Function LCase$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "UCase",
        signature: "Function UCase(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "UCase$",
        signature: "Function UCase$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "Left",
        signature: "Function Left(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "LeftB",
        signature: "Function LeftB(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "Left$",
        signature: "Function Left$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "LeftB$",
        signature: "Function LeftB$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "Right",
        signature: "Function Right(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "RightB",
        signature: "Function RightB(String As Variant, Length As Long) As Variant",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "Right$",
        signature: "Function Right$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "RightB$",
        signature: "Function RightB$(String As Variant, Length As Long) As String",
        parameters: &[("string", false), ("length", false)],
    },
    Function {
        module: Module::Strings,
        name: "LTrim",
        signature: "Function LTrim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "LTrim$",
        signature: "Function LTrim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "RTrim",
        signature: "Function RTrim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "RTrim$",
        signature: "Function RTrim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "Trim",
        signature: "Function Trim(String As Variant) As Variant",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "Trim$",
        signature: "Function Trim$(String As Variant) As String",
        parameters: &[("string", false)],
    },
    Function {
        module: Module::Strings,
        name: "Space",
        signature: "Function Space(Number As Long) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Strings,
        name: "Space$",
        signature: "Function Space$(Number As Long) As String",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Strings,
        name: "String",
        signature: "Function String(Number As Long, Character As Variant) As Variant",
        parameters: &[("number", false), ("character", false)],
    },
    Function {
        module: Module::Strings,
        name: "String$",
        signature: "Function String$(Number As Long, Character As Variant) As String",
        parameters: &[("number", false), ("character", false)],
    },
    Function {
        module: Module::Strings,
        name: "StrReverse",
        signature: "Function StrReverse(Expression As String) As String",
        parameters: &[("expression", false)],
    },
    // MS-VBAL 6.1.2.11 specifies character, formatting and localized-name calls.
    // These declarations do not evaluate code pages, calendar settings or locale values.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/f87cc0fb-f0ca-4875-9691-34d80e2933b0
    Function {
        module: Module::Strings,
        name: "Asc",
        signature: "Function Asc(StringValue As String) As Integer",
        parameters: &[("stringvalue", false)],
    },
    Function {
        module: Module::Strings,
        name: "AscB",
        signature: "Function AscB(StringValue As String) As Integer",
        parameters: &[("stringvalue", false)],
    },
    Function {
        module: Module::Strings,
        name: "AscW",
        signature: "Function AscW(StringValue As String) As Integer",
        parameters: &[("stringvalue", false)],
    },
    Function {
        module: Module::Strings,
        name: "Chr",
        signature: "Function Chr(CharCode As Long) As Variant",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "Chr$",
        signature: "Function Chr$(CharCode As Long) As String",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "ChrB",
        signature: "Function ChrB(CharCode As Long) As Variant",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "ChrB$",
        signature: "Function ChrB$(CharCode As Long) As String",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "ChrW",
        signature: "Function ChrW(CharCode As Long) As Variant",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "ChrW$",
        signature: "Function ChrW$(CharCode As Long) As String",
        parameters: &[("charcode", false)],
    },
    Function {
        module: Module::Strings,
        name: "Format",
        signature: "Function Format(Expression As Variant, Optional Format As Variant, Optional FirstDayOfWeek As VbDayOfWeek = vbSunday, Optional FirstWeekOfYear As VbFirstWeekOfYear = vbFirstJan1) As Variant",
        parameters: &[
            ("expression", false),
            ("format", true),
            ("firstdayofweek", true),
            ("firstweekofyear", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "Format$",
        signature: "Function Format$(Expression As Variant, Optional Format As Variant, Optional FirstDayOfWeek As VbDayOfWeek = vbSunday, Optional FirstWeekOfYear As VbFirstWeekOfYear = vbFirstJan1) As String",
        parameters: &[
            ("expression", false),
            ("format", true),
            ("firstdayofweek", true),
            ("firstweekofyear", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "FormatCurrency",
        signature: "Function FormatCurrency(Expression As Variant, Optional NumDigitsAfterDecimal As Long = -1, Optional IncludeLeadingDigit As VbTriState = vbUseDefault, Optional UseParensForNegativeNumbers As VbTriState = vbUseDefault, Optional GroupDigits As VbTriState = vbUseDefault) As String",
        parameters: &[
            ("expression", false),
            ("numdigitsafterdecimal", true),
            ("includeleadingdigit", true),
            ("useparensfornegativenumbers", true),
            ("groupdigits", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "FormatNumber",
        signature: "Function FormatNumber(Expression As Variant, Optional NumDigitsAfterDecimal As Long = -1, Optional IncludeLeadingDigit As VbTriState = vbUseDefault, Optional UseParensForNegativeNumbers As VbTriState = vbUseDefault, Optional GroupDigits As VbTriState = vbUseDefault) As String",
        parameters: &[
            ("expression", false),
            ("numdigitsafterdecimal", true),
            ("includeleadingdigit", true),
            ("useparensfornegativenumbers", true),
            ("groupdigits", true),
        ],
    },
    Function {
        module: Module::Strings,
        name: "FormatPercent",
        signature: "Function FormatPercent(Expression As Variant, Optional NumDigitsAfterDecimal As Long = -1, Optional IncludeLeadingDigit As VbTriState = vbUseDefault, Optional UseParensForNegativeNumbers As VbTriState = vbUseDefault, Optional GroupDigits As VbTriState = vbUseDefault) As String",
        parameters: &[
            ("expression", false),
            ("numdigitsafterdecimal", true),
            ("includeleadingdigit", true),
            ("useparensfornegativenumbers", true),
            ("groupdigits", true),
        ],
    },
    // The formal declaration omits Optional, but the same section's semantics
    // and Office contract permit NamedFormat omission with vbGeneralDate.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/dc56ab7e-dc09-4b50-90ae-d994460c99a6
    Function {
        module: Module::Strings,
        name: "FormatDateTime",
        signature: "Function FormatDateTime(Expression As Variant, Optional NamedFormat As VbDateTimeFormat = vbGeneralDate) As String",
        parameters: &[("expression", false), ("namedformat", true)],
    },
    Function {
        module: Module::Strings,
        name: "MonthName",
        signature: "Function MonthName(Month As Long, Optional Abbreviate As Boolean = False) As String",
        parameters: &[("month", false), ("abbreviate", true)],
    },
    // Office explicitly defines the named optional LCID, unlike the formal
    // specification's required LocaleID. Its system default is not a fixed value.
    // https://learn.microsoft.com/en-us/office/vba/language/reference/user-interface-help/strconv-function
    Function {
        module: Module::Strings,
        name: "StrConv",
        signature: "Function StrConv(String As Variant, Conversion As VbStrConv, Optional LCID As Long) As Variant",
        parameters: &[("string", false), ("conversion", false), ("lcid", true)],
    },
    Function {
        module: Module::Strings,
        name: "WeekdayName",
        signature: "Function WeekdayName(Weekday As Long, Optional Abbreviate As Boolean = False, Optional FirstDayOfWeek As VbDayOfWeek = vbUseSystemDayOfWeek) As String",
        parameters: &[
            ("weekday", false),
            ("abbreviate", true),
            ("firstdayofweek", true),
        ],
    },
    // MS-VBAL 6.1.2.11.1 defines these search, replacement and array contracts.
    // Optional argument values and element coercions remain runtime concerns.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/6af9457c-70ea-4691-80d6-3dca9f152aee
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/d9c2417c-2d36-45b1-b3d7-1876064e6085
    Function {
        module: Module::Strings,
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
        module: Module::Strings,
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
        module: Module::Strings,
        name: "Join",
        signature: "Function Join(SourceArray() As Variant, Optional Delimiter As Variant) As String",
        parameters: &[("sourcearray", false), ("delimiter", true)],
    },
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/22ca3cbf-f56b-4795-8f9c-f0a59414eaad
    Function {
        module: Module::Strings,
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
        module: Module::Strings,
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
        module: Module::Strings,
        name: "StrComp",
        signature: "Function StrComp(String1 As Variant, String2 As Variant, Optional Compare As VbCompareMethod = vbBinaryCompare) As Variant",
        parameters: &[("string1", false), ("string2", false), ("compare", true)],
    },
    // IIf is a function, not a short-circuit branch: both argument expressions
    // retain their dependencies regardless of the condition's apparent value.
    // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/3593c073-c770-4060-8416-aaf7545ea5fe
    Function {
        module: Module::Interaction,
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
        module: Module::Conversion,
        name: "CBool",
        signature: "Function CBool(Expression As Variant) As Boolean",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CByte",
        signature: "Function CByte(Expression As Variant) As Byte",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CCur",
        signature: "Function CCur(Expression As Variant) As Currency",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CDate",
        signature: "Function CDate(Expression As Variant) As Date",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CVDate",
        signature: "Function CVDate(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CDbl",
        signature: "Function CDbl(Expression As Variant) As Double",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CDec",
        signature: "Function CDec(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CInt",
        signature: "Function CInt(Expression As Variant) As Integer",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CLng",
        signature: "Function CLng(Expression As Variant) As Long",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CSng",
        signature: "Function CSng(Expression As Variant) As Single",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CStr",
        signature: "Function CStr(Expression As Variant) As String",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CVar",
        signature: "Function CVar(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "CVErr",
        signature: "Function CVErr(Expression As Variant) As Variant",
        parameters: &[("expression", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Error",
        signature: "Function Error(Optional ErrorNumber As Variant) As Variant",
        parameters: &[("errornumber", true)],
    },
    Function {
        module: Module::Conversion,
        name: "Error$",
        signature: "Function Error$(Optional ErrorNumber As Variant) As String",
        parameters: &[("errornumber", true)],
    },
    Function {
        module: Module::Conversion,
        name: "Fix",
        signature: "Function Fix(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Hex",
        signature: "Function Hex(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Hex$",
        signature: "Function Hex$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Int",
        signature: "Function Int(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Oct",
        signature: "Function Oct(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Oct$",
        signature: "Function Oct$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Str",
        signature: "Function Str(Number As Variant) As Variant",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Str$",
        signature: "Function Str$(Number As Variant) As String",
        parameters: &[("number", false)],
    },
    Function {
        module: Module::Conversion,
        name: "Val",
        signature: "Function Val(Value As String) As Double",
        parameters: &[("value", false)],
    },
    Function {
        module: Module::Information,
        name: "IsArray",
        signature: "Function IsArray(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsDate",
        signature: "Function IsDate(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsEmpty",
        signature: "Function IsEmpty(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsError",
        signature: "Function IsError(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsMissing",
        signature: "Function IsMissing(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsNull",
        signature: "Function IsNull(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsNumeric",
        signature: "Function IsNumeric(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "IsObject",
        signature: "Function IsObject(Arg As Variant) As Boolean",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
        name: "TypeName",
        signature: "Function TypeName(Arg As Variant) As String",
        parameters: &[("arg", false)],
    },
    Function {
        module: Module::Information,
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

/// Finds a function only in its declared procedural module, without creating aliases.
pub(super) fn find_in(module: Module, name: &str) -> Option<&'static Function> {
    find(name).filter(|function| function.module == module)
}

pub(super) fn qualified(name: &str) -> Option<&'static Function> {
    find(name.strip_prefix("VBA.")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn procedural_module_membership_is_exact_without_cross_module_aliases() {
        let expected: &[(Module, &[&str])] = &[
            (
                Module::Strings,
                &[
                    "Asc",
                    "AscB",
                    "AscW",
                    "Chr",
                    "Chr$",
                    "ChrB",
                    "ChrB$",
                    "ChrW",
                    "ChrW$",
                    "Filter",
                    "Format",
                    "Format$",
                    "FormatCurrency",
                    "FormatDateTime",
                    "FormatNumber",
                    "FormatPercent",
                    "InStr",
                    "InStrB",
                    "InStrRev",
                    "Join",
                    "LCase",
                    "LCase$",
                    "Left",
                    "Left$",
                    "LeftB",
                    "LeftB$",
                    "Len",
                    "LenB",
                    "LTrim",
                    "LTrim$",
                    "Mid",
                    "Mid$",
                    "MidB",
                    "MidB$",
                    "MonthName",
                    "Replace",
                    "Right",
                    "Right$",
                    "RightB",
                    "RightB$",
                    "RTrim",
                    "RTrim$",
                    "Space",
                    "Space$",
                    "Split",
                    "StrComp",
                    "StrConv",
                    "String",
                    "String$",
                    "StrReverse",
                    "Trim",
                    "Trim$",
                    "UCase",
                    "UCase$",
                    "WeekdayName",
                ],
            ),
            (
                Module::Conversion,
                &[
                    "CBool", "CByte", "CCur", "CDate", "CVDate", "CDbl", "CDec", "CInt", "CLng",
                    "CSng", "CStr", "CVar", "CVErr", "Error", "Error$", "Fix", "Hex", "Hex$",
                    "Int", "Oct", "Oct$", "Str", "Str$", "Val",
                ],
            ),
            (
                Module::Information,
                &[
                    "IsArray",
                    "IsDate",
                    "IsEmpty",
                    "IsError",
                    "IsMissing",
                    "IsNull",
                    "IsNumeric",
                    "IsObject",
                    "TypeName",
                    "VarType",
                ],
            ),
            (Module::Interaction, &["IIf"]),
        ];
        assert_eq!(expected.len(), Module::ALL.len());
        let mut names = BTreeSet::new();
        for &(module, members) in expected {
            assert_eq!(Module::find(module.name()), Some(module));
            assert_eq!(
                Module::find(&module.name().to_ascii_lowercase()),
                Some(module)
            );
            assert_eq!(
                Module::find(&module.name().to_ascii_uppercase()),
                Some(module)
            );
            let modeled = FUNCTIONS
                .iter()
                .filter(|function| function.module == module)
                .map(|function| function.name)
                .collect::<BTreeSet<_>>();
            assert_eq!(modeled, members.iter().copied().collect(), "{module:?}");
            for &name in members {
                assert!(names.insert(name), "duplicate expected declaration {name}");
                let function = find_in(module, name).unwrap();
                assert_eq!(function.name, name);
                assert_eq!(function.module, module);
                assert!(std::ptr::eq(function, find(name).unwrap()));
                assert!(std::ptr::eq(
                    function,
                    qualified(&format!("VBA.{name}")).unwrap()
                ));
                assert_eq!(
                    find_in(module, &name.to_ascii_lowercase()).unwrap().name,
                    name
                );
                assert_eq!(
                    find_in(module, &name.to_ascii_uppercase()).unwrap().name,
                    name
                );
                for other in Module::ALL {
                    if other != module {
                        assert!(find_in(other, name).is_none(), "{other:?}.{name}");
                        assert!(find_in(other, &name.to_ascii_lowercase()).is_none());
                    }
                }
            }
        }
        assert_eq!(names.len(), 90);
        assert_eq!(names.len(), FUNCTIONS.len());
        assert_eq!(
            expected
                .iter()
                .map(|(_, members)| members.len())
                .collect::<Vec<_>>(),
            [55, 24, 10, 1]
        );
        for name in [
            "",
            "Strings$",
            "VBA.Strings",
            "[Strings]",
            "DateTime",
            "Strings.extra",
        ] {
            assert!(Module::find(name).is_none(), "{name}");
        }
        for module in Module::ALL {
            for name in ["", "LenExtra", "CStr$", "IIf$", "VBA.Len", "Strings.Len"] {
                assert!(find_in(module, name).is_none(), "{module:?}.{name}");
            }
        }
        assert!(qualified("VBA.Strings.Len").is_none());
        assert!(qualified("VBA.Conversion.CStr").is_none());
    }

    #[test]
    fn string_extension_preserves_declared_types_and_defaults() {
        let contracts: &[(&[&str], &str, &str)] = &[
            (&["Asc", "AscB", "AscW"], "StringValue As String", "Integer"),
            (&["Chr", "ChrB", "ChrW"], "CharCode As Long", "Variant"),
            (&["Chr$", "ChrB$", "ChrW$"], "CharCode As Long", "String"),
            (
                &["Format"],
                "Expression As Variant, Optional Format As Variant, Optional FirstDayOfWeek As VbDayOfWeek = vbSunday, Optional FirstWeekOfYear As VbFirstWeekOfYear = vbFirstJan1",
                "Variant",
            ),
            (
                &["Format$"],
                "Expression As Variant, Optional Format As Variant, Optional FirstDayOfWeek As VbDayOfWeek = vbSunday, Optional FirstWeekOfYear As VbFirstWeekOfYear = vbFirstJan1",
                "String",
            ),
            (
                &["FormatCurrency", "FormatNumber", "FormatPercent"],
                "Expression As Variant, Optional NumDigitsAfterDecimal As Long = -1, Optional IncludeLeadingDigit As VbTriState = vbUseDefault, Optional UseParensForNegativeNumbers As VbTriState = vbUseDefault, Optional GroupDigits As VbTriState = vbUseDefault",
                "String",
            ),
            (
                &["FormatDateTime"],
                "Expression As Variant, Optional NamedFormat As VbDateTimeFormat = vbGeneralDate",
                "String",
            ),
            (
                &["MonthName"],
                "Month As Long, Optional Abbreviate As Boolean = False",
                "String",
            ),
            (
                &["WeekdayName"],
                "Weekday As Long, Optional Abbreviate As Boolean = False, Optional FirstDayOfWeek As VbDayOfWeek = vbUseSystemDayOfWeek",
                "String",
            ),
            (
                &["StrConv"],
                "String As Variant, Conversion As VbStrConv, Optional LCID As Long",
                "Variant",
            ),
        ];
        let mut checked = 0;
        for &(names, parameters, result) in contracts {
            for &name in names {
                assert_eq!(
                    find(name).unwrap().signature,
                    format!("Function {name}({parameters}) As {result}")
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 18);
    }

    #[test]
    fn string_declarations_are_complete_and_parameter_models_agree() {
        let strings = [
            "Asc",
            "AscB",
            "AscW",
            "Chr",
            "Chr$",
            "ChrB",
            "ChrB$",
            "ChrW",
            "ChrW$",
            "Filter",
            "Format",
            "Format$",
            "FormatCurrency",
            "FormatDateTime",
            "FormatNumber",
            "FormatPercent",
            "InStr",
            "InStrB",
            "InStrRev",
            "Join",
            "LCase",
            "LCase$",
            "Left",
            "Left$",
            "LeftB",
            "LeftB$",
            "Len",
            "LenB",
            "LTrim",
            "LTrim$",
            "Mid",
            "Mid$",
            "MidB",
            "MidB$",
            "MonthName",
            "Replace",
            "Right",
            "Right$",
            "RightB",
            "RightB$",
            "RTrim",
            "RTrim$",
            "Space",
            "Space$",
            "Split",
            "StrComp",
            "StrConv",
            "String",
            "String$",
            "StrReverse",
            "Trim",
            "Trim$",
            "UCase",
            "UCase$",
            "WeekdayName",
        ];
        assert_eq!(strings.len(), 55);
        for name in strings {
            assert_eq!(find(name).unwrap().name, name);
        }
        let mut names = BTreeSet::new();
        for function in FUNCTIONS {
            assert!(names.insert(function.name.to_ascii_lowercase()));
            let signature = function.signature.to_ascii_lowercase().replace("()", "");
            let mut parameters = BTreeSet::new();
            for &(name, optional) in function.parameters {
                assert!(parameters.insert(name));
                assert!(
                    signature.contains(&format!("{name} as ")),
                    "{}.{name}",
                    function.name
                );
                assert_eq!(
                    signature.contains(&format!("optional {name} as ")),
                    optional,
                    "{}.{name}",
                    function.name
                );
            }
        }
    }
}
