//! This library implements Nova, a high-speed recursive SNARK.
#![cfg_attr(not(feature = "std"), no_std)]
#![deny(
  //warnings,
  //unused, TODO: bring this back
  future_incompatible,
  nonstandard_style,
  rust_2018_idioms,
  missing_docs
)]
#![allow(non_snake_case)]
#![forbid(unsafe_code)]

#[cfg(not(feature = "std"))]
extern crate alloc;

// TODO -> Put cfg flag on mod, and not on imports inside mod
pub(crate) mod prelude {
  #[cfg(not(feature = "std"))]
  pub use alloc::borrow::ToOwned;
  #[cfg(not(feature = "std"))]
  pub use alloc::boxed::Box;
  #[cfg(not(feature = "std"))]
  pub use alloc::format;
  #[cfg(not(feature = "std"))]
  pub use alloc::string::String;
  #[cfg(not(feature = "std"))]
  pub use alloc::string::ToString;
  #[cfg(not(feature = "std"))]
  pub use alloc::vec;
  #[cfg(not(feature = "std"))]
  pub use alloc::vec::Vec;

  // use std::collections::{BTreeMap, HashMap} when std is enabled
  #[cfg(not(feature = "std"))]
  pub use alloc::collections::BTreeMap;
  #[cfg(not(feature = "std"))]
  pub use alloc::collections::VecDeque;
  #[cfg(feature = "std")]
  pub use std::collections::{BTreeMap, HashMap, VecDeque};
}

// private modules
mod circuit;
mod constants;
mod digest;
mod nifs;
mod r1cs;

// public modules
pub mod errors;
pub mod frontend;
pub mod gadgets;
pub mod provider;
pub mod spartan;
pub mod traits;

use crate::digest::{DigestComputer, SimpleDigestible};
use circuit::{NovaAugmentedCircuit, NovaAugmentedCircuitInputs, NovaAugmentedCircuitParams};
use constants::{BN_LIMB_WIDTH, BN_N_LIMBS, NUM_FE_WITHOUT_IO_FOR_CRHF, NUM_HASH_BITS};
use core::marker::PhantomData;
use errors::NovaError;
use ff::Field;
use frontend::{
  r1cs::{NovaShape, NovaWitness},
  shape_cs::ShapeCS,
  solver::SatisfyingAssignment,
  ConstraintSystem, SynthesisError,
};
use gadgets::utils::scalar_as_base;
use nifs::{NIFSRelaxed, NIFS};
// use once_cell::sync::OnceCell;
use prelude::*;
use r1cs::{
  CommitmentKeyHint, R1CSInstance, R1CSShape, R1CSWitness, RelaxedR1CSInstance, RelaxedR1CSWitness,
};

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
// use rand_core::OsRng;

use serde::{Deserialize, Serialize};
use traits::{
  circuit::StepCircuit, commitment::CommitmentEngineTrait, snark::RelaxedR1CSSNARKTrait,
  AbsorbInROTrait, Engine, ROConstants, ROConstantsCircuit, ROTrait,
};

/// A type that holds public parameters of Nova
#[derive(Serialize, Deserialize, Clone)]
#[serde(bound = "")]
pub struct PublicParams<E1, E2, C1, C2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
{
  F_arity_primary: usize,
  F_arity_secondary: usize,
  ro_consts_primary: ROConstants<E1>,
  ro_consts_circuit_primary: ROConstantsCircuit<E2>,
  ck_primary: CommitmentKey<E1>,
  r1cs_shape_primary: R1CSShape<E1>,
  ro_consts_secondary: ROConstants<E2>,
  ro_consts_circuit_secondary: ROConstantsCircuit<E1>,
  ck_secondary: CommitmentKey<E2>,
  r1cs_shape_secondary: R1CSShape<E2>,
  augmented_circuit_params_primary: NovaAugmentedCircuitParams,
  augmented_circuit_params_secondary: NovaAugmentedCircuitParams,
  #[serde(skip)]
  digest: Option<E1::Scalar>,
  _p: PhantomData<(C1, C2)>,
}

impl<E1, E2, C1, C2> SimpleDigestible for PublicParams<E1, E2, C1, C2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
{
}

impl<E1, E2, C1, C2> PublicParams<E1, E2, C1, C2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
{
  /// Creates a new `PublicParams` for a pair of circuits `C1` and `C2`.
  ///
  /// # Note
  ///
  /// Public parameters set up a number of bases for the homomorphic commitment scheme of Nova.
  ///
  /// Some final compressing SNARKs, like variants of Spartan, use computation commitments that require
  /// larger sizes for these parameters. These SNARKs provide a hint for these values by
  /// implementing `RelaxedR1CSSNARKTrait::ck_floor()`, which can be passed to this function.
  ///
  /// If you're not using such a SNARK, pass `nova_snark::traits::snark::default_ck_hint()` instead.
  ///
  /// # Arguments
  ///
  /// * `c_primary`: The primary circuit of type `C1`.
  /// * `c_secondary`: The secondary circuit of type `C2`.
  /// * `ck_hint1`: A `CommitmentKeyHint` for `G1`, which is a function that provides a hint
  ///   for the number of generators required in the commitment scheme for the primary circuit.
  /// * `ck_hint2`: A `CommitmentKeyHint` for `G2`, similar to `ck_hint1`, but for the secondary circuit.
  ///
  /// # Example
  ///
  /// ```rust
  /// # use nova_snark::spartan::ppsnark::RelaxedR1CSSNARK;
  /// # use nova_snark::provider::ipa_pc::EvaluationEngine;
  /// # use nova_snark::provider::{PallasEngine, VestaEngine};
  /// # use nova_snark::traits::{circuit::TrivialCircuit, Engine, snark::RelaxedR1CSSNARKTrait};
  /// use nova_snark::PublicParams;
  ///
  /// type E1 = PallasEngine;
  /// type E2 = VestaEngine;
  /// type EE<E> = EvaluationEngine<E>;
  /// type SPrime<E> = RelaxedR1CSSNARK<E, EE<E>>;
  ///
  /// let circuit1 = TrivialCircuit::<<E1 as Engine>::Scalar>::default();
  /// let circuit2 = TrivialCircuit::<<E2 as Engine>::Scalar>::default();
  /// // Only relevant for a SNARK using computational commitments, pass &(|_| 0)
  /// // or &*nova_snark::traits::snark::default_ck_hint() otherwise.
  /// let ck_hint1 = &*SPrime::<E1>::ck_floor();
  /// let ck_hint2 = &*SPrime::<E2>::ck_floor();
  ///
  /// let pp = PublicParams::setup(&circuit1, &circuit2, ck_hint1, ck_hint2);
  /// ```
  pub fn setup(
    c_primary: &C1,
    c_secondary: &C2,
    ck_hint1: &CommitmentKeyHint<E1>,
    ck_hint2: &CommitmentKeyHint<E2>,
  ) -> Result<Self, NovaError> {
    let augmented_circuit_params_primary =
      NovaAugmentedCircuitParams::new(BN_LIMB_WIDTH, BN_N_LIMBS, true);
    let augmented_circuit_params_secondary =
      NovaAugmentedCircuitParams::new(BN_LIMB_WIDTH, BN_N_LIMBS, false);

    let ro_consts_primary: ROConstants<E1> = ROConstants::<E1>::default();
    let ro_consts_secondary: ROConstants<E2> = ROConstants::<E2>::default();

    let F_arity_primary = c_primary.arity();
    let F_arity_secondary = c_secondary.arity();

    // ro_consts_circuit_primary are parameterized by E2 because the type alias uses E2::Base = E1::Scalar
    let ro_consts_circuit_primary: ROConstantsCircuit<E2> = ROConstantsCircuit::<E2>::default();
    let ro_consts_circuit_secondary: ROConstantsCircuit<E1> = ROConstantsCircuit::<E1>::default();

    // Initialize ck for the primary
    let circuit_primary: NovaAugmentedCircuit<'_, E2, C1> = NovaAugmentedCircuit::new(
      &augmented_circuit_params_primary,
      None,
      c_primary,
      ro_consts_circuit_primary.clone(),
    );
    let mut cs: ShapeCS<E1> = ShapeCS::new();
    let _ = circuit_primary.synthesize(&mut cs);
    let (r1cs_shape_primary, ck_primary) = cs.r1cs_shape(ck_hint1);

