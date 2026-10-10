//! Strongly typed identifiers. All ids are random UUIDs so that projects can
//! be merged, imported into each other and referenced by external interchange
//! files without collisions.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub uuid::Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            /// Deterministic id, useful for tests and for stable ids derived from data.
            pub fn from_u128(v: u128) -> Self {
                Self(uuid::Uuid::from_u128(v))
            }

            pub fn as_u128(&self) -> u128 {
                self.0.as_u128()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), &self.0.simple().to_string()[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    )*};
}

define_id!(
    /// A media asset in the project media pool.
    MediaId,
    /// A sequence (timeline).
    SequenceId,
    /// A track inside a sequence.
    TrackId,
    /// A clip inside a track.
    ClipId,
    /// A marker.
    MarkerId,
    /// An effect instance in an effect stack.
    EffectId,
    /// A link group (clips that move together, e.g. video + its audio).
    LinkId,
    /// A motion composition.
    CompId,
    /// A layer inside a motion composition.
    LayerId,
    /// A multicam source clip.
    MulticamId,
    /// A color correction node / window.
    NodeId,
    /// A render job.
    JobId,
);
