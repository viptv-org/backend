// The catalog extra negotiation rules now live in the shared `viptv-provider`
// crate so fat clients apply the same capability logic as the backend.
pub use viptv_provider::extras::*;