    // Initialize ck for the secondary
    let circuit_secondary: NovaAugmentedCircuit<'_, E1, C2> = NovaAugmentedCircuit::new(
      &augmented_circuit_params_secondary,
      None,
      c_secondary,
      ro_consts_circuit_secondary.clone(),
    );
    let mut cs: ShapeCS<E2> = ShapeCS::new();
    let _ = circuit_secondary.synthesize(&mut cs);
    let (r1cs_shape_secondary, ck_secondary) = cs.r1cs_shape(ck_hint2);

    if r1cs_shape_primary.num_io != 2 || r1cs_shape_secondary.num_io != 2 {
      return Err(NovaError::InvalidStepCircuitIO);
    }

    let mut pp = PublicParams {
      F_arity_primary,
      F_arity_secondary,
      ro_consts_primary,
      ro_consts_circuit_primary,
      ck_primary,
      r1cs_shape_primary,
      ro_consts_secondary,
      ro_consts_circuit_secondary,
      ck_secondary,
      r1cs_shape_secondary,
      augmented_circuit_params_primary,
      augmented_circuit_params_secondary,
      digest: None,
      _p: Default::default(),
    };

    // call pp.digest() so the digest is computed here rather than in RecursiveSNARK methods
    pp.digest();

    Ok(pp)
  }

  /// Retrieve the digest of the public parameters.
  pub fn digest(&mut self) -> E1::Scalar {
    if self.digest.is_none() {
      let computed_digest = DigestComputer::new(self)
        .digest()
        .expect("Failure in retrieving digest");
      self.digest = Some(computed_digest);
    }
    self.digest.unwrap()

    //   self
    //     .digest
    //     .get_or_try_init(|| DigestComputer::new(self).digest())
    //     .cloned()
    //     .expect("Failure in retrieving digest")
  }

  /// Returns the number of constraints in the primary and secondary circuits
  pub const fn num_constraints(&self) -> (usize, usize) {
    (
      self.r1cs_shape_primary.num_cons,
      self.r1cs_shape_secondary.num_cons,
    )
  }

  /// Returns the number of variables in the primary and secondary circuits
  pub const fn num_variables(&self) -> (usize, usize) {
    (
      self.r1cs_shape_primary.num_vars,
      self.r1cs_shape_secondary.num_vars,
    )
  }
}

/// A SNARK that proves the correct execution of an incremental computation
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct RecursiveSNARK<E1, E2, C1, C2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
{
  z0_primary: Vec<E1::Scalar>,
  z0_secondary: Vec<E2::Scalar>,
  r_W_primary: RelaxedR1CSWitness<E1>,
  r_U_primary: RelaxedR1CSInstance<E1>,
  ri_primary: E1::Scalar,
  r_W_secondary: RelaxedR1CSWitness<E2>,
  r_U_secondary: RelaxedR1CSInstance<E2>,
  ri_secondary: E2::Scalar,
  l_w_secondary: R1CSWitness<E2>,
  l_u_secondary: R1CSInstance<E2>,
  i: usize,
  zi_primary: Vec<E1::Scalar>,
  zi_secondary: Vec<E2::Scalar>,
  _p: PhantomData<(C1, C2)>,
}

impl<E1, E2, C1, C2> RecursiveSNARK<E1, E2, C1, C2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
{
  /// Create new instance of recursive SNARK
  pub fn new(
    pp: &mut PublicParams<E1, E2, C1, C2>,
    c_primary: &C1,
    c_secondary: &C2,
    z0_primary: &[E1::Scalar],
    z0_secondary: &[E2::Scalar],
  ) -> Result<Self, NovaError> {
    if z0_primary.len() != pp.F_arity_primary || z0_secondary.len() != pp.F_arity_secondary {
      return Err(NovaError::InvalidInitialInputLength);
    }
    let mut rng = ChaCha20Rng::seed_from_u64(0xDEADBEEF);

    let ri_primary = E1::Scalar::random(&mut rng);
    let ri_secondary = E2::Scalar::random(&mut rng);

    // base case for the primary
    let mut cs_primary = SatisfyingAssignment::<E1>::new();
    let inputs_primary: NovaAugmentedCircuitInputs<E2> = NovaAugmentedCircuitInputs::new(
      scalar_as_base::<E1>(pp.digest()),
      E1::Scalar::ZERO,
      z0_primary.to_vec(),
      None,
      None,
      None,
      ri_primary, // "r next"
      None,
      None,
    );

    let circuit_primary: NovaAugmentedCircuit<'_, E2, C1> = NovaAugmentedCircuit::new(
      &pp.augmented_circuit_params_primary,
      Some(inputs_primary),
      c_primary,
      pp.ro_consts_circuit_primary.clone(),
    );
    let zi_primary = circuit_primary.synthesize(&mut cs_primary)?;
    let (u_primary, w_primary) =
      cs_primary.r1cs_instance_and_witness(&pp.r1cs_shape_primary, &pp.ck_primary)?;

