//! The bridge's own Celestia chain: the node, Hyperlane on it, the paymaster, the mocha light
//! node Eden needs, and the `celestia-appd` wrapper the other steps use.

pub mod appd;
pub mod hyperlane;
pub mod mocha;
pub mod paymaster;
pub mod up;
