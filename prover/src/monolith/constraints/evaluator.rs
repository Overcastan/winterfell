// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use super::{
    BoundaryConstraintGroup, ConstraintEvaluationTable, PeriodicValueTable, StarkDomain, TraceTable,
};
use common::{Air, ConstraintDivisor, EvaluationFrame, PublicCoin, TransitionConstraintGroup};
use math::field::FieldElement;
use std::collections::HashMap;

#[cfg(feature = "concurrent")]
use rayon::prelude::*;

// CONSTANTS
// ================================================================================================

const MIN_CONCURRENT_DOMAIN_SIZE: usize = 8192;

// CONSTRAINT EVALUATOR
// ================================================================================================

pub struct ConstraintEvaluator<A: Air, E: FieldElement + From<A::BaseElement>> {
    air: A,
    boundary_constraints: Vec<BoundaryConstraintGroup<A::BaseElement, E>>,
    transition_constraints: Vec<TransitionConstraintGroup<E>>,
    periodic_values: PeriodicValueTable<A::BaseElement>,
    divisors: Vec<ConstraintDivisor<A::BaseElement>>,

    #[cfg(debug_assertions)]
    transition_constraint_degrees: Vec<usize>,
}

impl<A: Air, E: FieldElement + From<A::BaseElement>> ConstraintEvaluator<A, E> {
    // CONSTRUCTOR
    // --------------------------------------------------------------------------------------------
    /// Returns a new evaluator which can be used to evaluate transition and boundary constraints
    /// over extended execution trace.
    pub fn new<C: PublicCoin>(air: A, coin: &C) -> Self {
        // collect expected degrees for all transition constraints to compare them against actual
        // degrees; we do this in debug mode only because this comparison is expensive
        #[cfg(debug_assertions)]
        let transition_constraint_degrees = air
            .context()
            .transition_constraint_degrees()
            .iter()
            .map(|d| d.get_evaluation_degree(air.context().trace_length()))
            .collect();

        // build transition constraint groups; these will be used later to compute a random
        // linear combination of transition constraint evaluations.
        let transition_constraints =
            air.get_transition_constraints(coin.get_transition_coefficient_prng());

        // build periodic value table
        let periodic_values = PeriodicValueTable::new(&air);

        // set divisor for transition constraints; since divisors for all transition constraints
        // are the same: (x^steps - 1) / (x - x_at_last_step), all transition constraints will be
        // merged into a single value, and the divisor for that value will be first in the list
        let mut divisors = vec![ConstraintDivisor::from_transition(air.context())];

        // build boundary constraints and also append divisors for each group of boundary
        // constraints to the divisor list
        let mut twiddle_map = HashMap::new();
        let boundary_constraints = air
            .get_boundary_constraints(coin.get_boundary_coefficient_prng())
            .into_iter()
            .map(|group| {
                divisors.push(group.divisor().clone());
                BoundaryConstraintGroup::new(group, air.context(), &mut twiddle_map)
            })
            .collect();

        ConstraintEvaluator {
            air,
            boundary_constraints,
            transition_constraints,
            periodic_values,
            divisors,
            #[cfg(debug_assertions)]
            transition_constraint_degrees,
        }
    }

    // EVALUATOR
    // --------------------------------------------------------------------------------------------
    /// Evaluates constraints against the provided extended execution trace. Constraints
    /// are evaluated over a constraint evaluation domain. This is an optimization because
    /// constraint evaluation domain can be many times smaller than the full LDE domain.
    pub fn evaluate(
        &self,
        trace: &TraceTable<A::BaseElement>,
        domain: &StarkDomain<A::BaseElement>,
    ) -> ConstraintEvaluationTable<A::BaseElement, E> {
        assert_eq!(
            trace.len(),
            domain.lde_domain_size(),
            "extended trace length is not consistent with evaluation domain"
        );
        // allocate space for constraint evaluations; when we are in debug mode, we also allocate
        // memory to hold all transition constraint evaluations (before they are merged into a
        // single value) so that we can check their degree late
        #[cfg(not(debug_assertions))]
        let mut evaluation_table =
            ConstraintEvaluationTable::<A::BaseElement, E>::new(domain, self.divisors.clone());
        #[cfg(debug_assertions)]
        let mut evaluation_table = ConstraintEvaluationTable::<A::BaseElement, E>::new(
            domain,
            self.divisors.clone(),
            self.transition_constraint_degrees.to_vec(),
        );

        // when `concurrent` feature is enabled, evaluate constraints in multiple threads,
        // unless the constraint evaluation domain is small, then don't bother with concurrent
        // evaluation
        if cfg!(feature = "concurrent") && domain.ce_domain_size() >= MIN_CONCURRENT_DOMAIN_SIZE {
            #[cfg(feature = "concurrent")]
            self.evaluate_concurrent(trace, domain, &mut evaluation_table);
        } else {
            self.evaluate_sequential(trace, domain, &mut evaluation_table);
        }

        // when in debug mode, make sure expected transition constraint degrees align with
        // actual degrees we got during constraint evaluation
        #[cfg(debug_assertions)]
        evaluation_table.validate_transition_degrees();

        evaluation_table
    }

    // EVALUATION HELPERS
    // --------------------------------------------------------------------------------------------