    // base case for the secondary
    let mut cs_secondary = SatisfyingAssignment::<E2>::new();
    let inputs_secondary: NovaAugmentedCircuitInputs<E1> = NovaAugmentedCircuitInputs::new(
      pp.digest(),
      E2::Scalar::ZERO,
      z0_secondary.to_vec(),
      None,
      None,
      None,
      ri_secondary, // "r next"
      Some(u_primary.clone()),
      None,
    );
    let circuit_secondary: NovaAugmentedCircuit<'_, E1, C2> = NovaAugmentedCircuit::new(
      &pp.augmented_circuit_params_secondary,
      Some(inputs_secondary),
      c_secondary,
      pp.ro_consts_circuit_secondary.clone(),
    );
    let zi_secondary = circuit_secondary.synthesize(&mut cs_secondary)?;
    let (u_secondary, w_secondary) =
      cs_secondary.r1cs_instance_and_witness(&pp.r1cs_shape_secondary, &pp.ck_secondary)?;

    // IVC proof for the primary circuit
    let l_w_primary = w_primary;
    let l_u_primary = u_primary;
    let r_W_primary = RelaxedR1CSWitness::from_r1cs_witness(&pp.r1cs_shape_primary, &l_w_primary);
    let r_U_primary =
      RelaxedR1CSInstance::from_r1cs_instance(&pp.ck_primary, &pp.r1cs_shape_primary, &l_u_primary);

    // IVC proof for the secondary circuit
    let l_w_secondary = w_secondary;
    let l_u_secondary = u_secondary;
    let r_W_secondary = RelaxedR1CSWitness::<E2>::default(&pp.r1cs_shape_secondary);
    let r_U_secondary =
      RelaxedR1CSInstance::<E2>::default(&pp.ck_secondary, &pp.r1cs_shape_secondary);

    assert!(
      !(zi_primary.len() != pp.F_arity_primary || zi_secondary.len() != pp.F_arity_secondary),
      "Invalid step length"
    );

    let zi_primary = zi_primary
      .iter()
      .map(|v| v.get_value().ok_or(SynthesisError::AssignmentMissing))
      .collect::<Result<Vec<<E1 as Engine>::Scalar>, _>>()?;

    let zi_secondary = zi_secondary
      .iter()
      .map(|v| v.get_value().ok_or(SynthesisError::AssignmentMissing))
      .collect::<Result<Vec<<E2 as Engine>::Scalar>, _>>()?;

    Ok(Self {
      z0_primary: z0_primary.to_vec(),
      z0_secondary: z0_secondary.to_vec(),
      r_W_primary,
      r_U_primary,
      ri_primary,
      r_W_secondary,
      r_U_secondary,
      ri_secondary,
      l_w_secondary,
      l_u_secondary,
      i: 0,
      zi_primary,
      zi_secondary,
      _p: Default::default(),
    })
  }

  /// Create a new `RecursiveSNARK` (or updates the provided `RecursiveSNARK`)
  /// by executing a step of the incremental computation
  pub fn prove_step(
    &mut self,
    pp: &mut PublicParams<E1, E2, C1, C2>,
    c_primary: &C1,
    c_secondary: &C2,
  ) -> Result<(), NovaError> {
    // first step was already done in the constructor
    if self.i == 0 {
      self.i = 1;
      return Ok(());
    }

    let pp_clone = pp.clone();
    // fold the secondary circuit's instance
    let (nifs_secondary, (r_U_secondary, r_W_secondary)) = NIFS::prove(
      &pp_clone.ck_secondary,
      &pp_clone.ro_consts_secondary,
      &scalar_as_base::<E1>(pp.digest()),
      &pp.r1cs_shape_secondary,
      &self.r_U_secondary,
      &self.r_W_secondary,
      &self.l_u_secondary,
      &self.l_w_secondary,
    )?;

    let mut rng = ChaCha20Rng::seed_from_u64(0xDEADBEEF);
    let r_next_primary = E1::Scalar::random(&mut rng);

    let mut cs_primary = SatisfyingAssignment::<E1>::new();
    let inputs_primary: NovaAugmentedCircuitInputs<E2> = NovaAugmentedCircuitInputs::new(
      scalar_as_base::<E1>(pp.digest()),
      E1::Scalar::from(self.i as u64),
      self.z0_primary.to_vec(),
      Some(self.zi_primary.clone()),
      Some(self.r_U_secondary.clone()),
      Some(self.ri_primary),
      r_next_primary,
      Some(self.l_u_secondary.clone()),
      Some(nifs_secondary.comm_T),
    );

    let circuit_primary: NovaAugmentedCircuit<'_, E2, C1> = NovaAugmentedCircuit::new(
      &pp.augmented_circuit_params_primary,
      Some(inputs_primary),
      c_primary,
      pp.ro_consts_circuit_primary.clone(),
    );
    let zi_primary = circuit_primary.synthesize(&mut cs_primary)?;

    let (l_u_primary, l_w_primary) =
      cs_primary.r1cs_instance_and_witness(&pp.r1cs_shape_primary, &pp.ck_primary)?;

    let pp_clone = pp.clone();
    // fold the primary circuit's instance
    let (nifs_primary, (r_U_primary, r_W_primary)) = NIFS::prove(
      &pp_clone.ck_primary,
      &pp_clone.ro_consts_primary,
      &pp.digest(),
      &pp.r1cs_shape_primary,
      &self.r_U_primary,
      &self.r_W_primary,
      &l_u_primary,
      &l_w_primary,
    )?;

    let r_next_secondary = E2::Scalar::random(&mut rng);

    let mut cs_secondary = SatisfyingAssignment::<E2>::new();
    let inputs_secondary: NovaAugmentedCircuitInputs<E1> = NovaAugmentedCircuitInputs::new(
      pp.digest(),
      E2::Scalar::from(self.i as u64),
      self.z0_secondary.to_vec(),
      Some(self.zi_secondary.clone()),
      Some(self.r_U_primary.clone()),
      Some(self.ri_secondary),
      r_next_secondary,
      Some(l_u_primary),
      Some(nifs_primary.comm_T),
    );

    let circuit_secondary: NovaAugmentedCircuit<'_, E1, C2> = NovaAugmentedCircuit::new(
      &pp.augmented_circuit_params_secondary,
      Some(inputs_secondary),
      c_secondary,
      pp.ro_consts_circuit_secondary.clone(),
    );
    let zi_secondary = circuit_secondary.synthesize(&mut cs_secondary)?;

    let (l_u_secondary, l_w_secondary) = cs_secondary
      .r1cs_instance_and_witness(&pp.r1cs_shape_secondary, &pp.ck_secondary)
      .map_err(|_e| NovaError::UnSat)?;

    // update the running instances and witnesses
    self.zi_primary = zi_primary
      .iter()
      .map(|v| v.get_value().ok_or(SynthesisError::AssignmentMissing))
      .collect::<Result<Vec<<E1 as Engine>::Scalar>, _>>()?;
    self.zi_secondary = zi_secondary
      .iter()
      .map(|v| v.get_value().ok_or(SynthesisError::AssignmentMissing))
      .collect::<Result<Vec<<E2 as Engine>::Scalar>, _>>()?;

    self.l_u_secondary = l_u_secondary;
    self.l_w_secondary = l_w_secondary;

    self.r_U_primary = r_U_primary;
    self.r_W_primary = r_W_primary;

    self.i += 1;

    self.r_U_secondary = r_U_secondary;
    self.r_W_secondary = r_W_secondary;

    self.ri_primary = r_next_primary;
    self.ri_secondary = r_next_secondary;

    Ok(())
  }

  /// Verify the correctness of the `RecursiveSNARK`
  pub fn verify(
    &self,
    pp: &mut PublicParams<E1, E2, C1, C2>,
    num_steps: usize,
    z0_primary: &[E1::Scalar],
    z0_secondary: &[E2::Scalar],
  ) -> Result<(Vec<E1::Scalar>, Vec<E2::Scalar>), NovaError> {
    // number of steps cannot be zero
    let is_num_steps_zero = num_steps == 0;

    // check if the provided proof has executed num_steps
    let is_num_steps_not_match = self.i != num_steps;

    // check if the initial inputs match
    let is_inputs_not_match = self.z0_primary != z0_primary || self.z0_secondary != z0_secondary;

    // check if the (relaxed) R1CS instances have two public outputs
    let is_instance_has_two_outpus = self.l_u_secondary.X.len() != 2
      || self.r_U_primary.X.len() != 2
      || self.r_U_secondary.X.len() != 2;

    if is_num_steps_zero
      || is_num_steps_not_match
      || is_inputs_not_match
      || is_instance_has_two_outpus
    {
      return Err(NovaError::ProofVerifyError);
    }

    // check if the output hashes in R1CS instances point to the right running instances
    let (hash_primary, hash_secondary) = {
      let mut hasher = <E2 as Engine>::RO::new(
        pp.ro_consts_secondary.clone(),
        NUM_FE_WITHOUT_IO_FOR_CRHF + 2 * pp.F_arity_primary,
      );
      hasher.absorb(pp.digest());
      hasher.absorb(E1::Scalar::from(num_steps as u64));
      for e in z0_primary {
        hasher.absorb(*e);
      }
      for e in &self.zi_primary {
        hasher.absorb(*e);
      }
      self.r_U_secondary.absorb_in_ro(&mut hasher);
      hasher.absorb(self.ri_primary);

      let mut hasher2 = <E1 as Engine>::RO::new(
        pp.ro_consts_primary.clone(),
        NUM_FE_WITHOUT_IO_FOR_CRHF + 2 * pp.F_arity_secondary,
      );
      hasher2.absorb(scalar_as_base::<E1>(pp.digest()));
      hasher2.absorb(E2::Scalar::from(num_steps as u64));
      for e in z0_secondary {
        hasher2.absorb(*e);
      }
      for e in &self.zi_secondary {
        hasher2.absorb(*e);
      }
      self.r_U_primary.absorb_in_ro(&mut hasher2);
      hasher2.absorb(self.ri_secondary);

      (
        hasher.squeeze(NUM_HASH_BITS),
        hasher2.squeeze(NUM_HASH_BITS),
      )
    };

    if hash_primary != self.l_u_secondary.X[0]
      || hash_secondary != scalar_as_base::<E2>(self.l_u_secondary.X[1])
    {
      return Err(NovaError::ProofVerifyError);
    }

    // check the satisfiability of the provided instances
    let res_r_primary =
      pp.r1cs_shape_primary
        .is_sat_relaxed(&pp.ck_primary, &self.r_U_primary, &self.r_W_primary);

    let res_r_secondary = pp.r1cs_shape_secondary.is_sat_relaxed(
      &pp.ck_secondary,
      &self.r_U_secondary,
      &self.r_W_secondary,
    );

    let res_l_secondary =
      pp.r1cs_shape_secondary
        .is_sat(&pp.ck_secondary, &self.l_u_secondary, &self.l_w_secondary);

    // check the returned res objects
    res_r_primary?;
    res_r_secondary?;
    res_l_secondary?;

    Ok((self.zi_primary.clone(), self.zi_secondary.clone()))
  }

  /// Get the outputs after the last step of computation.
  pub fn outputs(&self) -> (&[E1::Scalar], &[E2::Scalar]) {
    (&self.zi_primary, &self.zi_secondary)
  }

  /// The number of steps which have been executed thus far.
  pub fn num_steps(&self) -> usize {
    self.i
  }
}

