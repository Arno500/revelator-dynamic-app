//! Protocol core for the PreSonus/Fender Revelator Dynamic. No I/O here: the USB layer is
//! supplied through the [`Transport`] trait.
//!
//! Public entry points: [`device::Device`] (`open`, `read_state`, `set`, `get`),
//! [`params::ALL`] (the parameter table) and [`device::State`].
//!
//! Everything about parameter ids / state slots is a hypothesis until probed on hardware; see
//! [`params::Confidence`].
pub mod appl;
pub mod device;
pub mod dsp;
pub mod frame;
pub mod fx;
pub mod fx_analog;
pub mod meters;
pub mod mixer;
pub mod params;
pub mod presetdoc;
pub mod safety;
pub mod scene;
pub mod transport;
pub mod ubjson;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use device::{slots, Controls, Device, DeviceError, State};
pub use params::{ParamDef, ALL};
pub use safety::SafetyError;
pub use transport::{Transport, TransportError};
