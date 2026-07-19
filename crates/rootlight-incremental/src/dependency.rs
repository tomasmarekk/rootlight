//! Declared pass dependencies and the scoped invalidation graph.
//!
//! Graph construction verifies every edge against a pass contract so an
//! observed hidden input cannot silently become a stale-cache dependency.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use serde::Serialize;

use crate::{
    FactDomainSet, FactNode, GraphLimits, IncrementalError, InputKey, InputKind, PassId,
    ResourceKind, model::check_count,
};

/// Declared typed inputs and outputs of one deterministic analysis pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassDeclaration {
    id: PassId,
    input_kinds: BTreeSet<InputKind>,
    input_domains: FactDomainSet,
    output_domains: FactDomainSet,
}

impl PassDeclaration {
    /// Creates one pass declaration.
    ///
    /// A source pass may have no input fact domains, while every pass must
    /// declare at least one output domain.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::EmptyPassOutputs`] when no output is declared.
    pub fn new(
        id: PassId,
        input_kinds: impl IntoIterator<Item = InputKind>,
        input_domains: FactDomainSet,
        output_domains: FactDomainSet,
    ) -> Result<Self, IncrementalError> {
        if output_domains.is_empty() {
            return Err(IncrementalError::EmptyPassOutputs { pass: id });
        }
        Ok(Self {
            id,
            input_kinds: input_kinds.into_iter().collect(),
            input_domains,
            output_domains,
        })
    }

    /// Returns the stable pass identifier.
    #[must_use]
    pub const fn id(&self) -> &PassId {
        &self.id
    }

    /// Returns declared direct input-key kinds in canonical order.
    pub fn input_kinds(&self) -> impl Iterator<Item = InputKind> + '_ {
        self.input_kinds.iter().copied()
    }

    /// Returns declared input fact domains.
    #[must_use]
    pub const fn input_domains(&self) -> &FactDomainSet {
        &self.input_domains
    }

    /// Returns declared output fact domains.
    #[must_use]
    pub const fn output_domains(&self) -> &FactDomainSet {
        &self.output_domains
    }
}

/// Dependencies and outputs actually observed while a pass executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassObservation {
    input_kinds: BTreeSet<InputKind>,
    input_domains: FactDomainSet,
    output_domains: FactDomainSet,
}

impl PassObservation {
    /// Creates one canonical pass observation.
    #[must_use]
    pub fn new(
        input_kinds: impl IntoIterator<Item = InputKind>,
        input_domains: FactDomainSet,
        output_domains: FactDomainSet,
    ) -> Self {
        Self {
            input_kinds: input_kinds.into_iter().collect(),
            input_domains,
            output_domains,
        }
    }
}

/// Validated registry of versioned analysis-pass dependency contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRegistry {
    declarations: BTreeMap<PassId, PassDeclaration>,
}

impl DependencyRegistry {
    /// Canonicalizes and validates pass declarations.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, limit, or cancellation error.
    pub fn new(
        declarations: impl IntoIterator<Item = PassDeclaration>,
        limits: GraphLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical = BTreeMap::new();
        for declaration in declarations {
            cancellation.check()?;
            let id = declaration.id().clone();
            if canonical.insert(id.clone(), declaration).is_some() {
                return Err(IncrementalError::DuplicatePass { pass: id });
            }
            check_count(ResourceKind::Passes, canonical.len(), limits.max_passes)?;
        }
        Ok(Self {
            declarations: canonical,
        })
    }

    /// Verifies that runtime dependency use is a subset of the declared contract.
    ///
    /// # Errors
    ///
    /// Returns an unknown-pass or undeclared input/output error. This is the
    /// mechanical guard that makes hidden dependency tests fail.
    pub fn verify_observation(
        &self,
        pass: &PassId,
        observation: &PassObservation,
    ) -> Result<(), IncrementalError> {
        let declaration = self
            .declarations
            .get(pass)
            .ok_or_else(|| IncrementalError::UnknownPass { pass: pass.clone() })?;
        for kind in &observation.input_kinds {
            if !declaration.input_kinds.contains(kind) {
                return Err(IncrementalError::UndeclaredInputKind {
                    pass: pass.clone(),
                    kind: *kind,
                });
            }
        }
        for domain in observation.input_domains.iter() {
            if !declaration.input_domains.contains(domain) {
                return Err(IncrementalError::UndeclaredInputDomain {
                    pass: pass.clone(),
                    domain,
                });
            }
        }
        for domain in observation.output_domains.iter() {
            if !declaration.output_domains.contains(domain) {
                return Err(IncrementalError::UndeclaredOutputDomain {
                    pass: pass.clone(),
                    domain,
                });
            }
        }
        Ok(())
    }

    fn verify_edge(&self, edge: &DependencyEdge) -> Result<(), IncrementalError> {
        let declaration =
            self.declarations
                .get(edge.pass())
                .ok_or_else(|| IncrementalError::UnknownPass {
                    pass: edge.pass().clone(),
                })?;
        match edge.source() {
            DependencySource::Input(key) => {
                let kind = key.kind();
                if !declaration.input_kinds.contains(&kind) {
                    return Err(IncrementalError::UndeclaredInputKind {
                        pass: edge.pass().clone(),
                        kind,
                    });
                }
            }
            DependencySource::Fact(node) => {
                if !declaration.input_domains.contains(node.domain()) {
                    return Err(IncrementalError::UndeclaredInputDomain {
                        pass: edge.pass().clone(),
                        domain: node.domain(),
                    });
                }
            }
        }
        if !declaration.output_domains.contains(edge.target().domain()) {
            return Err(IncrementalError::UndeclaredOutputDomain {
                pass: edge.pass().clone(),
                domain: edge.target().domain(),
            });
        }
        Ok(())
    }
}