/// A type that holds the prover key for `CompressedSNARK`
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct ProverKey<E1, E2, C1, C2, S1, S2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
  S1: RelaxedR1CSSNARKTrait<E1>,
  S2: RelaxedR1CSSNARKTrait<E2>,
{
  pk_primary: S1::ProverKey,
  pk_secondary: S2::ProverKey,
  _p: PhantomData<(C1, C2)>,
}

/// A type that holds the verifier key for `CompressedSNARK`
#[derive(Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct VerifierKey<E1, E2, C1, C2, S1, S2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
  S1: RelaxedR1CSSNARKTrait<E1>,
  S2: RelaxedR1CSSNARKTrait<E2>,
{
  F_arity_primary: usize,
  F_arity_secondary: usize,
  ro_consts_primary: ROConstants<E1>,
  ro_consts_secondary: ROConstants<E2>,
  pp_digest: E1::Scalar,
  vk_primary: S1::VerifierKey,
  vk_secondary: S2::VerifierKey,
  dk_primary: DerandKey<E1>,
  dk_secondary: DerandKey<E2>,
  _p: PhantomData<(C1, C2)>,
}

/// A SNARK that proves the knowledge of a valid `RecursiveSNARK`
#[derive(Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct CompressedSNARK<E1, E2, C1, C2, S1, S2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
  S1: RelaxedR1CSSNARKTrait<E1>,
  S2: RelaxedR1CSSNARKTrait<E2>,
{
  r_U_secondary: RelaxedR1CSInstance<E2>,
  ri_secondary: E2::Scalar,
  l_u_secondary: R1CSInstance<E2>,
  nifs_Uf_secondary: NIFS<E2>,

  l_ur_secondary: RelaxedR1CSInstance<E2>,
  nifs_Un_secondary: NIFSRelaxed<E2>,

  r_U_primary: RelaxedR1CSInstance<E1>,
  ri_primary: E1::Scalar,
  l_ur_primary: RelaxedR1CSInstance<E1>,
  nifs_Un_primary: NIFSRelaxed<E1>,

  wit_blind_r_Wn_primary: E1::Scalar,
  err_blind_r_Wn_primary: E1::Scalar,
  wit_blind_r_Wn_secondary: E2::Scalar,
  err_blind_r_Wn_secondary: E2::Scalar,

  snark_primary: S1,
  snark_secondary: S2,

  zn_primary: Vec<E1::Scalar>,
  zn_secondary: Vec<E2::Scalar>,

  _p: PhantomData<(C1, C2)>,
}

