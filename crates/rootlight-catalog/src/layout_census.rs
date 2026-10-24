//! Prospective accounting for the exact current Oracle table layout.
//! Transactions own additions and removals until the host commits its matching
//! document plan; dropping a preview restores reference counts and payload maxima.

use std::mem::size_of;

use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{NormalizedIrDocument, SourceRef};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationContext, GenerationResource, GenerationStats,
};

use crate::{
    CatalogError, CatalogErrorKind, OracleReadAdmissionError, OracleReadMemoryPlan, read,
    write::{self, RegistryIdentity},
};

fn invalid() -> CatalogError {
    CatalogError::new(CatalogErrorKind::InvalidGeneration)
}

fn storage() -> CatalogError {
    CatalogError::new(CatalogErrorKind::Storage)
}

fn bytes<T>(count: usize) -> Result<u64, CatalogError> {
    count
        .checked_mul(size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(invalid)
}

#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    records: [u64; 10],
    children: u64,
    text: u64,
}

impl Totals {
    fn from_stats(stats: GenerationStats) -> Result<Self, CatalogError> {
        let records = [
            stats.files(),
            stats.entities(),
            stats.occurrences(),
            stats.relations(),
            stats.provenance(),
            stats.source_mappings(),
            stats.coverage(),
            stats.skipped_regions(),
            stats.diagnostics(),
            stats.extensions(),
        ];
        let logical = records
            .into_iter()
            .try_fold(0_u64, u64::checked_add)
            .ok_or_else(invalid)?;
        let children = stats
            .stored_rows()
            .checked_sub(3)
            .and_then(|n| n.checked_sub(logical.checked_mul(2)?))
            .and_then(|n| n.checked_sub(stats.source_refs()))
            .ok_or_else(invalid)?;
        Ok(Self {
            records,
            children,
            text: stats.text_bytes(),
        })
    }

    fn transition(self, removed: Self, added: Self) -> Result<Self, CatalogError> {
        let mut next = self;
        for (index, value) in next.records.iter_mut().enumerate() {
            *value = value
                .checked_sub(removed.records[index])
                .and_then(|n| n.checked_add(added.records[index]))
                .ok_or_else(invalid)?;
        }
        next.children = self
            .children
            .checked_sub(removed.children)
            .and_then(|n| n.checked_add(added.children))
            .ok_or_else(invalid)?;
        next.text = self
            .text
            .checked_sub(removed.text)
            .and_then(|n| n.checked_add(added.text))
            .ok_or_else(invalid)?;
        Ok(next)
    }

    fn stats(self, sources: usize) -> Result<GenerationStats, CatalogError> {
        let sources = u64::try_from(sources).map_err(|_| invalid())?;
        let logical = self
            .records
            .into_iter()
            .try_fold(0_u64, u64::checked_add)
            .ok_or_else(invalid)?;
        let rows = logical
            .checked_mul(2)
            .and_then(|n| n.checked_add(3))
            .and_then(|n| n.checked_add(sources))
            .and_then(|n| n.checked_add(self.children))
            .ok_or_else(invalid)?;
        GenerationStats::new(
            self.records[0],
            self.records[1],
            self.records[2],
            self.records[3],
            self.records[4],
            self.records[5],
            self.records[6],
            self.records[7],
            self.records[8],
            self.records[9],
            sources,
            rows,
            self.text,
        )
        .map_err(CatalogError::invalid_generation)
    }
}

/// An owned contribution measured by the same rules as the Oracle writer.
///
/// This is accounting data, not an identity-verified generation capability. Its
/// source keys include the complete SourceRef, including presentation line hints.
#[derive(Debug)]
pub struct OracleLayoutFragment {
    repository: RepositoryId,
    generation: GenerationId,
    totals: Totals,
    identities: Vec<RegistryIdentity>,
    sources: Vec<(SourceRef, u64)>,
    payloads: Vec<(u64, u64)>,
}

