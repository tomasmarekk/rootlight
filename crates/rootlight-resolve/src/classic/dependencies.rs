//! Orders exact member/application dependencies before their consumers.
//! With receivers live outside the consumer span, so expression length is not
//! an ordering proof. Cycles and duplicate evidence never trigger retry passes.

use super::{calls::CallIndex, with_members::Member};
use crate::{ResolutionError, engine::ResolutionWorkBudget};
use rootlight_cancel::Cancellation;
use rootlight_ids::FileId;
use rootlight_ir::{OccurrenceRecord, SourceSpan};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn order<'o, 's>(
    events: &[(&'o OccurrenceRecord, Option<Member<'s>>)],
    calls: &CallIndex<'_, 'o>,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Vec<usize>, ResolutionError> {
    let mut members: BTreeMap<SourceSpan, Vec<usize>> = BTreeMap::new();
    let mut applications: BTreeMap<SourceSpan, Vec<usize>> = BTreeMap::new();
    for (i, (site, member)) in events.iter().enumerate() {
        cancellation.check()?;
        work.consume()?;
        let map = if member.is_some() {
            &mut members
        } else {
            &mut applications
        };
        map.entry(site.source.span()).or_default().push(i);
    }
    let mut incoming = vec![0_usize; events.len()];
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); events.len()];
    let mut ready = BTreeSet::new();
    for (i, (site, member)) in events.iter().enumerate() {
        cancellation.check()?;
        work.consume()?;
        let dependencies = if let Some(member) = member {
            applications
                .get(&member.span)
                .or_else(|| members.get(&member.span))
        } else if let Some((callee, _)) = calls.callee(site, sources, cancellation, work)? {
            members.get(&callee.source.span())
        } else {
            calls
                .returned_receiver(site, cancellation, work)?
                .and_then(|receiver| applications.get(&receiver.source.span()))
        };
        for &dependency in dependencies.into_iter().flatten() {
            work.consume()?;
            if let (Some(incoming), Some(consumers)) =
                (incoming.get_mut(i), consumers.get_mut(dependency))
            {
                *incoming += 1;
                consumers.push(i);
            }
        }
        if incoming.get(i) == Some(&0) {
            ready.insert(i);
        }
    }
    let mut ordered = Vec::new();
    while let Some(i) = ready.pop_first() {
        cancellation.check()?;
        work.consume()?;
        ordered.push(i);
        for &consumer in consumers.get(i).into_iter().flatten() {
            work.consume()?;
            if let Some(count) = incoming.get_mut(consumer) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(consumer);
                }
            }
        }
    }
    Ok(ordered)
}