impl<E1, E2, C1, C2, S1, S2> CompressedSNARK<E1, E2, C1, C2, S1, S2>
where
  E1: Engine<Base = <E2 as Engine>::Scalar>,
  E2: Engine<Base = <E1 as Engine>::Scalar>,
  C1: StepCircuit<E1::Scalar>,
  C2: StepCircuit<E2::Scalar>,
  S1: RelaxedR1CSSNARKTrait<E1>,
  S2: RelaxedR1CSSNARKTrait<E2>,
{
  /// Creates prover and verifier keys for `CompressedSNARK`
  pub fn setup(
    pp: &mut PublicParams<E1, E2, C1, C2>,
  ) -> Result<
    (
      ProverKey<E1, E2, C1, C2, S1, S2>,
      VerifierKey<E1, E2, C1, C2, S1, S2>,
    ),
    NovaError,
  > {
    let (pk_primary, vk_primary) = S1::setup(&pp.ck_primary, &pp.r1cs_shape_primary)?;
    let (pk_secondary, vk_secondary) = S2::setup(&pp.ck_secondary, &pp.r1cs_shape_secondary)?;

    let pk = ProverKey {
      pk_primary,
      pk_secondary,
      _p: Default::default(),
    };

    let vk = VerifierKey {
      F_arity_primary: pp.F_arity_primary,
      F_arity_secondary: pp.F_arity_secondary,
      ro_consts_primary: pp.ro_consts_primary.clone(),
      ro_consts_secondary: pp.ro_consts_secondary.clone(),
      pp_digest: pp.digest(),
      vk_primary,
      vk_secondary,
      dk_primary: E1::CE::derand_key(&pp.ck_primary),
      dk_secondary: E2::CE::derand_key(&pp.ck_secondary),
      _p: Default::default(),
    };

    Ok((pk, vk))
  }

  /// Create a new `CompressedSNARK` (provides zero-knowledge)
  pub fn prove(
    pp: &mut PublicParams<E1, E2, C1, C2>,
    pk: &ProverKey<E1, E2, C1, C2, S1, S2>,
    recursive_snark: &RecursiveSNARK<E1, E2, C1, C2>,
  ) -> Result<Self, NovaError> {
    // prove three foldings

    let pp_clone = pp.clone();
    // fold secondary U/W with secondary u/w to get Uf/Wf
    let (nifs_Uf_secondary, (r_Uf_secondary, r_Wf_secondary)) = NIFS::prove(
      &pp_clone.ck_secondary,
      &pp_clone.ro_consts_secondary,
      &scalar_as_base::<E1>(pp.digest()),
      &pp.r1cs_shape_secondary,
      &recursive_snark.r_U_secondary,
      &recursive_snark.r_W_secondary,
      &recursive_snark.l_u_secondary,
      &recursive_snark.l_w_secondary,
    )?;

    // fold Uf/Wf with random inst/wit to get U1/W1
    let (l_ur_secondary, l_wr_secondary) = pp
      .r1cs_shape_secondary
      .sample_random_instance_witness(&pp.ck_secondary)?;

    let pp_clone = pp.clone();
    let (nifs_Un_secondary, (r_Un_secondary, r_Wn_secondary)) = NIFSRelaxed::prove(
      &pp_clone.ck_secondary,
      &pp_clone.ro_consts_secondary,
      &scalar_as_base::<E1>(pp.digest()),
      &pp.r1cs_shape_secondary,
      &r_Uf_secondary,
      &r_Wf_secondary,
      &l_ur_secondary,
      &l_wr_secondary,
    )?;

    // fold primary U/W with random inst/wit to get U2/W2
    let (l_ur_primary, l_wr_primary) = pp
      .r1cs_shape_primary
      .sample_random_instance_witness(&pp.ck_primary)?;

    let pp_clone = pp.clone();
    let (nifs_Un_primary, (r_Un_primary, r_Wn_primary)) = NIFSRelaxed::prove(
      &pp_clone.ck_primary,
      &pp_clone.ro_consts_primary,
      &pp.digest(),
      &pp.r1cs_shape_primary,
      &recursive_snark.r_U_primary,
      &recursive_snark.r_W_primary,
      &l_ur_primary,
      &l_wr_primary,
    )?;

    // derandomize/unblind commitments
    let (derandom_r_Wn_primary, wit_blind_r_Wn_primary, err_blind_r_Wn_primary) =
      r_Wn_primary.derandomize();
    let derandom_r_Un_primary = r_Un_primary.derandomize(
      &E1::CE::derand_key(&pp.ck_primary),
      &wit_blind_r_Wn_primary,
      &err_blind_r_Wn_primary,
    );

    let (derandom_r_Wn_secondary, wit_blind_r_Wn_secondary, err_blind_r_Wn_secondary) =
      r_Wn_secondary.derandomize();
    let derandom_r_Un_secondary = r_Un_secondary.derandomize(
      &E2::CE::derand_key(&pp.ck_secondary),
      &wit_blind_r_Wn_secondary,
      &err_blind_r_Wn_secondary,
    );

    // create SNARKs proving the knowledge of Wn primary/secondary
    let snark_primary = S1::prove(
      &pp.ck_primary,
      &pk.pk_primary,
      &pp.r1cs_shape_primary,
      &derandom_r_Un_primary,
      &derandom_r_Wn_primary,
    );

    let snark_secondary = S2::prove(
      &pp.ck_secondary,
      &pk.pk_secondary,
      &pp.r1cs_shape_secondary,
      &derandom_r_Un_secondary,
      &derandom_r_Wn_secondary,
    );

    Ok(Self {
      r_U_secondary: recursive_snark.r_U_secondary.clone(),
      ri_secondary: recursive_snark.ri_secondary,
      l_u_secondary: recursive_snark.l_u_secondary.clone(),
      nifs_Uf_secondary: nifs_Uf_secondary.clone(),

      l_ur_secondary: l_ur_secondary.clone(),
      nifs_Un_secondary: nifs_Un_secondary.clone(),

      r_U_primary: recursive_snark.r_U_primary.clone(),
      ri_primary: recursive_snark.ri_primary,
      l_ur_primary: l_ur_primary.clone(),
      nifs_Un_primary: nifs_Un_primary.clone(),

      wit_blind_r_Wn_primary,
      err_blind_r_Wn_primary,
      wit_blind_r_Wn_secondary,
      err_blind_r_Wn_secondary,

      snark_primary: snark_primary?,
      snark_secondary: snark_secondary?,

      zn_primary: recursive_snark.zi_primary.clone(),
      zn_secondary: recursive_snark.zi_secondary.clone(),

      _p: Default::default(),
    })
  }

  /// Verify the correctness of the `CompressedSNARK` (provides zero-knowledge)
  pub fn verify(
    &self,
    vk: &mut VerifierKey<E1, E2, C1, C2, S1, S2>,
    num_steps: usize,
    z0_primary: &[E1::Scalar],
    z0_secondary: &[E2::Scalar],
  ) -> Result<(Vec<E1::Scalar>, Vec<E2::Scalar>), NovaError> {
    // the number of steps cannot be zero
    if num_steps == 0 {
      return Err(NovaError::ProofVerifyError);
    }

    // check if the (relaxed) R1CS instances have two public outputs
    if self.l_u_secondary.X.len() != 2
      || self.r_U_primary.X.len() != 2
      || self.r_U_secondary.X.len() != 2
      || self.l_ur_primary.X.len() != 2
      || self.l_ur_secondary.X.len() != 2
    {
      return Err(NovaError::ProofVerifyError);
    }

    // check if the output hashes in R1CS instances point to the right running instances
    let (hash_primary, hash_secondary) = {
      let mut hasher = <E2 as Engine>::RO::new(
        vk.ro_consts_secondary.clone(),
        NUM_FE_WITHOUT_IO_FOR_CRHF + 2 * vk.F_arity_primary,
      );
      hasher.absorb(vk.pp_digest);
      hasher.absorb(E1::Scalar::from(num_steps as u64));
      for e in z0_primary {
        hasher.absorb(*e);
      }
      for e in &self.zn_primary {
        hasher.absorb(*e);
      }
      self.r_U_secondary.absorb_in_ro(&mut hasher);
      hasher.absorb(self.ri_primary);

      let mut hasher2 = <E1 as Engine>::RO::new(
        vk.ro_consts_primary.clone(),
        NUM_FE_WITHOUT_IO_FOR_CRHF + 2 * vk.F_arity_secondary,
      );
      hasher2.absorb(scalar_as_base::<E1>(vk.pp_digest));
      hasher2.absorb(E2::Scalar::from(num_steps as u64));
      for e in z0_secondary {
        hasher2.absorb(*e);
      }
      for e in &self.zn_secondary {
        hasher2.absorb(*e);
      }
      self.r_U_primary.absorb_in_ro(&mut hasher2);
      hasher2.absorb(self.ri_secondary);

      (
        hasher.squeeze(NUM_HASH_BITS),
        hasher2.squeeze(NUM_HASH_BITS),
      )
    };

    if hash_primary != self.l_u_secondary.X[0]
      || hash_secondary != scalar_as_base::<E2>(self.l_u_secondary.X[1])
    {
      return Err(NovaError::ProofVerifyError);
    }

    // fold secondary U/W with secondary u/w to get Uf/Wf
    let r_Uf_secondary = self.nifs_Uf_secondary.verify(
      &vk.ro_consts_secondary,
      &scalar_as_base::<E1>(vk.pp_digest),
      &self.r_U_secondary,
      &self.l_u_secondary,
    )?;

    // fold Uf/Wf with random inst/wit to get U1/W1
    let r_Un_secondary = self.nifs_Un_secondary.verify(
      &vk.ro_consts_secondary,
      &scalar_as_base::<E1>(vk.pp_digest),
      &r_Uf_secondary,
      &self.l_ur_secondary,
    )?;

    // fold primary U/W with random inst/wit to get U2/W2
    let r_Un_primary = self.nifs_Un_primary.verify(
      &vk.ro_consts_primary,
      &vk.pp_digest,
      &self.r_U_primary,
      &self.l_ur_primary,
    )?;

    // derandomize/unblind commitments
    let derandom_r_Un_primary = r_Un_primary.derandomize(
      &vk.dk_primary,
      &self.wit_blind_r_Wn_primary,
      &self.err_blind_r_Wn_primary,
    );
    let derandom_r_Un_secondary = r_Un_secondary.derandomize(
      &vk.dk_secondary,
      &self.wit_blind_r_Wn_secondary,
      &self.err_blind_r_Wn_secondary,
    );

    // check the satisfiability of the folded instances using
    // SNARKs proving the knowledge of their satisfying witnesses
    let res_primary = self
      .snark_primary
      .verify(&mut vk.vk_primary, &derandom_r_Un_primary);
    let res_secondary = self
      .snark_secondary
      .verify(&mut vk.vk_secondary, &derandom_r_Un_secondary);

    res_primary?;
    res_secondary?;

    Ok((self.zn_primary.clone(), self.zn_secondary.clone()))
  }
}

type CommitmentKey<E> = <<E as Engine>::CE as CommitmentEngineTrait<E>>::CommitmentKey;
type DerandKey<E> = <<E as Engine>::CE as CommitmentEngineTrait<E>>::DerandKey;
type Commitment<E> = <<E as Engine>::CE as CommitmentEngineTrait<E>>::Commitment;
type CE<E> = <E as Engine>::CE;

#[cfg(test)]
mod tests {
  extern crate std;
  use super::*;
  use crate::{
    provider::{pedersen::CommitmentKeyExtTrait, traits::DlogGroup, PallasEngine, VestaEngine},
    traits::{circuit::TrivialCircuit, evaluation::EvaluationEngineTrait, snark::default_ck_hint},
  };
  use core::{fmt::Write, marker::PhantomData};
  use expect_test::{expect, Expect};
  use ff::PrimeField;
  use frontend::{num::AllocatedNum, ConstraintSystem, SynthesisError};
  use std::fs::File;
  use std::io::Write as Writee;

