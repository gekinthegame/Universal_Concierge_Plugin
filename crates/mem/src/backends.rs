//! Network backend plugins. Each network is one feature-gated module here;
//! adding one touches nothing else in core. See `ADDING_A_BACKEND.md`.

#[cfg(any(feature = "pinata", feature = "ipfs"))]
pub(crate) mod car; // shared CARv1 builder for CAR-ingesting backends

#[cfg(feature = "pinata")]
pub mod pinata;

#[cfg(feature = "ipfs")]
pub mod ipfs;
