//! A pass that eliminates branches on uninhabited enum variants.

use crate::transform::{MirPass, MirSource};
use rustc::mir::{BasicBlock, Body, Local, Operand, Rvalue, StatementKind, TerminatorKind};
use rustc::ty::layout::{Abi, TyLayout, Variants};
use rustc::ty::{Ty, TyCtxt};

pub struct UninhabitedEnumBranching;

fn get_discriminant_local(terminator: &TerminatorKind<'_>) -> Option<Local> {
    if let TerminatorKind::SwitchInt { discr: Operand::Move(p), .. } = terminator {
        p.as_local()
    } else {
        None
    }
}

/// Finds all basic blocks which terminate by switching on a discriminant.
/// Returns a `Vec` of block ids and the `Ty` the discriminant is read from.
fn find_eligible_blocks<'tcx>(body: &Body<'tcx>) -> Vec<(BasicBlock, Ty<'tcx>)> {
    let mut blocks_to_update = Vec::new();

    for (bb, block_data) in body.basic_blocks().iter_enumerated() {
        let terminator = block_data.terminator();

        // Only bother checking blocks which terminate by switching on a local.
        if let Some(local) = get_discriminant_local(&terminator.kind) {
            let stmt_before_term = (block_data.statements.len() > 0)
                .then_with(|| &block_data.statements[block_data.statements.len() - 1].kind);

            if let Some(StatementKind::Assign(box (l, Rvalue::Discriminant(place)))) =
                stmt_before_term
            {
                if l.as_local() == Some(local) {
                    if let Some(r_local) = place.as_local() {
                        let ty = body.local_decls[r_local].ty;

                        if ty.is_enum() {
                            blocks_to_update.push((bb, ty));
                        }
                    }
                }
            }
        }
    }

    blocks_to_update
}

fn variant_discriminants<'tcx>(
    layout: &TyLayout<'tcx>,
    ty: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Vec<u128> {
    match &layout.details.variants {
        Variants::Single { index } => vec![index.as_u32() as u128],
        Variants::Multiple { variants, .. } => variants
            .iter_enumerated()
            .filter_map(|(idx, layout)| {
                (layout.abi != Abi::Uninhabited)
                    .then_with(|| ty.discriminant_for_variant(tcx, idx).unwrap().val)
            })
            .collect(),
    }
}

impl<'tcx> MirPass<'tcx> for UninhabitedEnumBranching {
    fn run_pass(&self, tcx: TyCtxt<'tcx>, source: MirSource<'tcx>, body: &mut Body<'tcx>) {
        if source.promoted.is_some() {
            return;
        }

        trace!("UninhabitedEnumBranching starting for {:?}", source);

        let blocks_to_update = find_eligible_blocks(&body);

        for (bb, discriminant_ty) in blocks_to_update {
            trace!("processing block {:?}", bb);
            let block_data = &mut body[bb];

            let layout = tcx.layout_of(tcx.param_env(source.def_id()).and(discriminant_ty));

            let allowed_variants = if let Ok(layout) = layout {
                variant_discriminants(&layout, discriminant_ty, tcx)
            } else {
                continue;
            };

            trace!("allowed_variants = {:?}", allowed_variants);

            if let TerminatorKind::SwitchInt { values, targets, .. } =
                &mut block_data.terminator_mut().kind
            {
                let vals = &*values;
                let zipped = vals.iter().zip(targets.into_iter());

                let mut matched_values = Vec::with_capacity(allowed_variants.len());
                let mut matched_targets = Vec::with_capacity(allowed_variants.len() + 1);

                for (val, target) in zipped {
                    if allowed_variants.contains(val) {
                        matched_values.push(*val);
                        matched_targets.push(*target);
                    } else {
                        trace!("eliminating {:?} -> {:?}", val, target);
                    }
                }

                // handle the "otherwise" branch
                matched_targets.push(targets.pop().unwrap());

                *values = matched_values.into();
                *targets = matched_targets;
            } else {
                unreachable!()
            }
        }
    }
}
