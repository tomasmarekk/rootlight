//! Declaration contracts for the mandatory VBA Err object's explicit members.
//! These identities describe the language library, not runtime error state or
//! repository-owned definitions. Default-property coercion is not inferred here.

pub(super) struct Member {
    pub(super) name: &'static str,
    pub(super) signature: &'static str,
    pub(super) kind: Kind,
}

pub(super) enum Kind {
    Property {
        writable: bool,
    },
    Method {
        parameters: &'static [(&'static str, bool)],
    },
}

// MS-VBAL 6.1.3.2 defines a global class with one default instance.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/7075a4ae-6055-4173-b4b6-e0dab68155e3
pub(super) const MEMBERS: &[Member] = &[
    Member {
        name: "Number",
        signature: "Property Number As Long",
        kind: Kind::Property { writable: true },
    },
    Member {
        name: "Description",
        signature: "Property Description As String",
        kind: Kind::Property { writable: true },
    },
    Member {
        name: "Source",
        signature: "Property Source As String",
        kind: Kind::Property { writable: true },
    },
    Member {
        name: "HelpFile",
        signature: "Property HelpFile As String",
        kind: Kind::Property { writable: true },
    },
    Member {
        name: "HelpContext",
        signature: "Property HelpContext As Long",
        kind: Kind::Property { writable: true },
    },
    Member {
        name: "LastDllError",
        signature: "Property Get LastDllError As Long",
        kind: Kind::Property { writable: false },
    },
    Member {
        name: "Clear",
        signature: "Sub Clear()",
        kind: Kind::Method { parameters: &[] },
    },
    Member {
        name: "Raise",
        signature: "Sub Raise(Number As Long, Optional Source As Variant, Optional Description As Variant, Optional HelpFile As Variant, Optional HelpContext As Variant)",
        kind: Kind::Method {
            parameters: &[
                ("number", false),
                ("source", true),
                ("description", true),
                ("helpfile", true),
                ("helpcontext", true),
            ],
        },
    },
];

pub(super) fn find(name: &str) -> Option<&'static Member> {
    let name = super::identifier(name)?;
    MEMBERS
        .iter()
        .find(|member| member.name.eq_ignore_ascii_case(&name))
}

pub(super) fn qualified(name: &str) -> Option<&'static Member> {
    find(name.strip_prefix("VBA.Err.")?)
}

pub(super) fn receiver_root(
    text: &str,
    cancellation: &super::Cancellation,
    work: &mut super::ResolutionWorkBudget,
) -> Result<Option<&'static str>, super::ResolutionError> {
    if let Some(name) = super::identifier(text) {
        return Ok((name == "err").then_some("err"));
    }
    if !super::starts_with_namespace(text.as_bytes()) {
        return Ok(None);
    }
    Ok(
        super::member_access_parts(text.as_bytes(), cancellation, work)?
            .filter(|(root, member)| {
                super::identifier(root).as_deref() == Some("vba")
                    && super::identifier(member).as_deref() == Some("err")
            })
            .map(|_| "vba"),
    )
}
