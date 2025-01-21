//! This module defines traits that a step function must implement
use crate::{
  frontend::{num::AllocatedNum, ConstraintSystem, SynthesisError},
  prelude::*,
};
use core::marker::PhantomData;
use ff::PrimeField;

/// A helper trait for a step of the incremental computation (i.e., circuit for F)
pub trait StepCircuit<F: PrimeField>: Send + Sync + Clone {
  /// Return the number of inputs or outputs of each step
  /// (this method is called only at circuit synthesis time)
  /// `synthesize` and `output` methods are expected to take as
  /// input a vector of size equal to arity and output a vector of size equal to arity
  fn arity(&self) -> usize;

  /// Synthesize the circuit for a computation step and return variable
  /// that corresponds to the output of the step `z_{i+1}`
  fn synthesize<CS: ConstraintSystem<F>>(
    &self,
    cs: &mut CS,
    z: &[AllocatedNum<F>],
  ) -> Result<Vec<AllocatedNum<F>>, SynthesisError>;
}

/// A trivial step circuit that simply returns the input
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrivialCircuit<F: PrimeField> {
  _p: PhantomData<F>,
}

impl<F: PrimeField> StepCircuit<F> for TrivialCircuit<F> {
  fn arity(&self) -> usize {
    1
  }

  fn synthesize<CS: ConstraintSystem<F>>(
    &self,
    _cs: &mut CS,
    z: &[AllocatedNum<F>],
  ) -> Result<Vec<AllocatedNum<F>>, SynthesisError> {
    Ok(z.to_vec())
  }
}

/// A generic circuit that can be used for any circuit
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenericCircuit<F: PrimeField> {
  _p: PhantomData<F>,
  arity_value: usize,
  synthesize_value: Vec<AllocatedNum<F>>,
}

impl<F: PrimeField> GenericCircuit<F> {
  /// Create a new generic circuit
  pub fn new(arity_value: usize, synthesize_value: Vec<AllocatedNum<F>>) -> Self {
    Self {
      _p: PhantomData::<F>,
      arity_value,
      synthesize_value,
    }
  }
}

impl<F: PrimeField> StepCircuit<F> for GenericCircuit<F> {
  fn arity(&self) -> usize {
    self.arity_value
  }

  fn synthesize<CS: ConstraintSystem<F>>(
    &self,
    _cs: &mut CS,
    _z: &[AllocatedNum<F>],
  ) -> Result<Vec<AllocatedNum<F>>, SynthesisError> {
    Ok(self.synthesize_value.clone())
  }
}