/// A typed source of one dependency edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DependencySource {
    /// A complete generation input.
    Input(InputKey),
    /// A scoped fact-domain output of another pass.
    Fact(FactNode),
}

/// One declared directed dependency edge with a stable reason identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEdge {
    source: DependencySource,
    target: FactNode,
    pass: PassId,
}

impl DependencyEdge {
    /// Creates one dependency edge.
    #[must_use]
    pub const fn new(source: DependencySource, target: FactNode, pass: PassId) -> Self {
        Self {
            source,
            target,
            pass,
        }
    }

    /// Returns the input or fact source.
    #[must_use]
    pub const fn source(&self) -> DependencySource {
        self.source
    }

    /// Returns the invalidated target node.
    #[must_use]
    pub const fn target(&self) -> FactNode {
        self.target
    }

    /// Returns the pass that owns the dependency.
    #[must_use]
    pub const fn pass(&self) -> &PassId {
        &self.pass
    }
}

/// A validated deterministic scoped dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    nodes: BTreeSet<FactNode>,
    edges: BTreeSet<DependencyEdge>,
    input_dependents: BTreeMap<InputKey, Vec<DependencyTarget>>,
    fact_dependents: BTreeMap<FactNode, Vec<DependencyTarget>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyTarget {
    node: FactNode,
    pass: PassId,
}

impl DependencyGraph {
    /// Canonicalizes nodes and validates every edge against the pass registry.
    ///
    /// Cycles are allowed because fixed-point planning uses an iterative visited
    /// set. Duplicate identical nodes and edges are canonicalized.
    ///
    /// # Errors
    ///
    /// Returns a limit, unknown-node, undeclared-dependency, or cancellation error.
    pub fn new(
        nodes: impl IntoIterator<Item = FactNode>,
        edges: impl IntoIterator<Item = DependencyEdge>,
        registry: &DependencyRegistry,
        limits: GraphLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical_nodes = BTreeSet::new();
        for node in nodes {
            cancellation.check()?;
            canonical_nodes.insert(node);
            check_count(
                ResourceKind::DependencyNodes,
                canonical_nodes.len(),
                limits.max_nodes,
            )?;
        }

        let mut canonical_edges = BTreeSet::new();
        let mut observed_edges = 0_usize;
        for edge in edges {
            cancellation.check()?;
            observed_edges = observed_edges.saturating_add(1);
            check_count(
                ResourceKind::DependencyEdges,
                observed_edges,
                limits.max_edges,
            )?;
            if let DependencySource::Fact(node) = edge.source()
                && !canonical_nodes.contains(&node)
            {
                return Err(IncrementalError::UnknownFactNode { node });
            }
            if !canonical_nodes.contains(&edge.target()) {
                return Err(IncrementalError::UnknownFactNode {
                    node: edge.target(),
                });
            }
            registry.verify_edge(&edge)?;
            canonical_edges.insert(edge);
        }

        let mut input_dependents: BTreeMap<InputKey, Vec<DependencyTarget>> = BTreeMap::new();
        let mut fact_dependents: BTreeMap<FactNode, Vec<DependencyTarget>> = BTreeMap::new();
        for edge in &canonical_edges {
            let target = DependencyTarget {
                node: edge.target(),
                pass: edge.pass().clone(),
            };
            match edge.source() {
                DependencySource::Input(key) => {
                    input_dependents.entry(key).or_default().push(target);
                }
                DependencySource::Fact(node) => {
                    fact_dependents.entry(node).or_default().push(target);
                }
            }
        }

        Ok(Self {
            nodes: canonical_nodes,
            edges: canonical_edges,
            input_dependents,
            fact_dependents,
        })
    }

    /// Returns graph nodes in canonical unit-and-domain order.
    pub fn nodes(&self) -> impl Iterator<Item = FactNode> + '_ {
        self.nodes.iter().copied()
    }

    /// Returns validated dependency edges in canonical order.
    pub fn edges(&self) -> impl Iterator<Item = &DependencyEdge> {
        self.edges.iter()
    }

    /// Reports whether a scoped fact node is part of the graph.
    #[must_use]
    pub fn contains_node(&self, node: FactNode) -> bool {
        self.nodes.contains(&node)
    }

    pub(crate) fn input_dependents(
        &self,
        key: InputKey,
    ) -> impl Iterator<Item = (FactNode, &PassId)> {
        self.input_dependents
            .get(&key)
            .into_iter()
            .flatten()
            .map(|target| (target.node, &target.pass))
    }

    pub(crate) fn fact_dependents(
        &self,
        node: FactNode,
    ) -> impl Iterator<Item = (FactNode, &PassId)> {
        self.fact_dependents
            .get(&node)
            .into_iter()
            .flatten()
            .map(|target| (target.node, &target.pass))
    }

    pub(crate) fn has_input_dependency(&self, key: InputKey) -> bool {
        self.input_dependents.contains_key(&key)
    }
}