impl OracleLayoutFragment {
    /// Returns the writer-equivalent plan for this standalone contribution.
    ///
    /// This is useful for one final owned resolver candidate; constructing an
    /// additional persistent census tree is unnecessary for that one-shot check.
    /// # Errors
    /// Propagates generation budgets, cancellation and census arithmetic failures.
    pub fn read_memory_plan(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReadMemoryPlan, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        let stats = self.totals.stats(self.sources.len())?;
        context
            .require(GenerationResource::Rows, stats.stored_rows())
            .map_err(CatalogError::control)?;
        context
            .require(GenerationResource::SourceReferences, stats.source_refs())
            .map_err(CatalogError::control)?;
        context
            .require(GenerationResource::EncodedTextBytes, stats.text_bytes())
            .map_err(CatalogError::control)?;
        read::oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            self.payloads.last().map_or(0, |(bytes, _)| *bytes),
        )
    }

    /// Copies fixed-layout accounting keys while retaining the original owner.
    ///
    /// `admit` receives absolute simultaneous workspace, including both owners.
    /// # Errors
    /// Propagates generation control, overflow, allocation and host admission failures.
    pub fn duplicate<E>(
        &self,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        context.check().map_err(CatalogError::control)?;
        let predicted = bytes::<RegistryIdentity>(self.identities.len())?
            .checked_add(bytes::<(SourceRef, u64)>(self.sources.len())?)
            .and_then(|n| n.checked_add(bytes::<(u64, u64)>(self.payloads.len()).ok()?))
            .ok_or_else(invalid)?;
        let other = other_resident_bytes
            .checked_add(self.retained_bytes()?)
            .ok_or_else(invalid)?;
        admit(other.checked_add(predicted).ok_or_else(invalid)?)
            .map_err(OracleReadAdmissionError::Admission)?;
        let mut copy = Self::empty(self.repository, self.generation);
        copy.totals = self.totals;
        copy.identities
            .try_reserve_exact(self.identities.len())
            .map_err(|_| storage())?;
        copy.sources
            .try_reserve_exact(self.sources.len())
            .map_err(|_| storage())?;
        copy.payloads
            .try_reserve_exact(self.payloads.len())
            .map_err(|_| storage())?;
        for identity in &self.identities {
            context.check().map_err(CatalogError::control)?;
            copy.identities.push(*identity);
        }
        for source in &self.sources {
            context.check().map_err(CatalogError::control)?;
            copy.sources.push(source.clone());
        }
        copy.payloads.extend_from_slice(&self.payloads);
        admit(
            other
                .checked_add(copy.retained_bytes()?)
                .ok_or_else(invalid)?,
        )
        .map_err(OracleReadAdmissionError::Admission)?;
        context.check().map_err(CatalogError::control)?;
        Ok(copy)
    }

    /// Combines disjoint record contributions without materializing document text.
    ///
    /// Full source keys and payload multiplicities merge; duplicate record IDs
    /// reject the composition. This supports future metadata floors together
    /// with the small target removals owned by an optional-compaction plan.
    /// # Errors
    /// Rejects foreign generations, duplicate IDs, overflow, allocation, control
    /// and absolute simultaneous-workspace admission failures.
    pub fn merge<E>(
        mut self,
        mut other: Self,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        context.check().map_err(CatalogError::control)?;
        if self.repository != other.repository || self.generation != other.generation {
            return Err(invalid().into());
        }
        let totals = self.totals.transition(Totals::default(), other.totals)?;
        let identities = self
            .identities
            .len()
            .checked_add(other.identities.len())
            .ok_or_else(invalid)?;
        let sources = self
            .sources
            .len()
            .checked_add(other.sources.len())
            .ok_or_else(invalid)?;
        let payloads = self
            .payloads
            .len()
            .checked_add(other.payloads.len())
            .ok_or_else(invalid)?;
        let resident = other_resident_bytes
            .checked_add(self.retained_bytes()?)
            .and_then(|n| n.checked_add(other.retained_bytes().ok()?))
            .ok_or_else(invalid)?;
        let predicted = bytes::<RegistryIdentity>(identities)?
            .checked_add(bytes::<(SourceRef, u64)>(sources)?)
            .and_then(|n| n.checked_add(bytes::<(u64, u64)>(payloads).ok()?))
            .ok_or_else(invalid)?;
        admit(resident.checked_add(predicted).ok_or_else(invalid)?)
            .map_err(OracleReadAdmissionError::Admission)?;
        let mut combined = Self::empty(self.repository, self.generation);
        combined.totals = totals;
        combined
            .identities
            .try_reserve_exact(identities)
            .map_err(|_| storage())?;
        combined
            .sources
            .try_reserve_exact(sources)
            .map_err(|_| storage())?;
        combined
            .payloads
            .try_reserve_exact(payloads)
            .map_err(|_| storage())?;
        admit(
            resident
                .checked_add(combined.retained_bytes()?)
                .ok_or_else(invalid)?,
        )
        .map_err(OracleReadAdmissionError::Admission)?;
        combined.identities.append(&mut self.identities);
        combined.identities.append(&mut other.identities);
        combined.sources.append(&mut self.sources);
        combined.sources.append(&mut other.sources);
        combined.payloads.append(&mut self.payloads);
        combined.payloads.append(&mut other.payloads);
        combined.identities.sort_unstable();
        if combined
            .identities
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(invalid().into());
        }
        merge_counts(&mut combined.sources, context)?;
        merge_counts(&mut combined.payloads, context)?;
        context.check().map_err(CatalogError::control)?;
        Ok(combined)
    }

    /// Creates an allocation-free empty contribution for a pinned generation.
    #[must_use]
    pub fn empty(repository: RepositoryId, generation: GenerationId) -> Self {
        Self {
            repository,
            generation,
            totals: Totals::default(),
            identities: Vec::new(),
            sources: Vec::new(),
            payloads: Vec::new(),
        }
    }

    /// Measures exact writer rows, auxiliary children, text and payload lengths.
    ///
    /// `admit` receives an absolute temporary workspace charge, including the
    /// supplied other resident workspace. A host must retain a high-water
    /// reservation across calls rather than adding every temporary peak.
    ///
    /// # Errors
    /// Propagates catalog validation, cancellation, budget, allocation and host
    /// admission failures. None of these failures proves restore-capacity refusal.
    pub fn measure<E>(
        document: &NormalizedIrDocument,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        Self::measure_view(document.into(), other_resident_bytes, context, admit)
    }

    /// Measures one borrowed DiagnosticRecord using the current physical writer rules.
    ///
    /// No text or opaque payload is cloned. Admission uses absolute simultaneous workspace.
    /// # Errors
    /// Propagates generation, allocation, arithmetic and host workspace failures.
    pub fn measure_diagnostic<E>(
        record: &rootlight_ir::DiagnosticRecord,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        let mut view = write::DocumentView::empty(record.repository, record.generation);
        view.diagnostics = std::slice::from_ref(record);
        Self::measure_view(view, other_resident_bytes, context, admit)
    }

    /// Measures one borrowed ExtensionEnvelope using the current physical writer rules.
    ///
    /// No text or opaque payload is cloned. Admission uses absolute simultaneous workspace.
    /// # Errors
    /// Propagates generation, allocation, arithmetic and host workspace failures.
    pub fn measure_extension<E>(
        record: &rootlight_ir::ExtensionEnvelope,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        let mut view = write::DocumentView::empty(record.repository, record.generation);
        view.extensions = std::slice::from_ref(record);
        Self::measure_view(view, other_resident_bytes, context, admit)
    }

    /// Measures one borrowed CoverageRecord using the current physical writer rules.
    ///
    /// No text or opaque payload is cloned. Admission uses absolute simultaneous workspace.
    /// # Errors
    /// Propagates generation, allocation, arithmetic and host workspace failures.
    pub fn measure_coverage<E>(
        record: &rootlight_ir::CoverageRecord,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        let mut view = write::DocumentView::empty(record.repository, record.generation);
        view.coverage_records = std::slice::from_ref(record);
        Self::measure_view(view, other_resident_bytes, context, admit)
    }

    fn measure_view<E>(
        document: write::DocumentView<'_>,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, OracleReadAdmissionError<E>> {
        context.check().map_err(CatalogError::control)?;
        let logical = [
            document.files.len(),
            document.entities.len(),
            document.occurrences.len(),
            document.relations.len(),
            document.provenance.len(),
            document.source_mappings.len(),
            document.coverage_records.len(),
            document.skipped_regions.len(),
            document.diagnostics.len(),
            document.extensions.len(),
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
        .ok_or_else(invalid)?;
        let inputs = write::source_reference_inputs_view(document).try_fold(0_usize, |n, _| {
            context.check().map_err(CatalogError::control)?;
            n.checked_add(1).ok_or_else(invalid)
        })?;
        let payloads = document
            .files
            .iter()
            .filter(|file| file.path_locator.is_some())
            .count()
            .checked_add(document.source_mappings.len())
            .and_then(|n| n.checked_add(document.skipped_regions.len()))
            .and_then(|n| n.checked_add(document.diagnostics.len()))
            .and_then(|n| n.checked_add(document.extensions.len()))
            .ok_or_else(invalid)?;
        let registry = u64::try_from(logical.checked_add(1).ok_or_else(invalid)?)
            .map_err(|_| invalid())?
            .checked_mul(read::tree_node_bytes::<RegistryIdentity, ()>()?)
            .ok_or_else(invalid)?;
        let workspace = other_resident_bytes
            .checked_add(registry)
            .and_then(|n| n.checked_add(bytes::<&SourceRef>(inputs).ok()?))
            .and_then(|n| n.checked_add(bytes::<u64>(inputs).ok()?))
            .and_then(|n| n.checked_add(bytes::<(SourceRef, u64)>(inputs).ok()?))
            .and_then(|n| n.checked_add(bytes::<RegistryIdentity>(logical).ok()?))
            .and_then(|n| n.checked_add(bytes::<u64>(payloads).ok()?))
            .and_then(|n| n.checked_add(bytes::<(u64, u64)>(payloads).ok()?))
            .ok_or_else(invalid)?;
        admit(workspace).map_err(OracleReadAdmissionError::Admission)?;
        let mut auxiliary = write::AuxiliaryCensus::default();
        auxiliary
            .source_counts
            .try_reserve_exact(inputs)
            .map_err(|_| storage())?;
        auxiliary
            .payloads
            .try_reserve_exact(payloads)
            .map_err(|_| storage())?;
        let plan = write::measure_view(document, context, Some(&mut auxiliary))?;
        let source_capacity = plan.source_ordinals.capacity();
        let source_counts_capacity = auxiliary.source_counts.capacity();
        let payload_capacity = auxiliary.payloads.capacity();
        let mut fragment = Self::empty(document.repository, document.generation);
        fragment.totals = Totals::from_stats(plan.stats)?;
        fragment
            .identities
            .try_reserve_exact(logical)
            .map_err(|_| storage())?;
        fragment
            .sources
            .try_reserve_exact(plan.source_ordinals.len())
            .map_err(|_| storage())?;
        fragment
            .payloads
            .try_reserve_exact(auxiliary.payloads.len())
            .map_err(|_| storage())?;
        if plan.source_ordinals.len() != auxiliary.source_counts.len() {
            return Err(invalid().into());
        }
        // These are fixed-layout identity/source values. No document text or
        // decoded opaque payload survives measurement in the contribution.
        for identity in plan.identities {
            context.check().map_err(CatalogError::control)?;
            if !matches!(identity, RegistryIdentity::Repository(_)) {
                fragment.identities.push(identity);
            }
        }
        for (source, count) in plan
            .source_ordinals
            .into_iter()
            .zip(auxiliary.source_counts)
        {
            context.check().map_err(CatalogError::control)?;
            fragment.sources.push((source.clone(), count));
        }
        auxiliary.payloads.sort_unstable();
        context.check().map_err(CatalogError::control)?;
        for payload in auxiliary.payloads {
            if let Some((previous, count)) = fragment.payloads.last_mut()
                && *previous == payload
            {
                *count = count.checked_add(1).ok_or_else(invalid)?;
            } else {
                fragment.payloads.push((payload, 1));
            }
        }
        let actual = other_resident_bytes
            .checked_add(registry)
            .and_then(|n| n.checked_add(bytes::<&SourceRef>(source_capacity).ok()?))
            .and_then(|n| n.checked_add(bytes::<u64>(source_counts_capacity).ok()?))
            .and_then(|n| n.checked_add(fragment.retained_bytes().ok()?))
            .and_then(|n| n.checked_add(bytes::<u64>(payload_capacity).ok()?))
            .ok_or_else(invalid)?;
        admit(actual.max(workspace)).map_err(OracleReadAdmissionError::Admission)?;
        context.check().map_err(CatalogError::control)?;
        Ok(fragment)
    }

    /// Returns the charged resident capacity of this owned contribution.
    ///
    /// # Errors
    /// Returns a catalog error on integer overflow.
    pub fn retained_bytes(&self) -> Result<u64, CatalogError> {
        bytes::<RegistryIdentity>(self.identities.capacity())?
            .checked_add(bytes::<(SourceRef, u64)>(self.sources.capacity())?)
            .and_then(|n| n.checked_add(bytes::<(u64, u64)>(self.payloads.capacity()).ok()?))
            .ok_or_else(invalid)
    }
}

