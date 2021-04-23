# Common
This crate contains common components used in STARK proof generation and verification. The most important of these components are `ProofOptions` struct and `Air` trait.

## Proof options
`ProofOptions` struct defines a set of options which are used during STARK proof generation and verification. These options have a direct impact on the security of the generated proofs as well as the proof generation time. Specifically, security of STARK proofs depends on:

1. Hash function - proof security is limited by the collision resistance of the hash function used by the protocol. For example, if a hash function with 128-bit collision resistance is used, security of a STARK proof cannot exceed 128 bits.
2. Finite field - proof security is limited by the finite field used by the protocol. This means, that for small fields (e.g. smaller than ~128 bits), field extensions must be used to achieve adequate security. And even for ~128 bit fields, to achieve security over 100 bits, a field extension may be required.
3. Number of queries - higher values increase proof security, but also increase proof size.
4. Blowup factor - higher values increase proof security, but also increase proof generation time and proof size. However, higher blowup factors require fewer queries for the same security level. Thus, it is frequently possible to increase blowup factor and at the same time decrease the number of queries in such  a way that the proofs become smaller.
5. Grinding factor - higher values increase proof security, but also may increase proof generation time.

See [options.rs](src/options.rs) for more info on currently available options and their meaning. Additionally, security level of a proof can be estimated using `StarkProof::security_level()` function.

## Air trait
Before we can generate proofs attesting that some computations were executed correctly, we need to reduce these computations to algebraic statements involving a set of bounded-degree polynomials. This step is usually called *arithmetization*. For basics of AIR arithmetization please refer to the excellent posts from StarkWare:

* [Arithmetization I](https://medium.com/starkware/arithmetization-i-15c046390862)
* [Arithmetization II](https://medium.com/starkware/arithmetization-ii-403c3b3f4355)
* [StarkDEX Deep Dive: the STARK Core Engine](https://medium.com/starkware/starkdex-deep-dive-the-stark-core-engine-497942d0f0ab)

Coming up with efficient arithmetizations for computations is highly non-trivial, and describing arithmetizations could be tedious and error-prone. `Air` trait aims to help with the latter, which, hopefully, also makes the former a little simpler.

To define AIR for a given computation, you'll need to implement the `Air` trait which involves the following:

1. Define base field for your computation via the `BaseElement` associated type (see [math crate](../math) for available field options).
2. Define a set of public inputs which are required for your computation via the `PublicInputs` associated type.
3. Implement `Air::new()` function. As a part of this function you should create a `ComputationContext` struct which takes degrees for all transition constraints as one of the constructor parameters.
4. Implement `context()` method which should return a reference to the `ComputationContext` struct created in `Air::new()` function.
5. Implement `evaluate_transition()` method which should evaluate [transition constraints](#Transition-constraints) over a given evaluation frame.
6. Implement `get_assertions()` method which should return a vector of [assertions](#Trace-assertions) for a given instance of your computation.
7. If your computation requires [periodic values](#Periodic-values), you can also override the default `get_periodic_column_values()` method.

For more information, take a look at the definition at the [Air trait](src/air/mod.rs) and check out [examples crate](../examples) which illustrates how to implement the trait for a several different computations.

### Transition constraints
Transition constraints define algebraic relations between two consecutive steps of a computation. In Winterfell, transition constraints are evaluated inside `evaluate_transition()` function which takes the following parameters:

- **frame**: `&EvaluationFrame<FieldElement>`, which contains vectors with current and next states of the computation.
- **periodic_values**: `&[FieldElement]`, when periodic columns are defined for a computation, this will contain values of periodic columns at the current step of the computation. Otherwise, this will be an empty slice.
- **result**: `&mut [FieldElement]`, this is the slice where constraint evaluation results should be written to.

The constraints are considered to be satisfied if and only if, after the function returns, the `result` slice contains all zeros. In general, it is important for the transition constraint evaluation function to work as follows:

* For all valid transitions between consecutive computation steps, transition constraints should evaluation to all zeros.
* For any invalid transition, at least one constraint must evaluate to a non-zero value.

Keep in mind is that since transition constraints define algebraic relations, they should be described using only algebraic operations: additions, subtractions, and multiplications. It is also important to note that multiplying register values increases constraint degree. We usually want to keep constraint degrees as low as possible to ensure reasonable proof generation time without sacrificing security. Thus, multiplications should be used judiciously - though, there are ways to ease this restriction a bit (check out [mulfib8](../examples/src/fibonacci/mulfib8/air.rs) example).

### Trace assertions
Assertions are used to specify that a valid execution trace of a computation must contain certain values in certain cells. They are frequently used to tie public inputs to a specific execution trace, but can be used to constrain a computation in other ways as well. Internally within Winterfell, assertions are converted into *boundary constraints*.

To define assertions for your computation, you'll need to implement `get_assertions()` function of the `Air` trait. Every computation must have at least one assertion. Assertions can be of the following types:

* A single assertion - such assertion specifies that a single cell of an execution trace must be equal to a specific value. For example: *value in register 0, step 0, must be equal to 1*.
* A periodic assertion - such assertion specifies that values in a given register at specified intervals should be equal to some values. For example: *values in register 0, steps 0, 8, 16, 24 etc. must be equal to 2*.
* A sequence assertion - such assertion specifies that values in a given register at specific intervals must be equal to a sequence of provided values. For example: *values in register 0, step 0 must be equal to 1, step 8 must be equal to 2, step 16 must be equal to 3 etc.*

For more information on how to define assertions see the [assertions](src/air/assertions/mod.rs) module and check out the examples in the [examples crate](../examples).

### Periodic values
Sometimes, it may be useful to define a column in an execution trace which contains a set of repeating values. For example, let's say we have a register which contains value 1 on every 4th step, and 0 otherwise. Such a column can be described with a simple periodic sequence of `[1, 0, 0, 0]`.

To define such columns for your computation, you can override `get_periodic_column_values()` method of the `Air` trait. The values of the periodic columns at a given step of the computation will be supplied to the `evaluate_transition()` method via the `periodic_values` parameter.

License
-------

This project is [MIT licensed](../LICENSE).