  type EE<E> = provider::ipa_pc::EvaluationEngine<E>;
  type EEPrime<E> = provider::hyperkzg::EvaluationEngine<E>;
  type S<E, EE> = spartan::snark::RelaxedR1CSSNARK<E, EE>;
  type SPrime<E, EE> = spartan::ppsnark::RelaxedR1CSSNARK<E, EE>;

  #[derive(Clone, Debug, Default)]
  struct CubicCircuit<F: PrimeField> {
    _p: PhantomData<F>,
  }

  impl<F: PrimeField> StepCircuit<F> for CubicCircuit<F> {
    fn arity(&self) -> usize {
      1
    }

    fn synthesize<CS: ConstraintSystem<F>>(
      &self,
      cs: &mut CS,
      z: &[AllocatedNum<F>],
    ) -> Result<Vec<AllocatedNum<F>>, SynthesisError> {
      // Consider a cubic equation: `x^3 + x + 5 = y`, where `x` and `y` are respectively the input and output.
      let x = &z[0];
      let x_sq = x.square(cs.namespace(|| "x_sq"))?;
      let x_cu = x_sq.mul(cs.namespace(|| "x_cu"), x)?;
      let y = AllocatedNum::alloc(cs.namespace(|| "y"), || {
        Ok(x_cu.get_value().unwrap() + x.get_value().unwrap() + F::from(5u64))
      })?;

      cs.enforce(
        || "y = x^3 + x + 5",
        |lc| {
          lc + x_cu.get_variable()
            + x.get_variable()
            + CS::one()
            + CS::one()
            + CS::one()
            + CS::one()
            + CS::one()
        },
        |lc| lc + CS::one(),
        |lc| lc + y.get_variable(),
      );

      Ok(vec![y])
    }
  }

  impl<F: PrimeField> CubicCircuit<F> {
    fn output(&self, z: &[F]) -> Vec<F> {
      vec![z[0] * z[0] * z[0] + z[0] + F::from(5u64)]
    }
  }

  fn test_pp_digest_with<E1, E2, T1, T2>(circuit1: &T1, circuit2: &T2, expected: &Expect)
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
    E1::GE: DlogGroup,
    E2::GE: DlogGroup,
    T1: StepCircuit<E1::Scalar>,
    T2: StepCircuit<E2::Scalar>,
    // required to use the IPA in the initialization of the commitment key hints below
    <E1::CE as CommitmentEngineTrait<E1>>::CommitmentKey: CommitmentKeyExtTrait<E1>,
    <E2::CE as CommitmentEngineTrait<E2>>::CommitmentKey: CommitmentKeyExtTrait<E2>,
  {
    // this tests public parameters with a size specifically intended for a spark-compressed SNARK
    let ck_hint1 = &*SPrime::<E1, EE<E1>>::ck_floor();
    let ck_hint2 = &*SPrime::<E2, EE<E2>>::ck_floor();
    let mut pp =
      PublicParams::<E1, E2, T1, T2>::setup(circuit1, circuit2, ck_hint1, ck_hint2).unwrap();

    let digest_str = pp
      .digest()
      .to_repr()
      .as_ref()
      .iter()
      .fold(String::new(), |mut output, b| {
        let _ = write!(output, "{b:02x}");
        output
      });
    expected.assert_eq(&digest_str);
  }

  #[test]
  fn test_pp_digest() {
    test_pp_digest_with::<PallasEngine, VestaEngine, _, _>(
      &TrivialCircuit::<_>::default(),
      &TrivialCircuit::<_>::default(),
      &expect!["ba7ff40bc60f95f7157350608b2f1892dc33b2470ccf52c3fae0464c61db9501"],
    );

    // test_pp_digest_with::<Bn256EngineIPA, GrumpkinEngine, _, _>(
    //   &TrivialCircuit::<_>::default(),
    //   &TrivialCircuit::<_>::default(),
    //   &expect!["e0d75ecff901aee5b22223a4be82af30d7988a5f2cbd40815fda88dd79a22a01"],
    // );

    // test_pp_digest_with::<Secp256k1Engine, Secq256k1Engine, _, _>(
    //   &TrivialCircuit::<_>::default(),
    //   &TrivialCircuit::<_>::default(),
    //   &expect!["ee4bd444ffe1f1be8224a09dae09bdf4532035655fd3f25e70955eaa13c48d03"],
    // );
  }

  fn test_ivc_trivial_with<E1, E2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
  {
    let test_circuit1 = TrivialCircuit::<<E1 as Engine>::Scalar>::default();
    let test_circuit2 = TrivialCircuit::<<E2 as Engine>::Scalar>::default();

    // produce public parameters
    let mut pp = PublicParams::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      TrivialCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &test_circuit1,
      &test_circuit2,
      &*default_ck_hint(),
      &*default_ck_hint(),
    )
    .unwrap();

    let num_steps = 1;

    // produce a recursive SNARK
    let mut recursive_snark = RecursiveSNARK::new(
      &mut pp,
      &test_circuit1,
      &test_circuit2,
      &[<E1 as Engine>::Scalar::ZERO],
      &[<E2 as Engine>::Scalar::ZERO],
    )
    .unwrap();

    let res = recursive_snark.prove_step(&mut pp, &test_circuit1, &test_circuit2);

    assert!(res.is_ok());