fn merge_counts<K: Ord>(
    values: &mut Vec<(K, u64)>,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    values.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    context.check().map_err(CatalogError::control)?;
    let mut overflow = false;
    values.dedup_by(|next, previous| {
        if next.0 != previous.0 {
            return false;
        }
        match previous.1.checked_add(next.1) {
            Some(count) => previous.1 = count,
            None => overflow = true,
        }
        true
    });
    context.check().map_err(CatalogError::control)?;
    if overflow {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Debug)]
struct Node<K> {
    key: Option<K>,
    count: u64,
    left: Option<usize>,
    right: Option<usize>,
    height: i16,
}

#[derive(Debug)]
struct CountedTree<K> {
    nodes: Vec<Node<K>>,
    root: Option<usize>,
    free: Option<usize>,
    len: usize,
}

impl<K: Ord + Clone> Default for CountedTree<K> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            root: None,
            free: None,
            len: 0,
        }
    }
}

impl<K: Ord + Clone> CountedTree<K> {
    fn count(&self, key: &K) -> u64 {
        let mut next = self.root;
        while let Some(index) = next {
            let node = &self.nodes[index];
            match key.cmp(node.key.as_ref().expect("reachable node has a key")) {
                std::cmp::Ordering::Less => next = node.left,
                std::cmp::Ordering::Greater => next = node.right,
                std::cmp::Ordering::Equal => return node.count,
            }
        }
        0
    }

