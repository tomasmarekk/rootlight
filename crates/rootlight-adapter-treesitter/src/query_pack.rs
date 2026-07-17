//! Reviewed structural query packs for Rootlight's audited grammars.
//!
//! Native queries and capture indices stay private; runtime extraction sees
//! only the closed, parser-independent role mapping defined here.

use tree_sitter::Query;

use crate::{GrammarFamily, registry::language_for};

const EXPECTED_CAPTURES: [&str; 11] = [
    "comment",
    "declaration",
    "definition",
    "documentation",
    "import",
    "module",
    "reference",
    "root",
    "scope",
    "signature",
    "string",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StructuralRole {
    Root,
    Module,
    Declaration,
    Signature,
    Import,
    Scope,
    Definition,
    Reference,
    Comment,
    Documentation,
    StringLiteral,
}

impl StructuralRole {
    fn from_capture_name(name: &str) -> Option<Self> {
        match name {
            "root" => Some(Self::Root),
            "module" => Some(Self::Module),
            "declaration" => Some(Self::Declaration),
            "signature" => Some(Self::Signature),
            "import" => Some(Self::Import),
            "scope" => Some(Self::Scope),
            "definition" => Some(Self::Definition),
            "reference" => Some(Self::Reference),
            "comment" => Some(Self::Comment),
            "documentation" => Some(Self::Documentation),
            "string" => Some(Self::StringLiteral),
            _ => None,
        }
    }
}

pub(crate) struct QueryPack {
    query: Query,
    roles_by_capture: Vec<StructuralRole>,
}

impl QueryPack {
    fn compile(family: GrammarFamily, source: &str) -> Result<Self, GrammarFamily> {
        let query = Query::new(&language_for(family), source).map_err(|_| family)?;
        let mut observed = query.capture_names().to_vec();
        observed.sort_unstable();
        if observed != EXPECTED_CAPTURES {
            return Err(family);
        }
        let roles_by_capture = query
            .capture_names()
            .iter()
            .map(|name| StructuralRole::from_capture_name(name).ok_or(family))
            .collect::<Result<Vec<_>, _>>()?;
        let pack = Self {
            query,
            roles_by_capture,
        };
        if (0..u32::try_from(pack.roles_by_capture.len()).map_err(|_| family)?)
            .any(|capture| pack.role_for_capture(capture).is_none())
        {
            return Err(family);
        }
        Ok(pack)
    }

    pub(crate) fn role_for_capture(&self, capture: u32) -> Option<StructuralRole> {
        usize::try_from(capture)
            .ok()
            .and_then(|index| self.roles_by_capture.get(index))
            .copied()
    }
}

pub(crate) struct QueryPackRegistry {
    packs: Vec<(GrammarFamily, QueryPack)>,
}

impl QueryPackRegistry {
    pub(crate) fn audited() -> Result<Self, GrammarFamily> {
        let mut packs = Vec::with_capacity(4);
        for (family, source) in [
            (GrammarFamily::Rust, include_str!("../queries/rust.scm")),
            (GrammarFamily::Python, include_str!("../queries/python.scm")),
            (
                GrammarFamily::JavaScript,
                include_str!("../queries/javascript.scm"),
            ),
            (GrammarFamily::Java, include_str!("../queries/java.scm")),
        ] {
            packs.push((family, QueryPack::compile(family, source)?));
        }
        packs.sort_by_key(|(family, _)| *family);
        Ok(Self { packs })
    }

    pub(crate) fn get(&self, family: GrammarFamily) -> Option<&QueryPack> {
        self.packs
            .binary_search_by_key(&family, |(registered, _)| *registered)
            .ok()
            .and_then(|index| self.packs.get(index))
            .map(|(_, pack)| pack)
    }

    pub(crate) const fn len(&self) -> usize {
        self.packs.len()
    }

    pub(crate) fn pattern_count(&self) -> usize {
        self.packs
            .iter()
            .map(|(_, pack)| pack.query.pattern_count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_packs_compile_with_the_exact_closed_capture_contract() {
        let registry = QueryPackRegistry::audited().expect("reviewed packs compile");

        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
        ] {
            let pack = registry.get(family).expect("family has a query pack");
            let mut names = pack.query.capture_names().to_vec();
            names.sort_unstable();
            assert_eq!(names, EXPECTED_CAPTURES);
            assert_eq!(pack.roles_by_capture.len(), EXPECTED_CAPTURES.len());
        }
    }
}
