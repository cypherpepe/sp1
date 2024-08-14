use p3_air::{Air, BaseAir};
use p3_baby_bear::BabyBear;
use p3_commit::PolynomialSpace;
use p3_commit::{LagrangeSelectors, Mmcs, TwoAdicMultiplicativeCoset};
use p3_field::TwoAdicField;
use p3_field::{AbstractExtensionField, AbstractField};
use p3_matrix::dense::RowMajorMatrix;

use sp1_core::air::MachineAir;
use sp1_core::stark::{AirOpenedValues, GenericVerifierConstraintFolder, MachineChip};
use sp1_core::stark::{ChipOpenedValues, OpeningShapeError};
use sp1_recursion_compiler::ir::{Builder, Config, Ext, Felt, SymbolicExt};

use crate::{domain::PolynomialSpaceVariable, stark::StarkVerifier, BabyBearFriConfigVariable};

pub type RecursiveVerifierConstraintFolder<'a, C> = GenericVerifierConstraintFolder<
    'a,
    <C as Config>::F,
    <C as Config>::EF,
    Felt<<C as Config>::F>,
    Ext<<C as Config>::F, <C as Config>::EF>,
    SymbolicExt<<C as Config>::F, <C as Config>::EF>,
>;

impl<C, SC> StarkVerifier<C, SC>
where
    C::F: TwoAdicField,
    SC: BabyBearFriConfigVariable<C = C>,
    C: Config<F = SC::Val>,
    <SC::ValMmcs as Mmcs<BabyBear>>::ProverData<RowMajorMatrix<BabyBear>>: Clone,
{
    #[allow(clippy::too_many_arguments)]
    pub fn verify_constraints<A>(
        builder: &mut Builder<C>,
        chip: &MachineChip<SC, A>,
        opening: &ChipOpenedValues<Ext<C::F, C::EF>>,
        trace_domain: TwoAdicMultiplicativeCoset<C::F>,
        qc_domains: Vec<TwoAdicMultiplicativeCoset<C::F>>,
        zeta: Ext<C::F, C::EF>,
        alpha: Ext<C::F, C::EF>,
        permutation_challenges: &[Ext<C::F, C::EF>],
        public_values: &[Felt<C::F>],
    ) where
        A: for<'a> Air<RecursiveVerifierConstraintFolder<'a, C>>,
    {
        let sels = trace_domain.selectors_at_point_variable(builder, zeta);

        // Recompute the quotient at zeta from the chunks.
        let quotient = Self::recompute_quotient(builder, opening, &qc_domains, zeta);

        // Calculate the evaluations of the constraints at zeta.
        let folded_constraints = Self::eval_constraints(
            builder,
            chip,
            opening,
            &sels,
            alpha,
            permutation_challenges,
            public_values,
        );

        // Assert that the quotient times the zerofier is equal to the folded constraints.
        builder.assert_ext_eq(folded_constraints * sels.inv_zeroifier, quotient);
    }

    pub fn eval_constraints<A>(
        builder: &mut Builder<C>,
        chip: &MachineChip<SC, A>,
        opening: &ChipOpenedValues<Ext<C::F, C::EF>>,
        selectors: &LagrangeSelectors<Ext<C::F, C::EF>>,
        alpha: Ext<C::F, C::EF>,
        permutation_challenges: &[Ext<C::F, C::EF>],
        public_values: &[Felt<C::F>],
    ) -> Ext<C::F, C::EF>
    where
        A: for<'a> Air<RecursiveVerifierConstraintFolder<'a, C>>,
    {
        let mut unflatten = |v: &[Ext<C::F, C::EF>]| {
            v.chunks_exact(<SC::Challenge as AbstractExtensionField<C::F>>::D)
                .map(|chunk| {
                    builder.eval(
                        chunk
                            .iter()
                            .enumerate()
                            .map(
                                |(e_i, x): (usize, &Ext<C::F, C::EF>)| -> SymbolicExt<C::F, C::EF> {
                                    SymbolicExt::from(*x) * C::EF::monomial(e_i)
                                },
                            )
                            .sum::<SymbolicExt<_, _>>(),
                    )
                })
                .collect::<Vec<Ext<_, _>>>()
        };
        let perm_opening = AirOpenedValues {
            local: unflatten(&opening.permutation.local),
            next: unflatten(&opening.permutation.next),
        };

        let mut folder = RecursiveVerifierConstraintFolder::<C> {
            preprocessed: opening.preprocessed.view(),
            main: opening.main.view(),
            perm: perm_opening.view(),
            perm_challenges: permutation_challenges,
            cumulative_sum: opening.cumulative_sum,
            public_values,
            is_first_row: selectors.is_first_row,
            is_last_row: selectors.is_last_row,
            is_transition: selectors.is_transition,
            alpha,
            accumulator: SymbolicExt::zero(),
            _marker: std::marker::PhantomData,
        };

        chip.eval(&mut folder);
        builder.eval(folder.accumulator)
    }

    pub fn recompute_quotient(
        builder: &mut Builder<C>,
        opening: &ChipOpenedValues<Ext<C::F, C::EF>>,
        qc_domains: &[TwoAdicMultiplicativeCoset<C::F>],
        zeta: Ext<C::F, C::EF>,
    ) -> Ext<C::F, C::EF> {
        let zps = qc_domains
            .iter()
            .enumerate()
            .map(|(i, domain)| {
                qc_domains
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, other_domain)| {
                        let first_point = builder.eval(domain.first_point());
                        other_domain.zp_at_point_variable(builder, zeta)
                            * other_domain
                                .zp_at_point_variable(builder, first_point)
                                .inverse()
                    })
                    .product::<SymbolicExt<_, _>>()
            })
            .collect::<Vec<SymbolicExt<_, _>>>()
            .into_iter()
            .map(|x| builder.eval(x))
            .collect::<Vec<Ext<_, _>>>();

        builder.eval(
            opening
                .quotient
                .iter()
                .enumerate()
                .map(|(ch_i, ch)| {
                    assert_eq!(ch.len(), C::EF::D);
                    ch.iter()
                        .enumerate()
                        .map(|(e_i, &c)| zps[ch_i] * C::EF::monomial(e_i) * c)
                        .sum::<SymbolicExt<_, _>>()
                })
                .sum::<SymbolicExt<_, _>>(),
        )
    }

    pub fn verify_opening_shape<A>(
        chip: &MachineChip<SC, A>,
        opening: &ChipOpenedValues<Ext<C::F, C::EF>>,
    ) -> Result<(), OpeningShapeError>
    where
        A: MachineAir<C::F> + for<'a> Air<RecursiveVerifierConstraintFolder<'a, C>>,
    {
        // Verify that the preprocessed width matches the expected value for the chip.
        if opening.preprocessed.local.len() != chip.preprocessed_width() {
            return Err(OpeningShapeError::PreprocessedWidthMismatch(
                chip.preprocessed_width(),
                opening.preprocessed.local.len(),
            ));
        }
        if opening.preprocessed.next.len() != chip.preprocessed_width() {
            return Err(OpeningShapeError::PreprocessedWidthMismatch(
                chip.preprocessed_width(),
                opening.preprocessed.next.len(),
            ));
        }

        // Verify that the main width matches the expected value for the chip.
        if opening.main.local.len() != chip.width() {
            return Err(OpeningShapeError::MainWidthMismatch(
                chip.width(),
                opening.main.local.len(),
            ));
        }
        if opening.main.next.len() != chip.width() {
            return Err(OpeningShapeError::MainWidthMismatch(
                chip.width(),
                opening.main.next.len(),
            ));
        }

        // Verify that the permutation width matches the expected value for the chip.
        if opening.permutation.local.len()
            != chip.permutation_width() * <SC::Challenge as AbstractExtensionField<C::F>>::D
        {
            return Err(OpeningShapeError::PermutationWidthMismatch(
                chip.permutation_width(),
                opening.permutation.local.len(),
            ));
        }
        if opening.permutation.next.len()
            != chip.permutation_width() * <SC::Challenge as AbstractExtensionField<C::F>>::D
        {
            return Err(OpeningShapeError::PermutationWidthMismatch(
                chip.permutation_width(),
                opening.permutation.next.len(),
            ));
        }

        // Verift that the number of quotient chunks matches the expected value for the chip.
        if opening.quotient.len() != chip.quotient_width() {
            return Err(OpeningShapeError::QuotientWidthMismatch(
                chip.quotient_width(),
                opening.quotient.len(),
            ));
        }
        // For each quotient chunk, verify that the number of elements is equal to the degree of the
        // challenge extension field over the value field.
        for slice in &opening.quotient {
            if slice.len() != <SC::Challenge as AbstractExtensionField<C::F>>::D {
                return Err(OpeningShapeError::QuotientChunkSizeMismatch(
                    <SC::Challenge as AbstractExtensionField<C::F>>::D,
                    slice.len(),
                ));
            }
        }

        Ok(())
    }
}