    fn last(&self) -> Option<&K> {
        let mut index = self.root?;
        while let Some(right) = self.nodes[index].right {
            index = right;
        }
        self.nodes[index].key.as_ref()
    }

    fn retained_bytes(&self) -> Result<u64, CatalogError> {
        bytes::<Node<K>>(self.nodes.capacity())
    }

    fn reserve<E>(
        &mut self,
        additional: usize,
        other: u64,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<(), OracleReadAdmissionError<E>> {
        let requested = self.len.checked_add(additional).ok_or_else(invalid)?;
        if requested <= self.nodes.capacity() {
            return Ok(());
        }
        let desired = requested.max(self.nodes.capacity().checked_mul(2).ok_or_else(invalid)?);
        let transient = other
            .checked_add(self.retained_bytes()?)
            .and_then(|n| n.checked_add(bytes::<Node<K>>(desired).ok()?))
            .ok_or_else(invalid)?;
        admit(transient).map_err(OracleReadAdmissionError::Admission)?;
        let mut replacement = Vec::new();
        #[cfg(test)]
        if tests::FAIL_NEXT_ALLOCATION.with(|slot| slot.replace(false)) {
            return Err(storage().into());
        }
        replacement
            .try_reserve_exact(desired)
            .map_err(|_| storage())?;
        let actual = other
            .checked_add(self.retained_bytes()?)
            .and_then(|n| n.checked_add(bytes::<Node<K>>(replacement.capacity()).ok()?))
            .ok_or_else(invalid)?;
        admit(actual.max(transient)).map_err(OracleReadAdmissionError::Admission)?;
        replacement.append(&mut self.nodes);
        self.nodes = replacement;
        Ok(())
    }

    fn height(&self, index: Option<usize>) -> i16 {
        index.map_or(0, |index| self.nodes[index].height)
    }
    fn refresh(&mut self, index: usize) {
        self.nodes[index].height = 1 + self
            .height(self.nodes[index].left)
            .max(self.height(self.nodes[index].right));
    }
    fn rotate_left(&mut self, index: usize) -> usize {
        let root = self.nodes[index]
            .right
            .expect("left rotation has a right child");
        self.nodes[index].right = self.nodes[root].left;
        self.nodes[root].left = Some(index);
        self.refresh(index);
        self.refresh(root);
        root
    }
    fn rotate_right(&mut self, index: usize) -> usize {
        let root = self.nodes[index]
            .left
            .expect("right rotation has a left child");
        self.nodes[index].left = self.nodes[root].right;
        self.nodes[root].right = Some(index);
        self.refresh(index);
        self.refresh(root);
        root
    }
    fn balance(&mut self, index: usize) -> usize {
        self.refresh(index);
        let difference = self.height(self.nodes[index].left) - self.height(self.nodes[index].right);
        if difference > 1 {
            let left = self.nodes[index].left.expect("left-heavy node has a child");
            if self.height(self.nodes[left].left) < self.height(self.nodes[left].right) {
                self.nodes[index].left = Some(self.rotate_left(left));
            }
            return self.rotate_right(index);
        }
        if difference < -1 {
            let right = self.nodes[index]
                .right
                .expect("right-heavy node has a child");
            if self.height(self.nodes[right].right) < self.height(self.nodes[right].left) {
                self.nodes[index].right = Some(self.rotate_right(right));
            }
            return self.rotate_left(index);
        }
        index
    }
    fn allocate(&mut self, key: K, count: u64) -> usize {
        let node = Node {
            key: Some(key),
            count,
            left: None,
            right: None,
            height: 1,
        };
        if let Some(index) = self.free {
            self.free = self.nodes[index].left;
            self.nodes[index] = node;
            index
        } else {
            assert!(
                self.nodes.len() < self.nodes.capacity(),
                "transaction reserved every insertion"
            );
            self.nodes.push(node);
            self.nodes.len() - 1
        }
    }
    fn release(&mut self, index: usize) {
        self.nodes[index] = Node {
            key: None,
            count: 0,
            left: self.free,
            right: None,
            height: 0,
        };
        self.free = Some(index);
    }
    fn take_min(&mut self, index: usize) -> (Option<usize>, K, u64) {
        if let Some(left) = self.nodes[index].left {
            let (next, key, count) = self.take_min(left);
            self.nodes[index].left = next;
            return (Some(self.balance(index)), key, count);
        }
        let right = self.nodes[index].right;
        let key = self.nodes[index]
            .key
            .take()
            .expect("minimum node has a key");
        let count = self.nodes[index].count;
        self.release(index);
        (right, key, count)
    }
    fn update(&mut self, root: Option<usize>, key: &K, count: u64) -> Option<usize> {
        let Some(index) = root else {
            return Some(self.allocate(key.clone(), count));
        };
        match key.cmp(
            self.nodes[index]
                .key
                .as_ref()
                .expect("reachable node has a key"),
        ) {
            std::cmp::Ordering::Less => {
                self.nodes[index].left = self.update(self.nodes[index].left, key, count)
            }
            std::cmp::Ordering::Greater => {
                self.nodes[index].right = self.update(self.nodes[index].right, key, count)
            }
            std::cmp::Ordering::Equal => {
                if count != 0 {
                    self.nodes[index].count = count;
                    return Some(index);
                }
                match (self.nodes[index].left, self.nodes[index].right) {
                    (None, next) | (next, None) => {
                        self.release(index);
                        return next;
                    }
                    (Some(_), Some(right)) => {
                        let (next, successor, count) = self.take_min(right);
                        self.nodes[index].right = next;
                        self.nodes[index].key = Some(successor);
                        self.nodes[index].count = count;
                    }
                }
            }
        }
        Some(self.balance(index))
    }
    fn set(&mut self, key: &K, count: u64) {
        let before = self.count(key);
        if before == count {
            return;
        }
        if before == 0 {
            self.len += 1;
        }
        if count == 0 {
            self.len -= 1;
        }
        self.root = self.update(self.root, key, count);
    }
}

/// Reversible exact layout census for one prospective current-contract generation.
///
/// Updates touch only changed records and use fallible vector-backed AVL trees.
/// An accepted prefix is never rescanned when a later file is considered.
#[derive(Debug)]
pub struct OracleLayoutCensus {
    repository: RepositoryId,
    generation: GenerationId,
    totals: Totals,
    identities: CountedTree<RegistryIdentity>,
    sources: CountedTree<SourceRef>,
    payloads: CountedTree<u64>,
}

impl OracleLayoutCensus {
    /// Creates an empty current-contract census for one pinned generation.
    #[must_use]
    pub fn new(repository: RepositoryId, generation: GenerationId) -> Self {
        Self {
            repository,
            generation,
            totals: Totals::default(),
            identities: CountedTree::default(),
            sources: CountedTree::default(),
            payloads: CountedTree::default(),
        }
    }

