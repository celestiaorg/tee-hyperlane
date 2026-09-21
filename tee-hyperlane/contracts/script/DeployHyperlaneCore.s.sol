// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Mailbox} from "@hyperlane-xyz/core/contracts/Mailbox.sol";
import {MerkleTreeHook} from "@hyperlane-xyz/core/contracts/hooks/MerkleTreeHook.sol";

/// Hyperlane core for a chain that has none.
///
/// Sepolia, Arbitrum and Base all have a canonical Hyperlane deployment that this bridge
/// reuses. Eden does not, so its mailbox and merkle tree hook are ours.
///
/// The ordering matters and is the reason this is one script. `MerkleTreeHook` takes the
/// mailbox in its constructor, while the mailbox needs the hook in `initialize`; doing it in
/// one run is what lets each have the other. `initialize` is separate from the constructor,
/// so the mailbox exists before it is told about anything.
///
/// The hook is set as **both** the required and the default hook by `InitHyperlaneCore`.
/// Required is the one that matters: a message that is not inserted into the merkle tree can
/// never be attested, and a router that forgets to set its own hook would otherwise dispatch
/// messages that are unprovable forever.
contract DeployHyperlaneCore is Script {
    function run() external {
        uint32 domain = uint32(vm.envUint("LOCAL_DOMAIN"));
        address owner = vm.envAddress("OWNER");

        vm.startBroadcast();

        // Deploy only. `initialize` is a separate step because the ISM this mailbox will
        // trust has to name the mailbox in *its* constructor, so neither can be built
        // knowing the other. Deploy both, then deploy the ISM, then initialize.
        Mailbox mailbox = new Mailbox(domain);
        MerkleTreeHook hook = new MerkleTreeHook(address(mailbox));

        vm.stopBroadcast();

        console.log("Mailbox        ", address(mailbox));
        console.log("MerkleTreeHook ", address(hook));
        console.log("domain         ", domain);
        console.log("initialize     ", "run DEFAULT_ISM=<ism> forge script InitHyperlaneCore");
        console.log("owner          ", owner);
    }
}

/// The second half: point the mailbox at the ISM that pins it.
contract InitHyperlaneCore is Script {
    function run() external {
        Mailbox mailbox = Mailbox(vm.envAddress("MAILBOX"));
        address hook = vm.envAddress("MERKLE_TREE_HOOK");
        address ism = vm.envAddress("DEFAULT_ISM");
        address owner = vm.envAddress("OWNER");

        vm.startBroadcast();
        mailbox.initialize(owner, ism, hook, hook);
        vm.stopBroadcast();

        console.log("mailbox        ", address(mailbox));
        console.log("default ism    ", ism);
        console.log("required hook  ", hook);
    }
}
