//! The bridge's own Celestia chain, one file per subcommand: `up` (genesis and node), `core`
//! (Hyperlane core) and `warp` (the Celestia side of the warp routes).

pub mod core;
pub mod up;
pub mod warp;

pub const CONTAINER: &str = "teeism-celestia";

/// Keys derived from CELESTIA_MNEMONIC, by account index. 0 is what Keplr selects when the
/// phrase is imported. 3 is the paymaster's `bridge` key, added by `teeism paymaster`.
const KEYS: [(&str, u32); 4] = [("user", 0), ("relayer", 1), ("validator", 2), ("smoke", 5)];
pub const BRIDGE_KEY: (&str, u32) = ("bridge", 3);