    /// Returns current resident vector capacity, including reusable vacant slots.
    ///
    /// # Errors
    /// Returns a catalog error on integer overflow.
    pub fn retained_bytes(&self) -> Result<u64, CatalogError> {
        self.identities
            .retained_bytes()?
            .checked_add(self.sources.retained_bytes()?)
            .and_then(|n| n.checked_add(self.payloads.retained_bytes().ok()?))
            .ok_or_else(invalid)
    }

    /// Builds the same restore plan used for a sealed current-contract Oracle.
    ///
    /// # Errors
    /// Propagates cancellation, generation budgets, or census arithmetic errors.
    pub fn read_memory_plan(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReadMemoryPlan, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        let stats = self.totals.stats(self.sources.len)?;
        if self
            .identities
            .len
            .checked_add(1)
            .and_then(|n| u64::try_from(n).ok())
            != Some(
                self.totals
                    .records
                    .into_iter()
                    .try_fold(1_u64, u64::checked_add)
                    .ok_or_else(invalid)?,
            )
        {
            return Err(invalid());
        }
        context
            .require(GenerationResource::Rows, stats.stored_rows())
            .map_err(CatalogError::control)?;
        context
            .require(GenerationResource::SourceReferences, stats.source_refs())
            .map_err(CatalogError::control)?;
        context
            .require(GenerationResource::EncodedTextBytes, stats.text_bytes())
            .map_err(CatalogError::control)?;
        read::oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            stats,
            self.payloads.last().copied().unwrap_or(0),
        )
    }