    // verify the recursive SNARK
    let res = recursive_snark.verify(
      &mut pp,
      num_steps,
      &[<E1 as Engine>::Scalar::ZERO],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());
  }

  #[test]
  fn test_ivc_trivial() {
    test_ivc_trivial_with::<PallasEngine, VestaEngine>();
    // test_ivc_trivial_with::<Bn256EngineKZG, GrumpkinEngine>();
    // test_ivc_trivial_with::<Secp256k1Engine, Secq256k1Engine>();
  }

  fn test_ivc_nontrivial_with<E1, E2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
  {
    let circuit_primary = TrivialCircuit::default();
    let circuit_secondary = CubicCircuit::default();

    // produce public parameters
    let mut pp = PublicParams::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &circuit_primary,
      &circuit_secondary,
      &*default_ck_hint(),
      &*default_ck_hint(),
    )
    .unwrap();

    let num_steps = 3;

    // produce a recursive SNARK
    let mut recursive_snark = RecursiveSNARK::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::new(
      &mut pp,
      &circuit_primary,
      &circuit_secondary,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    )
    .unwrap();

    for i in 0..num_steps {
      let res = recursive_snark.prove_step(&mut pp, &circuit_primary, &circuit_secondary);
      assert!(res.is_ok());

      // verify the recursive snark at each step of recursion
      let res = recursive_snark.verify(
        &mut pp,
        i + 1,
        &[<E1 as Engine>::Scalar::ONE],
        &[<E2 as Engine>::Scalar::ZERO],
      );
      assert!(res.is_ok());
    }

    // verify the recursive SNARK
    let res = recursive_snark.verify(
      &mut pp,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());

    let (zn_primary, zn_secondary) = res.unwrap();

    // sanity: check the claimed output with a direct computation of the same
    assert_eq!(zn_primary, vec![<E1 as Engine>::Scalar::ONE]);
    let mut zn_secondary_direct = vec![<E2 as Engine>::Scalar::ZERO];
    for _i in 0..num_steps {
      zn_secondary_direct = circuit_secondary.clone().output(&zn_secondary_direct);
    }
    assert_eq!(zn_secondary, zn_secondary_direct);
    assert_eq!(zn_secondary, vec![<E2 as Engine>::Scalar::from(2460515u64)]);
  }

  #[test]
  fn test_ivc_nontrivial() {
    test_ivc_nontrivial_with::<PallasEngine, VestaEngine>();
    // test_ivc_nontrivial_with::<Bn256EngineKZG, GrumpkinEngine>();
    // test_ivc_nontrivial_with::<Secp256k1Engine, Secq256k1Engine>();
  }

  fn test_ivc_nontrivial_with_compression_with<E1, E2, EE1, EE2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
    EE1: EvaluationEngineTrait<E1>,
    EE2: EvaluationEngineTrait<E2>,
  {
    let circuit_primary = TrivialCircuit::default();
    let circuit_secondary = CubicCircuit::default();

    // produce public parameters
    let mut pp = PublicParams::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &circuit_primary,
      &circuit_secondary,
      &*default_ck_hint(),
      &*default_ck_hint(),
    )
    .unwrap();

    let num_steps = 3;

    // produce a recursive SNARK
    let mut recursive_snark = RecursiveSNARK::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::new(
      &mut pp,
      &circuit_primary,
      &circuit_secondary,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    )
    .unwrap();

    for _i in 0..num_steps {
      let res = recursive_snark.prove_step(&mut pp, &circuit_primary, &circuit_secondary);
      assert!(res.is_ok());
    }

    // verify the recursive SNARK
    let res = recursive_snark.verify(
      &mut pp,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());

    let (zn_primary, zn_secondary) = res.unwrap();

    // sanity: check the claimed output with a direct computation of the same
    assert_eq!(zn_primary, vec![<E1 as Engine>::Scalar::ONE]);
    let mut zn_secondary_direct = vec![<E2 as Engine>::Scalar::ZERO];
    for _i in 0..num_steps {
      zn_secondary_direct = circuit_secondary.clone().output(&zn_secondary_direct);
    }
    assert_eq!(zn_secondary, zn_secondary_direct);
    assert_eq!(zn_secondary, vec![<E2 as Engine>::Scalar::from(2460515u64)]);

    // produce the prover and verifier keys for compressed snark
    let (pk, mut vk) =
      CompressedSNARK::<_, _, _, _, S<E1, EE1>, S<E2, EE2>>::setup(&mut pp).unwrap();

    // produce a compressed SNARK
    let res =
      CompressedSNARK::<_, _, _, _, S<E1, EE1>, S<E2, EE2>>::prove(&mut pp, &pk, &recursive_snark);
    assert!(res.is_ok());
    let compressed_snark = res.unwrap();
    let json_data = serde_json::to_string_pretty(&compressed_snark).unwrap();
    let mut file = File::create("./compressed_snark.json").unwrap();
    file.write_all(json_data.as_bytes()).unwrap();

    let json_data = serde_json::to_string_pretty(&vk).unwrap();
    let mut file = File::create("./verifying_key.json").unwrap();
    file.write_all(json_data.as_bytes()).unwrap();

    // print!("{}", num_steps);
    // verify the compressed SNARK
    let res = compressed_snark.verify(
      &mut vk,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());
  }

  #[test]
  fn test_ivc_nontrivial_with_compression() {
    // TODO -> THIS TEST
    test_ivc_nontrivial_with_compression_with::<PallasEngine, VestaEngine, EE<_>, EE<_>>();
    // test_ivc_nontrivial_with_compression_with::<Bn256EngineKZG, GrumpkinEngine, EEPrime<_>, EE<_>>(
    // );
    // test_ivc_nontrivial_with_compression_with::<Secp256k1Engine, Secq256k1Engine, EE<_>, EE<_>>();

    // test_ivc_nontrivial_with_spark_compression_with::<
    //   Bn256EngineKZG,
    //   GrumpkinEngine,
    //   provider::hyperkzg::EvaluationEngine<_>,
    //   EE<_>,
    // >();
  }

  fn test_ivc_nontrivial_with_spark_compression_with<E1, E2, EE1, EE2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
    EE1: EvaluationEngineTrait<E1>,
    EE2: EvaluationEngineTrait<E2>,
  {
    let circuit_primary = TrivialCircuit::default();
    let circuit_secondary = CubicCircuit::default();

    // produce public parameters, which we'll use with a spark-compressed SNARK
    let mut pp = PublicParams::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &circuit_primary,
      &circuit_secondary,
      &*SPrime::<E1, EE1>::ck_floor(),
      &*SPrime::<E2, EE2>::ck_floor(),
    )
    .unwrap();

    let num_steps = 3;

    // produce a recursive SNARK
    let mut recursive_snark = RecursiveSNARK::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::new(
      &mut pp,
      &circuit_primary,
      &circuit_secondary,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    )
    .unwrap();

    for _i in 0..num_steps {
      let res = recursive_snark.prove_step(&mut pp, &circuit_primary, &circuit_secondary);
      assert!(res.is_ok());
    }

    // verify the recursive SNARK
    let res = recursive_snark.verify(
      &mut pp,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());

    let (zn_primary, zn_secondary) = res.unwrap();

    // sanity: check the claimed output with a direct computation of the same
    assert_eq!(zn_primary, vec![<E1 as Engine>::Scalar::ONE]);
    let mut zn_secondary_direct = vec![<E2 as Engine>::Scalar::ZERO];
    for _i in 0..num_steps {
      zn_secondary_direct = CubicCircuit::default().output(&zn_secondary_direct);
    }
    assert_eq!(zn_secondary, zn_secondary_direct);
    assert_eq!(zn_secondary, vec![<E2 as Engine>::Scalar::from(2460515u64)]);

    // run the compressed snark with Spark compiler
    // produce the prover and verifier keys for compressed snark
    let (pk, mut vk) =
      CompressedSNARK::<_, _, _, _, SPrime<E1, EE1>, SPrime<E2, EE2>>::setup(&mut pp).unwrap();

    // produce a compressed SNARK
    let res = CompressedSNARK::<_, _, _, _, SPrime<E1, EE1>, SPrime<E2, EE2>>::prove(
      &mut pp,
      &pk,
      &recursive_snark,
    );
    assert!(res.is_ok());
    let compressed_snark = res.unwrap();

    // verify the compressed SNARK
    let res = compressed_snark.verify(
      &mut vk,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());
  }

  #[test]
  fn test_ivc_nontrivial_with_spark_compression() {
    // TODO -> THIS TEST
    test_ivc_nontrivial_with_spark_compression_with::<PallasEngine, VestaEngine, EE<_>, EE<_>>();
    // test_ivc_nontrivial_with_spark_compression_with::<
    //   Bn256EngineKZG,
    //   GrumpkinEngine,
    //   EEPrime<_>,
    //   EE<_>,
    // >();
    // test_ivc_nontrivial_with_spark_compression_with::<Secp256k1Engine, Secq256k1Engine, EE<_>, EE<_>>(
    // );
  }

  fn test_ivc_nondet_with_compression_with<E1, E2, EE1, EE2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
    EE1: EvaluationEngineTrait<E1>,
    EE2: EvaluationEngineTrait<E2>,
  {
    // y is a non-deterministic advice representing the fifth root of the input at a step.
    #[derive(Clone, Debug)]
    struct FifthRootCheckingCircuit<F: PrimeField> {
      y: F,
    }

    impl<F: PrimeField> FifthRootCheckingCircuit<F> {
      fn new(num_steps: usize) -> (Vec<F>, Vec<Self>) {
        let mut powers = Vec::new();
        let rng = ChaCha20Rng::seed_from_u64(0xDEADBEEF);
        let mut seed = F::random(rng);
        for _i in 0..num_steps + 1 {
          seed *= seed.clone().square().square();

          powers.push(Self { y: seed });
        }

        // reverse the powers to get roots
        let roots = powers.into_iter().rev().collect::<Vec<Self>>();
        (vec![roots[0].y], roots[1..].to_vec())
      }
    }

    impl<F> StepCircuit<F> for FifthRootCheckingCircuit<F>
    where
      F: PrimeField,
    {
      fn arity(&self) -> usize {
        1
      }

      fn synthesize<CS: ConstraintSystem<F>>(
        &self,
        cs: &mut CS,
        z: &[AllocatedNum<F>],
      ) -> Result<Vec<AllocatedNum<F>>, SynthesisError> {
        let x = &z[0];

        // we allocate a variable and set it to the provided non-deterministic advice.
        let y = AllocatedNum::alloc_infallible(cs.namespace(|| "y"), || self.y);

        // We now check if y = x^{1/5} by checking if y^5 = x
        let y_sq = y.square(cs.namespace(|| "y_sq"))?;
        let y_quad = y_sq.square(cs.namespace(|| "y_quad"))?;
        let y_pow_5 = y_quad.mul(cs.namespace(|| "y_fifth"), &y)?;

        cs.enforce(
          || "y^5 = x",
          |lc| lc + y_pow_5.get_variable(),
          |lc| lc + CS::one(),
          |lc| lc + x.get_variable(),
        );

        Ok(vec![y])
      }
    }

    let circuit_primary = FifthRootCheckingCircuit {
      y: <E1 as Engine>::Scalar::ZERO,
    };

    let circuit_secondary = TrivialCircuit::default();

    // produce public parameters
    let mut pp = PublicParams::<
      E1,
      E2,
      FifthRootCheckingCircuit<<E1 as Engine>::Scalar>,
      TrivialCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &circuit_primary,
      &circuit_secondary,
      &*default_ck_hint(),
      &*default_ck_hint(),
    )
    .unwrap();

    let num_steps = 3;

    // produce non-deterministic advice
    let (z0_primary, roots) = FifthRootCheckingCircuit::new(num_steps);
    let z0_secondary = vec![<E2 as Engine>::Scalar::ZERO];

    // produce a recursive SNARK
    let mut recursive_snark: RecursiveSNARK<
      E1,
      E2,
      FifthRootCheckingCircuit<<E1 as Engine>::Scalar>,
      TrivialCircuit<<E2 as Engine>::Scalar>,
    > = RecursiveSNARK::<
      E1,
      E2,
      FifthRootCheckingCircuit<<E1 as Engine>::Scalar>,
      TrivialCircuit<<E2 as Engine>::Scalar>,
    >::new(
      &mut pp,
      &roots[0],
      &circuit_secondary,
      &z0_primary,
      &z0_secondary,
    )
    .unwrap();

    for circuit_primary in roots.iter().take(num_steps) {
      let res = recursive_snark.prove_step(&mut pp, circuit_primary, &circuit_secondary);
      assert!(res.is_ok());
    }

    // verify the recursive SNARK
    let res = recursive_snark.verify(&mut pp, num_steps, &z0_primary, &z0_secondary);
    assert!(res.is_ok());

    // produce the prover and verifier keys for compressed snark
    let (pk, mut vk) =
      CompressedSNARK::<_, _, _, _, S<E1, EE1>, S<E2, EE2>>::setup(&mut pp).unwrap();

    // produce a compressed SNARK
    let res =
      CompressedSNARK::<_, _, _, _, S<E1, EE1>, S<E2, EE2>>::prove(&mut pp, &pk, &recursive_snark);
    assert!(res.is_ok());
    let compressed_snark = res.unwrap();

    // verify the compressed SNARK
    let res = compressed_snark.verify(&mut vk, num_steps, &z0_primary, &z0_secondary);
    assert!(res.is_ok());
  }

  #[test]
  fn test_ivc_nondet_with_compression() {
    // TODO -> THIS TEST
    test_ivc_nondet_with_compression_with::<PallasEngine, VestaEngine, EE<_>, EE<_>>();
    // test_ivc_nondet_with_compression_with::<Bn256EngineKZG, GrumpkinEngine, EEPrime<_>, EE<_>>();
    // test_ivc_nondet_with_compression_with::<Secp256k1Engine, Secq256k1Engine, EE<_>, EE<_>>();
  }

  fn test_ivc_base_with<E1, E2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
  {
    let test_circuit1 = TrivialCircuit::<<E1 as Engine>::Scalar>::default();
    let test_circuit2 = CubicCircuit::<<E2 as Engine>::Scalar>::default();

    // produce public parameters
    let mut pp = PublicParams::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::setup(
      &test_circuit1,
      &test_circuit2,
      &*default_ck_hint(),
      &*default_ck_hint(),
    )
    .unwrap();

    let num_steps = 1;

    // produce a recursive SNARK
    let mut recursive_snark = RecursiveSNARK::<
      E1,
      E2,
      TrivialCircuit<<E1 as Engine>::Scalar>,
      CubicCircuit<<E2 as Engine>::Scalar>,
    >::new(
      &mut pp,
      &test_circuit1,
      &test_circuit2,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    )
    .unwrap();

    // produce a recursive SNARK
    let res = recursive_snark.prove_step(&mut pp, &test_circuit1, &test_circuit2);

    assert!(res.is_ok());

    // verify the recursive SNARK
    let res = recursive_snark.verify(
      &mut pp,
      num_steps,
      &[<E1 as Engine>::Scalar::ONE],
      &[<E2 as Engine>::Scalar::ZERO],
    );
    assert!(res.is_ok());

    let (zn_primary, zn_secondary) = res.unwrap();

    assert_eq!(zn_primary, vec![<E1 as Engine>::Scalar::ONE]);
    assert_eq!(zn_secondary, vec![<E2 as Engine>::Scalar::from(5u64)]);
  }

  #[test]
  fn test_ivc_base() {
    test_ivc_base_with::<PallasEngine, VestaEngine>();
    // test_ivc_base_with::<Bn256EngineKZG, GrumpkinEngine>();
    // test_ivc_base_with::<Secp256k1Engine, Secq256k1Engine>();
  }

  fn test_setup_with<E1, E2>()
  where
    E1: Engine<Base = <E2 as Engine>::Scalar>,
    E2: Engine<Base = <E1 as Engine>::Scalar>,
  {
    #[derive(Clone, Debug, Default)]
    struct CircuitWithInputize<F: PrimeField> {
      _p: PhantomData<F>,
    }

    impl<F: PrimeField> StepCircuit<F> for CircuitWithInputize<F> {
      fn arity(&self) -> usize {
        1
      }

      fn synthesize<CS: ConstraintSystem<F>>(
        &self,
        cs: &mut CS,
        z: &[AllocatedNum<F>],
      ) -> Result<Vec<AllocatedNum<F>>, SynthesisError> {
        let x = &z[0];
        let y = x.square(cs.namespace(|| "x_sq"))?;
        y.inputize(cs.namespace(|| "y"))?; // inputize y
        Ok(vec![y])
      }
    }

    // produce public parameters with trivial secondary
    let circuit = CircuitWithInputize::<<E1 as Engine>::Scalar>::default();
    let pp =
      PublicParams::<E1, E2, CircuitWithInputize<E1::Scalar>, TrivialCircuit<E2::Scalar>>::setup(
        &circuit,
        &TrivialCircuit::default(),
        &*default_ck_hint(),
        &*default_ck_hint(),
      );
    assert!(pp.is_err());
    assert_eq!(pp.err(), Some(NovaError::InvalidStepCircuitIO));

    // produce public parameters with the trivial primary
    let circuit = CircuitWithInputize::<E2::Scalar>::default();
    let pp =
      PublicParams::<E1, E2, TrivialCircuit<E1::Scalar>, CircuitWithInputize<E2::Scalar>>::setup(
        &TrivialCircuit::default(),
        &circuit,
        &*default_ck_hint(),
        &*default_ck_hint(),
      );
    assert!(pp.is_err());
    assert_eq!(pp.err(), Some(NovaError::InvalidStepCircuitIO));
  }

  #[test]
  fn test_setup() {
    test_setup_with::<PallasEngine, VestaEngine>();
  }
}
