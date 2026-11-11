pragma solidity >0.8.0;

// Salts are domain-separated so this deployment is ours rather than a collision with
// Automata's. CREATE2 through a shared deployer means salt + bytecode fully determine
// the address, so an unprefixed salt reproduces their live contracts exactly. The
// prefix keeps our addresses identical across every chain we deploy to, and different
// from theirs on all of them.

bytes32 constant PCCS_ROUTER_SALT = keccak256(bytes("tee-isms/v1:PCCS_ROUTER_SALT"));
bytes32 constant DCAP_ATTESTATION_SALT = keccak256(bytes("tee-isms/v1:DCAP_ATTESTATION_SALT"));

// Compute salt for any verifier version
// Usage: verifierSalt(3) returns same value as V3_VERIFIER_SALT
// Note: For new versions, use this function instead of adding new constants
function verifierSalt(uint16 version) pure returns (bytes32) {
    return keccak256(abi.encodePacked("tee-isms/v1:QUOTE_VERIFIER_V", version));
}