    /// Previews an owned record transition without committing its matching document.
    ///
    /// `admit` receives absolute simultaneous census/contribution workspace;
    /// callers keep its high-water reservation across all previews. Admission and
    /// allocation errors leave the accepted census intact. A returned preview
    /// rolls back when dropped unless the matching document plan has committed.
    ///
    /// # Errors
    /// Rejects foreign generation identities, duplicate or missing identities,
    /// invalid removals, arithmetic overflow, control, allocation or admission failures.
    pub fn preview<E>(
        &mut self,
        removed: OracleLayoutFragment,
        added: OracleLayoutFragment,
        other_resident_bytes: u64,
        context: &GenerationContext<'_>,
        admit: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<OracleLayoutPreview<'_>, OracleReadAdmissionError<E>> {
        context.check().map_err(CatalogError::control)?;
        for fragment in [&removed, &added] {
            if fragment.repository != self.repository || fragment.generation != self.generation {
                return Err(invalid().into());
            }
        }
        let totals = self.totals.transition(removed.totals, added.totals)?;
        for identity in &removed.identities {
            if self.identities.count(identity) != 1 {
                return Err(invalid().into());
            }
        }
        for identity in &added.identities {
            let removed = removed.identities.binary_search(identity).is_ok();
            if self.identities.count(identity) != u64::from(removed) {
                return Err(invalid().into());
            }
        }
        validate_counts(&self.sources, &removed.sources, &added.sources, context)?;
        validate_counts(&self.payloads, &removed.payloads, &added.payloads, context)?;
        let other = other_resident_bytes
            .checked_add(removed.retained_bytes()?)
            .and_then(|n| n.checked_add(added.retained_bytes().ok()?))
            .ok_or_else(invalid)?;
        admit(
            other
                .checked_add(self.retained_bytes()?)
                .ok_or_else(invalid)?,
        )
        .map_err(OracleReadAdmissionError::Admission)?;
        self.identities.reserve(
            added.identities.len(),
            other
                .checked_add(self.sources.retained_bytes()?)
                .and_then(|n| n.checked_add(self.payloads.retained_bytes().ok()?))
                .ok_or_else(invalid)?,
            admit,
        )?;
        self.sources.reserve(
            added.sources.len(),
            other
                .checked_add(self.identities.retained_bytes()?)
                .and_then(|n| n.checked_add(self.payloads.retained_bytes().ok()?))
                .ok_or_else(invalid)?,
            admit,
        )?;
        self.payloads.reserve(
            added.payloads.len(),
            other
                .checked_add(self.identities.retained_bytes()?)
                .and_then(|n| n.checked_add(self.sources.retained_bytes().ok()?))
                .ok_or_else(invalid)?,
            admit,
        )?;
        context.check().map_err(CatalogError::control)?;
        let before = self.totals;
        change(self, &removed, false);
        change(self, &added, true);
        self.totals = totals;
        Ok(OracleLayoutPreview {
            census: self,
            removed,
            added,
            before,
            committed: false,
        })
    }
}