    /// Evaluates the constraints in a single thread and saves the result into `evaluation_table`.
    pub fn evaluate_sequential(
        &self,
        trace: &TraceTable<A::BaseElement>,
        domain: &StarkDomain<A::BaseElement>,
        evaluation_table: &mut ConstraintEvaluationTable<A::BaseElement, E>,
    ) {
        // initialize buffers to hold trace values and evaluation results at each step
        let mut ev_frame = EvaluationFrame::new(trace.width());
        let mut evaluations = vec![E::ZERO; evaluation_table.num_columns()];
        let mut t_evaluations = vec![A::BaseElement::ZERO; self.air.num_transition_constraints()];

        for step in 0..evaluation_table.num_rows() {
            // translate steps in the constraint evaluation domain to steps in LDE domain
            let (lde_step, x) = domain.ce_step_to_lde_info(step);

            // update evaluation frame buffer with data from the execution trace; this will
            // read current and next rows from the trace into the buffer
            trace.read_frame_into(lde_step, &mut ev_frame);

            // evaluate transition constraints and save the merged result the first slot of the
            // evaluations buffer
            evaluations[0] =
                self.evaluate_transition_constraints(&ev_frame, x, step, &mut t_evaluations);

            // when in debug mode, save transition constraint evaluations
            #[cfg(all(debug_assertions, not(feature = "concurrent")))]
            evaluation_table.update_transition_evaluations(step, &t_evaluations);

            // evaluate boundary constraints; the results go into remaining slots of the
            // evaluations buffer
            self.evaluate_boundary_constraints(&ev_frame.current, x, step, &mut evaluations[1..]);

            // record the result in the evaluation table
            evaluation_table.update_row(step, &evaluations);
        }
    }

    /// Evaluates the constraints in multiple threads (usually as many threads as are available
    /// in rayon's global thread pool) and saves the result into `evaluation_table`. The evaluation
    /// is done by breaking the evaluation table into multiple fragments and processing each
    /// fragment in a separate thread.
    #[cfg(feature = "concurrent")]
    fn evaluate_concurrent(
        &self,
        trace: &TraceTable<A::BaseElement>,
        domain: &StarkDomain<A::BaseElement>,
        evaluation_table: &mut ConstraintEvaluationTable<A::BaseElement, E>,
    ) {
        let num_evaluation_columns = evaluation_table.num_columns();
        let num_fragments = rayon::current_num_threads().next_power_of_two();

        evaluation_table
            .fragments(num_fragments)
            .par_iter_mut()
            .for_each(|fragment| {
                // initialize buffers to hold trace values and evaluation results at each
                // step; in concurrent mode we do this separately for each fragment
                let mut ev_frame = EvaluationFrame::new(trace.width());
                let mut evaluations = vec![E::ZERO; num_evaluation_columns];
                let mut t_evaluations =
                    vec![A::BaseElement::ZERO; self.air.num_transition_constraints()];

                for i in 0..fragment.num_rows() {
                    let step = i + fragment.offset();

                    // translate steps in the constraint evaluation domain to steps in LDE domain
                    let (lde_step, x) = domain.ce_step_to_lde_info(step);

                    // update evaluation frame buffer with data from the execution trace;
                    // this will read current and next rows from the trace into the buffer
                    trace.read_frame_into(lde_step, &mut ev_frame);

                    // evaluate transition constraints and save the merged result the
                    // first slot of the evaluations buffer
                    evaluations[0] = self.evaluate_transition_constraints(
                        &ev_frame,
                        x,
                        step,
                        &mut t_evaluations,
                    );

                    // TODO: in debug mode, save t_evaluations into the fragment

                    // evaluate boundary constraints; the results go into remaining slots
                    // of the evaluations buffer
                    let current_state = &ev_frame.current;
                    self.evaluate_boundary_constraints(
                        current_state,
                        x,
                        step,
                        &mut evaluations[1..],
                    );

                    // record the result in the evaluation table
                    fragment.update_row(i, &evaluations);
                }
            });
    }

    /// Evaluates transition constraints at the specified step of the execution trace. `step` is
    /// the step in the constraint evaluation, and `x` is the corresponding domain value. That
    /// is, x = s * g^step, where g is the generator of the constraint evaluation domain, and s
    /// is the domain offset.
    fn evaluate_transition_constraints(
        &self,
        frame: &EvaluationFrame<A::BaseElement>,
        x: A::BaseElement,
        step: usize,
        evaluations: &mut [A::BaseElement],
    ) -> E {
        // TODO: use a more efficient way to zero out memory
        evaluations.fill(A::BaseElement::ZERO);

        // get periodic values at the evaluation step
        let periodic_values = self.periodic_values.get_row(step);

        // evaluate transition constraints and save the results into evaluations buffer
        self.air
            .evaluate_transition(frame, periodic_values, evaluations);

        // merge transition constraint evaluations into a single value and return it;
        // we can do this here because all transition constraints have the same divisor.
        self.transition_constraints
            .iter()
            .fold(E::ZERO, |result, group| {
                result + group.merge_evaluations(evaluations, x)
            })
    }

    /// Evaluates all boundary constraint groups at a specific step of the execution trace.
    /// `step` is the step in the constraint evaluation domain, and `x` is the corresponding
    /// domain value. That is, x = s * g^step, where g is the generator of the constraint
    /// evaluation domain, and s is the domain offset.
    fn evaluate_boundary_constraints(
        &self,
        state: &[A::BaseElement],
        x: A::BaseElement,
        step: usize,
        result: &mut [E],
    ) {
        // compute the adjustment degree outside of the group so that we can re-use
        // it for groups which have the same adjustment degree
        let mut degree_adjustment = self.boundary_constraints[0].degree_adjustment;
        let mut xp = E::from(x.exp(degree_adjustment.into()));

        for (group, result) in self.boundary_constraints.iter().zip(result.iter_mut()) {
            // recompute adjustment degree only when it has changed
            if group.degree_adjustment != degree_adjustment {
                degree_adjustment = group.degree_adjustment;
                xp = E::from(x.exp(degree_adjustment.into()));
            }
            // evaluate the group and save the result
            *result = group.evaluate(state, step, x, xp);
        }
    }
}