fn validate_counts<K: Ord + Clone>(
    tree: &CountedTree<K>,
    removed: &[(K, u64)],
    added: &[(K, u64)],
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    for (key, amount) in removed {
        context.check().map_err(CatalogError::control)?;
        if *amount == 0 || tree.count(key) < *amount {
            return Err(invalid());
        }
    }
    for (key, amount) in added {
        context.check().map_err(CatalogError::control)?;
        let previous = tree.count(key);
        let removal = removed
            .binary_search_by(|(candidate, _)| candidate.cmp(key))
            .ok()
            .map_or(0, |index| removed[index].1);
        if *amount == 0
            || previous
                .checked_sub(removal)
                .and_then(|n| n.checked_add(*amount))
                .is_none()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn change(census: &mut OracleLayoutCensus, fragment: &OracleLayoutFragment, add: bool) {
    for identity in &fragment.identities {
        census.identities.set(identity, u64::from(add));
    }
    for (key, count) in &fragment.sources {
        let before = census.sources.count(key);
        census
            .sources
            .set(key, if add { before + count } else { before - count });
    }
    for (key, count) in &fragment.payloads {
        let before = census.payloads.count(key);
        census
            .payloads
            .set(key, if add { before + count } else { before - count });
    }
}

/// A private-census transaction that owns its rollback until explicitly committed.
#[derive(Debug)]
pub struct OracleLayoutPreview<'a> {
    census: &'a mut OracleLayoutCensus,
    removed: OracleLayoutFragment,
    added: OracleLayoutFragment,
    before: Totals,
    committed: bool,
}

impl OracleLayoutPreview<'_> {
    /// Returns the current hypothetical plan without changing document ownership.
    ///
    /// # Errors
    /// Returns the same census/control errors as [`OracleLayoutCensus::read_memory_plan`].
    pub fn read_memory_plan(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReadMemoryPlan, CatalogError> {
        self.census.read_memory_plan(context)
    }

    /// Retains the census transition after its matching owned document plan commits.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for OracleLayoutPreview<'_> {
    fn drop(&mut self) {
        if !self.committed {
            change(self.census, &self.added, false);
            change(self.census, &self.removed, true);
            self.census.totals = self.before;
        }
    }
}

#[cfg(test)]
#[path = "layout_census/tests.rs"]
mod tests;
