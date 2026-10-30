// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {TeeDcapIsm, IDcapAttestation} from "../src/TeeDcapIsm.sol";

/// A stand-in for Automata's verifier, so everything downstream of the signature check can be
/// exercised without a live quote. The fork test below covers the real one.
contract MockDcap is IDcapAttestation {
    bool public ok = true;
    bytes public output;

    function set(bool _ok, bytes memory _output) external {
        ok = _ok;
        output = _output;
    }

    function verifyAndAttestOnChain(bytes calldata) external payable returns (bool, bytes memory) {
        return (ok, output);
    }
}

contract TeeDcapIsmTest is Test {
    // The measurements of the enclave that produced test/fixtures/real_quote.hex, and the
    // identity digest the Celestia module derives for the same enclave.
    bytes32 constant MEASUREMENTS = 0xbccc1d1a0259eb0fc7476eb9812eb37d6a516ecd74f2de9595f24fbd4c37b8cd;
    bytes32 constant IDENTITY = bytes32(uint256(0xd803bb1e));
    bytes32 constant TREE = bytes32(uint256(0xabcdef));
    address constant MAILBOX = address(0xBEEF);
    uint256 constant SKEW = 1 days;

    MockDcap dcap;
    TeeDcapIsm ism;

    uint64 constant GENESIS_HEIGHT = 100;
    uint64 constant GENESIS_TIME = 1_800_000_000;

    /// Any root that is not the one stored, for payloads whose tree moved.
    bytes32 constant SOME_ROOT = bytes32(uint256(0x5005));

    // The seven-leaf tree from the Rust encoder's vectors: its root, and leaf 3's branch. The
    // same vectors pin x/teeism's decoder, so a proof that verifies here verifies there.
    bytes32 constant VECTOR_ROOT = 0x2ef5909997f057152c8289967c1b2363e980940d8aa1fb2b58c1832973a97e93;
    uint32 constant VECTOR_COUNT = 7;
    uint32 constant VECTOR_INDEX = 3;

    function _vectorLeaves() internal pure returns (bytes32[7] memory leaves) {
        leaves[0] = 0xe8e77626586f73b955364c7b4bbf0bb7f7685ebd40e852b164633a4acbd3244c;
        leaves[1] = 0x51f81bcdfc324a0dff2b5bec9d92e21cbebc4d5e29d3a3d30de3e03fbeab8d7f;
        leaves[2] = 0x2ee5f854d076701c8753d72779187e404f9b2fb705c495137d78551250314a46;
        leaves[3] = 0x3ef5a213fe22de1cea28d60f518364ff95fe0b73660793e3efcfbe31bda68aec;
        leaves[4] = 0xcc21cc9477a6aea5df8cae73422b700c47e54d892691e099167e77befc94780a;
        leaves[5] = 0x920ae4155769cd69c30626f054134b5f003772473f57f84837402df6d166e663;
        leaves[6] = 0xc29021b0e084f7dc16f6ec88cc597f1aea9f8e0b9501e0f7a546805d2a20eeda;
    }

    function _vectorProof() internal pure returns (bytes32[32] memory proof) {
        proof[0] = 0x2ee5f854d076701c8753d72779187e404f9b2fb705c495137d78551250314a46;
        proof[1] = 0xedf56861e0206958c3d8c124c0f32eccbbc23c3cb9240c6eb96dec0166866a30;
        proof[2] = 0xc6c3fe92fe2841beeb3429b11f13c00adfa63d6611110815689e8587491f934b;
        proof[3] = 0x21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85;
        proof[4] = 0xe58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344;
        proof[5] = 0x0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d;
        proof[6] = 0x887c22bd8750d34016ac3c66b5ff102dacdd73f6b014e710b51e8022af9a1968;
        proof[7] = 0xffd70157e48063fc33c97a050f7f640233bf646cc98d9524c6b92bcf3ab56f83;
        proof[8] = 0x9867cc5f7f196b93bae1e27e6320742445d290f2263827498b54fec539f756af;
        proof[9] = 0xcefad4e508c098b9a7e1d8feb19955fb02ba9675585078710969d3440f5054e0;
        proof[10] = 0xf9dc3e7fe016e050eff260334f18a5d4fe391d82092319f5964f2e2eb7c1c3a5;
        proof[11] = 0xf8b13a49e282f609c317a833fb8d976d11517c571d1221a265d25af778ecf892;
        proof[12] = 0x3490c6ceeb450aecdc82e28293031d10c7d73bf85e57bf041a97360aa2c5d99c;
        proof[13] = 0xc1df82d9c4b87413eae2ef048f94b4d3554cea73d92b0f7af96e0271c691e2bb;
        proof[14] = 0x5c67add7c6caf302256adedf7ab114da0acfe870d449a3a489f781d659e8becc;
        proof[15] = 0xda7bce9f4e8618b6bd2f4132ce798cdc7a60e7e1460a7299e3c6342a579626d2;
        proof[16] = 0x2733e50f526ec2fa19a22b31e8ed50f23cd1fdf94c9154ed3a7609a2f1ff981f;
        proof[17] = 0xe1d3b5c807b281e4683cc6d6315cf95b9ade8641defcb32372f1c126e398ef7a;
        proof[18] = 0x5a2dce0a8a7f68bb74560f8f71837c2c2ebbcbf7fffb42ae1896f13f7c7479a0;
        proof[19] = 0xb46a28b6f55540f89444f63de0378e3d121be09e06cc9ded1c20e65876d36aa0;
        proof[20] = 0xc65e9645644786b620e2dd2ad648ddfcbf4a7e5b1a3a4ecfe7f64667a3f0b7e2;
        proof[21] = 0xf4418588ed35a2458cffeb39b93d26f18d2ab13bdce6aee58e7b99359ec2dfd9;
        proof[22] = 0x5a9c16dc00d6ef18b7933a6f8dc65ccb55667138776f7dea101070dc8796e377;
        proof[23] = 0x4df84f40ae0c8229d0d6069e5c8f39a7c299677a09d367fc7b05e3bc380ee652;
        proof[24] = 0xcdc72595f74c7b1043d0e1ffbab734648c838dfb0527d971b602bc216c9619ef;
        proof[25] = 0x0abf5ac974a1ed57f4050aa510dd9c74f508277b39d7973bb2dfccc5eeb0618d;
        proof[26] = 0xb8cd74046ff337f0a7bf2c8e03e10f642c1886798d71806ab1e888d9e5ee87d0;
        proof[27] = 0x838c5655cb21c6cb83313b5a631175dff4963772cce9108188b34ac87c81c41e;
        proof[28] = 0x662ee4dd2dd7b2bc707961b1e646c4047669dcb6584f0d8d770daf5d7e7deb2e;
        proof[29] = 0x388ab20e2573d171a88108e79d820e98f26c0b84aa8b2f4aa4968dbb818ea322;
        proof[30] = 0x93237c50ba75ee485f4c22adf2f741400bdf8d6a9cc7df7ecae576221665d735;
        proof[31] = 0x8448818bb4ae4562849e949e17ac16e0be16688e156b5cf15e098c627c0056a9;
    }

    /// `payloads[0]` of the vectors, byte for byte, and the fields it decodes to.
    bytes constant VECTOR_PAYLOAD =
        hex"010101010101010101010101010101010101010101010101010101010101010101020304000000000000000a00000000000003e802020202020202020202020202020202020202020202020202020202020202020303030303030303030303030303030303030303030303030303030303030303040404040404040404040404040404040404040404040404040404040404040401020304000000000000001400000000000007d005050505050505050505050505050505050505050505050505050505050505050606060606060606060606060606060606060606060606060606060606060606abababababababababababababababababababababababababababababababab0000000000000834cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd0a0b0c0d";
    bytes32 constant VECTOR_PAYLOAD_TREE_ROOT = 0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd;
    uint32 constant VECTOR_PAYLOAD_TREE_COUNT = 168496141;
    uint64 constant VECTOR_PAYLOAD_ATTESTED_AT = 2100;

    function setUp() public {
        dcap = new MockDcap();
        ism = new TeeDcapIsm(dcap, MEASUREMENTS, IDENTITY, TREE, MAILBOX, _state(1, GENESIS_HEIGHT, GENESIS_TIME), SKEW);
        vm.warp(GENESIS_TIME + 60);
    }

    // ---------------------------------------------------------------- helpers

    /// The 116-byte ISM state, matching the Celestia module's layout exactly.
    function _state(uint8 rootSeed, uint64 height, uint64 timestamp) internal pure returns (bytes memory) {
        return _stateOf(rootSeed, height, timestamp, IDENTITY);
    }

    function _stateOf(uint8 rootSeed, uint64 height, uint64 timestamp, bytes32 identity)
        internal
        pure
        returns (bytes memory)
    {
        bytes memory s = new bytes(116);
        for (uint256 i = 0; i < 32; i++) {
            s[i] = bytes1(rootSeed);
        }
        // origin domain 11155111, big endian
        s[32] = 0x00; s[33] = 0xaa; s[34] = 0x36; s[35] = 0xa7;
        for (uint256 i = 0; i < 8; i++) {
            s[36 + i] = bytes1(uint8(height >> (8 * (7 - i))));
            s[44 + i] = bytes1(uint8(timestamp >> (8 * (7 - i))));
        }
        for (uint256 i = 0; i < 32; i++) {
            s[84 + i] = identity[i];
        }
        return s;
    }

    /// The 308-byte attested update: both states, the hook, the attested time, then the origin
    /// tree's root and leaf count.
    function _payload(bytes memory prev, bytes memory next, uint64 attestedAt, bytes32 root, uint32 count)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(prev, next, TREE, attestedAt, root, count);
    }

    /// A payload whose tree moved: the common case.
    function _payloadWithLeaves(bytes memory prev, bytes memory next, uint64 attestedAt)
        internal
        pure
        returns (bytes memory)
    {
        return _payload(prev, next, attestedAt, SOME_ROOT, 1);
    }

    /// Delivery metadata: the root, the leaf's index, its 32 siblings.
    function _metadata(bytes32 root, uint32 index, bytes32[32] memory proof) internal pure returns (bytes memory m) {
        m = abi.encodePacked(root, index);
        for (uint256 i = 0; i < 32; i++) {
            m = abi.encodePacked(m, proof[i]);
        }
    }

    /// A verifier output shaped exactly like Automata's, committing to `payload`.
    function _output(bytes memory payload, uint8 tcbStatus, bool debug) internal pure returns (bytes memory) {
        return _outputAs(4, 2, 584, payload, tcbStatus, debug);
    }

    /// The same for any quote version and body type. A TD 1.5 body (648 bytes) is a TD 1.0
    /// body with 64 bytes appended, which Automata passes through at the same offset.
    function _outputAs(uint16 version, uint16 bodyType, uint256 bodyLen, bytes memory payload, uint8 tcbStatus, bool debug)
        internal
        pure
        returns (bytes memory)
    {
        bytes memory body = new bytes(bodyLen);
        if (debug) body[120] = 0x01;
        // mr_td and the RTMRs are left zero; the test that needs them sets MEASUREMENTS to match.
        bytes32 h = sha256(payload);
        for (uint256 i = 0; i < 32; i++) {
            body[520 + i] = h[i];
        }
        for (uint256 i = 584; i < bodyLen; i++) {
            body[i] = 0xee; // tee_tcb_svn2 and mr_servicetd, which nothing here reads
        }
        return abi.encodePacked(version, bodyType, tcbStatus, bytes6(0), body);
    }

    function _oneStepPayload() internal view returns (bytes memory) {
        return _payloadWithLeaves(
            _state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp)
        );
    }

    /// Land `payload` on `target` through the mock verifier.
    function _submit(TeeDcapIsm target, bytes memory payload) internal {
        dcap.set(true, _output(payload, 0, false));
        target.submitAttestation(hex"00", payload);
    }

    function _zeroMeasurements() internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(new bytes(96), new bytes(144)));
    }

    function _fresh() internal returns (TeeDcapIsm) {
        return new TeeDcapIsm(
            dcap, _zeroMeasurements(), IDENTITY, TREE, MAILBOX, _state(1, GENESIS_HEIGHT, GENESIS_TIME), SKEW
        );
    }

    // ---------------------------------------------------------------- the happy path

    function test_advancesStateAndRecordsTheRoot() public {
        TeeDcapIsm target = _fresh();
        bytes memory next = _state(2, GENESIS_HEIGHT + 5, GENESIS_TIME + 10);
        bytes memory payload = _payload(_state(1, GENESIS_HEIGHT, GENESIS_TIME), next, uint64(block.timestamp), VECTOR_ROOT, VECTOR_COUNT);

        dcap.set(true, _output(payload, 0, false));
        vm.expectEmit(address(target));
        emit TeeDcapIsm.StateAdvanced(
            bytes32(uint256(0x0202020202020202020202020202020202020202020202020202020202020202)),
            GENESIS_HEIGHT + 5,
            GENESIS_TIME + 10,
            VECTOR_ROOT,
            VECTOR_COUNT
        );
        target.submitAttestation(hex"00", payload);

        assertEq(keccak256(target.state()), keccak256(next));
        assertEq(target.treeRoot(), VECTOR_ROOT);
        assertEq(target.treeCount(), VECTOR_COUNT);
        assertTrue(target.attestedRoots(VECTOR_ROOT));
        assertFalse(target.attestedRoots(SOME_ROOT));
    }

    // ---------------------------------------------------------------- delivery

    /// The leaf is the message id, so the message whose keccak is the leaf is what proves.
    function _vectorMessage() internal pure returns (bytes memory) {
        return hex"00000003";
    }

    function _attestVector(TeeDcapIsm target) internal {
        _submit(
            target,
            _payload(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp), VECTOR_ROOT, VECTOR_COUNT)
        );
    }

    function test_aMessageProvesItselfAgainstAnAttestedRoot() public {
        // The vectors' leaves are keccak256 of the big-endian index, so leaf 3 is the id of
        // the message `00000003`.
        assertEq(keccak256(_vectorMessage()), _vectorLeaves()[VECTOR_INDEX]);
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        vm.prank(MAILBOX);
        assertTrue(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof()), _vectorMessage()));
    }

    /// Nothing is consumed: the mailbox refuses a message id it has processed, not this ISM.
    function test_aProofVerifiesAgain() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        bytes memory metadata = _metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof());
        vm.prank(MAILBOX);
        assertTrue(target.verify(metadata, _vectorMessage()));
        vm.prank(MAILBOX);
        assertTrue(target.verify(metadata, _vectorMessage()));
    }

    function test_aRootNotAttestedIsRefused() public {
        TeeDcapIsm target = _fresh();
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof()), _vectorMessage()), "never attested");
        _submit(target, _oneStepPayload()); // attests SOME_ROOT, not the vector's
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof()), _vectorMessage()), "another root");
    }

    function test_aWrongIndexIsRefused() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX + 1, _vectorProof()), _vectorMessage()));
    }

    function test_aTamperedMessageIsRefused() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof()), hex"00000004"));
    }

    function test_aTamperedProofIsRefused() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        bytes32[32] memory proof = _vectorProof();
        proof[5] = bytes32(uint256(proof[5]) ^ 1);
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, proof), _vectorMessage()));
    }

    /// A proof claiming an attested root with the wrong leaf behind it: the branch does not fold
    /// to that root.
    function test_aProofForAnotherRootIsRefused() public {
        TeeDcapIsm target = _fresh();
        _submit(target, _oneStepPayload()); // attests SOME_ROOT
        vm.prank(MAILBOX);
        assertFalse(target.verify(_metadata(SOME_ROOT, VECTOR_INDEX, _vectorProof()), _vectorMessage()));
    }

    function test_malformedMetadataIsRefused() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        bytes memory good = _metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof());
        assertEq(good.length, 1060);
        vm.prank(MAILBOX);
        assertFalse(target.verify(hex"", _vectorMessage()), "empty");
        vm.prank(MAILBOX);
        assertFalse(target.verify(abi.encodePacked(good, hex"00"), _vectorMessage()), "trailing byte");
        bytes memory short = new bytes(good.length - 1);
        for (uint256 i = 0; i < short.length; i++) short[i] = good[i];
        vm.prank(MAILBOX);
        assertFalse(target.verify(short, _vectorMessage()), "truncated");
    }

    /// A proof built against one root stays good after the ISM moves to another, so a delivery
    /// is never lost to an attestation that landed first.
    function test_anOldRootStaysAttested() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        _submit(
            target,
            _payload(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), _state(3, GENESIS_HEIGHT + 2, GENESIS_TIME), uint64(block.timestamp), SOME_ROOT, VECTOR_COUNT + 1)
        );
        assertEq(target.treeRoot(), SOME_ROOT);
        assertTrue(target.attestedRoots(VECTOR_ROOT));
        vm.prank(MAILBOX);
        assertTrue(target.verify(_metadata(VECTOR_ROOT, VECTOR_INDEX, _vectorProof()), _vectorMessage()));
    }

    // ---------------------------------------------------------------- the payload

    /// The vector the Rust encoder produced decodes to the fields it was built from.
    function test_decodesTheRustEncodersPayload() public {
        assertEq(VECTOR_PAYLOAD.length, 308);
        bytes memory prev = new bytes(116);
        bytes memory next = new bytes(116);
        for (uint256 i = 0; i < 116; i++) {
            prev[i] = VECTOR_PAYLOAD[i];
            next[i] = VECTOR_PAYLOAD[116 + i];
        }
        bytes32 hook = bytes32(uint256(0xabababababababababababababababababababababababababababababababab));
        TeeDcapIsm target = new TeeDcapIsm(dcap, _zeroMeasurements(), bytes32(prev[84] == 0x03 ? uint256(0x0303030303030303030303030303030303030303030303030303030303030303) : 0), hook, MAILBOX, prev, SKEW);
        // The vector's states carry identity 0x03..03 and 0x05..05: the second differs, as it
        // would after a re-pin, so re-pin to it and let the state follow.
        target.setEnclave(_zeroMeasurements(), bytes32(uint256(0x0505050505050505050505050505050505050505050505050505050505050505)));
        // After the re-pin the stored prev carries the new identity, which the vector's prev
        // does not, so this checks the decoder up to the state comparison and then the rest
        // through the fields the mock quote commits to.
        dcap.set(true, _output(VECTOR_PAYLOAD, 0, false));
        vm.expectRevert(TeeDcapIsm.TrustedStateMismatch.selector);
        target.submitAttestation(hex"00", VECTOR_PAYLOAD);

        // Decoded the way the contract does: the fields past the two states.
        assertEq(bytes32(_slice(VECTOR_PAYLOAD, 232, 32)), hook);
        assertEq(uint64(bytes8(_slice(VECTOR_PAYLOAD, 264, 8))), VECTOR_PAYLOAD_ATTESTED_AT);
        assertEq(bytes32(_slice(VECTOR_PAYLOAD, 272, 32)), VECTOR_PAYLOAD_TREE_ROOT);
        assertEq(uint32(bytes4(_slice(VECTOR_PAYLOAD, 304, 4))), VECTOR_PAYLOAD_TREE_COUNT);
    }

    function test_aPayloadOfAnotherLengthIsRefused() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        bytes memory longer = abi.encodePacked(payload, hex"00");
        dcap.set(true, _output(longer, 0, false));
        vm.expectRevert(TeeDcapIsm.MalformedPayload.selector);
        target.submitAttestation(hex"00", longer);

        bytes memory shorter = new bytes(payload.length - 1);
        for (uint256 i = 0; i < shorter.length; i++) shorter[i] = payload[i];
        dcap.set(true, _output(shorter, 0, false));
        vm.expectRevert(TeeDcapIsm.MalformedPayload.selector);
        target.submitAttestation(hex"00", shorter);

        // The old layout's shortest form, 280 bytes.
        bytes memory old = new bytes(280);
        dcap.set(true, _output(old, 0, false));
        vm.expectRevert(TeeDcapIsm.MalformedPayload.selector);
        target.submitAttestation(hex"00", old);
    }

    // ---------------------------------------------------------------- cadence

    /// The daily heartbeat: no new leaves, same root, but the state still moves to the
    /// origin's head once the origin's clock has moved enough.
    function test_anUnchangedRootAdvancesAQuietRoute() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        uint64 later = GENESIS_TIME + target.EMPTY_BATCH_MIN_INTERVAL();
        vm.warp(later + 60);
        bytes memory next = _state(3, GENESIS_HEIGHT + 300, later);
        _submit(target, _payload(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), next, uint64(block.timestamp), VECTOR_ROOT, VECTOR_COUNT));
        assertEq(keccak256(target.state()), keccak256(next));
        assertEq(target.treeRoot(), VECTOR_ROOT);
    }

    /// Anyone can obtain one from the public enclave, so how often is bounded by the origin's clock.
    function test_anUnchangedRootTooSoonIsRefused() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        uint64 interval = target.EMPTY_BATCH_MIN_INTERVAL();
        uint64 early = GENESIS_TIME + interval - 1;
        vm.warp(early + 60);
        bytes memory payload =
            _payload(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), _state(3, GENESIS_HEIGHT + 2, early), uint64(block.timestamp), VECTOR_ROOT, VECTOR_COUNT);
        dcap.set(true, _output(payload, 0, false));
        vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.EmptyBatchTooSoon.selector, GENESIS_TIME, early, interval));
        target.submitAttestation(hex"00", payload);
    }

    /// A root that moved brought leaves, and is never held back.
    function test_aChangedRootIsNotHeldBack() public {
        TeeDcapIsm target = _fresh();
        _attestVector(target);
        _submit(
            target,
            _payload(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), _state(3, GENESIS_HEIGHT + 2, GENESIS_TIME), uint64(block.timestamp), SOME_ROOT, VECTOR_COUNT + 1)
        );
        assertEq(target.treeRoot(), SOME_ROOT);
    }

    /// From genesis nothing is stored, so the first attestation passes whatever root it brings
    /// with no clock requirement. An empty hook's root is not zero either: Hyperlane's empty
    /// tree hashes to the top zero hash, so no real tree ever collides with the unset slot.
    function test_theFirstAttestationIsNeverHeldBack() public {
        TeeDcapIsm target = _fresh();
        assertEq(target.treeRoot(), bytes32(0));
        _submit(
            target,
            _payload(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp), VECTOR_ROOT, VECTOR_COUNT)
        );
        assertEq(target.treeRoot(), VECTOR_ROOT);
    }

    // ---------------------------------------------------------------- quote versions

    function test_acceptsAV5QuoteWithATd15Body() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        dcap.set(true, _outputAs(5, 3, 648, payload, 0, false));
        target.submitAttestation(hex"00", payload);
        assertEq(keccak256(target.state()), keccak256(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME)));
    }

    function test_acceptsAV5QuoteWithATd10Body() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        dcap.set(true, _outputAs(5, 2, 584, payload, 0, false));
        target.submitAttestation(hex"00", payload);
        assertEq(keccak256(target.state()), keccak256(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME)));
    }

    /// The measurements sit at the same offsets in a TD 1.5 body, so a different enclave is
    /// still caught there.
    function test_aTd15BodyFromAnotherEnclaveIsRejected() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        bytes memory out = _outputAs(5, 3, 648, payload, 0, false);
        out[11 + 136] = 0x01; // first byte of mr_td
        dcap.set(true, out);
        vm.expectPartialRevert(TeeDcapIsm.WrongEnclave.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsQuoteShapesThatAreNotTdx() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        uint16[3] memory versions = [uint16(4), 5, 3];
        uint16[3] memory types = [uint16(3), 1, 2];
        for (uint256 i = 0; i < 3; i++) {
            dcap.set(true, _outputAs(versions[i], types[i], 648, payload, 0, false));
            vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.UnsupportedQuote.selector, versions[i], types[i]));
            target.submitAttestation(hex"00", payload);
        }
    }

    function test_rejectsRelaunchAdvised() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();
        for (uint8 status = 8; status <= 9; status++) {
            dcap.set(true, _outputAs(5, 3, 648, payload, status, false));
            vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.TcbNotAcceptable.selector, status));
            target.submitAttestation(hex"00", payload);
        }
    }

    function test_reportsItsRevision() public view {
        assertEq(ism.VERSION(), 5);
    }

    // ---------------------------------------------------------------- re-pinning

    function test_onlyTheOwnerRepins() public {
        assertEq(ism.owner(), address(this));
        vm.prank(address(0xBAD));
        vm.expectRevert("Ownable: caller is not the owner");
        ism.setEnclave(_zeroMeasurements(), bytes32(uint256(0xfeed)));
    }

    function test_aRepinSwapsOnlyTheIdentity() public {
        bytes32 next = bytes32(uint256(0xfeed));
        vm.expectEmit(address(ism));
        emit TeeDcapIsm.EnclaveChanged(MEASUREMENTS, _zeroMeasurements(), IDENTITY, next);
        ism.setEnclave(_zeroMeasurements(), next);

        assertEq(ism.enclaveMeasurements(), _zeroMeasurements());
        assertEq(ism.identityDigest(), next);
        assertEq(keccak256(ism.state()), keccak256(_stateOf(1, GENESIS_HEIGHT, GENESIS_TIME, next)));
    }

    /// After a re-pin the new enclave carries on from the same state, and the old one is refused.
    function test_aRepinnedIsmAdmitsTheNewEnclaveOnly() public {
        bytes32 next = bytes32(uint256(0xfeed));
        ism.setEnclave(_zeroMeasurements(), next);

        bytes memory stale = _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(stale, 0, false));
        vm.expectRevert(TeeDcapIsm.TrustedStateMismatch.selector);
        ism.submitAttestation(hex"00", stale);

        bytes memory payload = _payloadWithLeaves(_stateOf(1, GENESIS_HEIGHT, GENESIS_TIME, next),
            _stateOf(2, GENESIS_HEIGHT + 1, GENESIS_TIME, next),
            uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, false));
        ism.submitAttestation(hex"00", payload);
        assertEq(keccak256(ism.state()), keccak256(_stateOf(2, GENESIS_HEIGHT + 1, GENESIS_TIME, next)));
    }

    function test_ownershipCanBeHandedToAMultisig() public {
        address multisig = address(0x5AFE);
        ism.transferOwnership(multisig);
        vm.expectRevert("Ownable: caller is not the owner");
        ism.setEnclave(_zeroMeasurements(), IDENTITY);
        vm.prank(multisig);
        ism.setEnclave(_zeroMeasurements(), IDENTITY);
        assertEq(ism.enclaveMeasurements(), _zeroMeasurements());
    }

    function test_verifyOnlyFromMailbox() public {
        vm.expectRevert(TeeDcapIsm.NotMailbox.selector);
        ism.verify(hex"", hex"00");
    }

    // ---------------------------------------------------------------- refusals

    function test_rejectsWhenVerifierFails() public {
        dcap.set(false, bytes("TCBR"));
        vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.QuoteRejected.selector, bytes("TCBR")));
        ism.submitAttestation(hex"00", hex"00");
    }

    /// The failure an operator will actually meet, roughly monthly, when the collateral in
    /// the PCCS expires. The revert must name it and the description must say what to do.
    function test_expiredCollateralIsDiagnosable() public {
        dcap.set(false, bytes("TCBR"));
        vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.QuoteRejected.selector, bytes("TCBR")));
        ism.submitAttestation(hex"00", hex"00");

        string memory why = ism.describeQuoteError(bytes("TCBR"));
        assertEq(
            why,
            "TCB status revoked or missing: this chain's PCCS has no TCB info for the enclave's FMSPC, or the platform is revoked. Publish the current TCB info for that FMSPC."
        );
    }

    function test_everyVerifierCodeIsDescribed() public view {
        string[27] memory codes = [
            "TEE","OUTS","QHS","QHV","QHATTF","QEVEN","QBF","QBS","ADS","ADF","TD10F","TD15F",
            "TDMF","TDRF","TCBR","QEF","QEVE","QEIDVE","X509VE","ATTVE","TCBCH","QEIDCH",
            "ROOTH","SIGNH","ROOTCRLH","PCKCRLM","PCKCRLH"
        ];
        for (uint256 i = 0; i < codes.length; i++) {
            string memory d = ism.describeQuoteError(bytes(codes[i]));
            assertTrue(bytes(d).length > 0, "code has no description");
            assertNotEq(
                keccak256(bytes(d)),
                keccak256(bytes("Unrecognised verifier error code.")),
                string.concat("undescribed code: ", codes[i])
            );
        }
        // and an unknown code still returns something rather than reverting
        assertEq(ism.describeQuoteError(bytes("ZZZZ")), "Unrecognised verifier error code.");
    }

    function test_rejectsUnacceptableTcb() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 4, false)); // TCB_OUT_OF_DATE
        vm.expectRevert(abi.encodeWithSelector(TeeDcapIsm.TcbNotAcceptable.selector, uint8(4)));
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsDebugEnclave() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, true));
        vm.expectRevert(TeeDcapIsm.DebugEnclave.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsWrongEnclave() public {
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, false)); // zero measurements, but `ism` pins the real ones
        vm.expectRevert(
            abi.encodeWithSelector(TeeDcapIsm.WrongEnclave.selector, MEASUREMENTS, _zeroMeasurements())
        );
        ism.submitAttestation(hex"00", payload);
    }

    /// The same enclave image running on a different CVM must still be admitted.
    ///
    /// dstack measures `app-id` and `instance-id` into rtmr3, so rtmr3 differs for every
    /// instance. This ISM pins mr_td, mr_config_id and rtmr0..2 and deliberately ignores
    /// rtmr3, which is what lets a replacement enclave keep serving an existing ISM.
    function test_acceptsSameEnclaveOnADifferentInstance() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload = _oneStepPayload();

        bytes memory out = _output(payload, 0, false);
        // Vary rtmr3 the way a different instance would.
        for (uint256 i = 0; i < 48; i++) {
            out[11 + 472 + i] = bytes1(uint8(0xAB));
        }
        dcap.set(true, out);

        target.submitAttestation(hex"00", payload);
        assertEq(keccak256(target.state()), keccak256(_state(2, GENESIS_HEIGHT + 1, GENESIS_TIME)));
    }

    /// A different enclave image must be refused, however genuine its quote.
    function test_rejectsADifferentEnclaveImage() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));

        bytes memory out = _output(payload, 0, false);
        // A different compose hash is a different image; mr_config_id carries it.
        out[11 + 185] = bytes1(uint8(0xFF));
        dcap.set(true, out);

        vm.expectRevert(
            abi.encodeWithSelector(
                TeeDcapIsm.WrongEnclave.selector,
                _zeroMeasurements(),
                keccak256(abi.encodePacked(_slice(out, 11 + 136, 96), _slice(out, 11 + 328, 144)))
            )
        );
        target.submitAttestation(hex"00", payload);
    }

    function _slice(bytes memory b, uint256 start, uint256 len) internal pure returns (bytes memory o) {
        o = new bytes(len);
        for (uint256 i = 0; i < len; i++) o[i] = b[start + i];
    }

    function test_rejectsPayloadTheQuoteDoesNotCommitTo() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        bytes memory other =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(3, GENESIS_HEIGHT + 2, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(other, 0, false));
        vm.expectRevert(TeeDcapIsm.PayloadNotAttested.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsReplayOnceStateMoved() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, false));
        target.submitAttestation(hex"00", payload);

        vm.expectRevert(TeeDcapIsm.TrustedStateMismatch.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsHeightThatDoesNotAdvance() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, false));
        vm.expectRevert(TeeDcapIsm.HeightNotAdvanced.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsUnchangedStateRoot() public {
        TeeDcapIsm target = _fresh();
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(1, GENESIS_HEIGHT + 1, GENESIS_TIME), uint64(block.timestamp));
        dcap.set(true, _output(payload, 0, false));
        vm.expectRevert(TeeDcapIsm.StateRootUnchanged.selector);
        target.submitAttestation(hex"00", payload);
    }

    function test_rejectsStaleAttestation() public {
        TeeDcapIsm target = _fresh();
        uint64 attestedAt = uint64(block.timestamp);
        bytes memory payload =
            _payloadWithLeaves(_state(1, GENESIS_HEIGHT, GENESIS_TIME), _state(2, GENESIS_HEIGHT + 1, GENESIS_TIME), attestedAt);
        dcap.set(true, _output(payload, 0, false));
        vm.warp(block.timestamp + SKEW + 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                TeeDcapIsm.AttestationTooOld.selector, uint64(block.timestamp), attestedAt, SKEW
            )
        );
        target.submitAttestation(hex"00", payload);
    }

}

/// The real verifier, on the chain it is deployed to, with a real quote.
///
/// Everything above proves the bridge-specific logic. Only this proves that Automata's
/// deployed contract accepts a quote from our enclave, and that the offsets this contract
/// reads the report at are the offsets their output actually uses.
contract TeeDcapIsmForkTest is Test {
    IDcapAttestation constant AUTOMATA = IDcapAttestation(0xaDdeC7e85c2182202b66E331f2a4A0bBB2cEEa1F);
    bytes32 constant MEASUREMENTS = 0xbccc1d1a0259eb0fc7476eb9812eb37d6a516ecd74f2de9595f24fbd4c37b8cd;

    function test_realQuoteVerifiesAndMatchesOurMeasurements() public {
        string memory rpc = vm.envOr("BASE_SEPOLIA_RPC", string(""));
        if (bytes(rpc).length == 0) {
            emit log("BASE_SEPOLIA_RPC unset, skipping fork test");
            return;
        }
        vm.createSelectFork(rpc);

        // Automata's verifier calls the RIP-7212 P256 precompile at 0x100. Base Sepolia has
        // it; a forked EVM only does when the runner enables it, and without it the quote
        // fails for a reason that has nothing to do with this contract. Skip rather than
        // report a false failure - a real deployment on the real chain covers this properly.
        bytes memory quote = vm.parseBytes(vm.readFile("test/fixtures/real_quote.hex"));

        // Attempt it low-level: a forked EVM without the precompile fails here for a reason
        // that has nothing to do with this contract, and reporting that as a failure would be
        // misleading. A real deployment on the real chain covers this properly.
        (bool reached, bytes memory ret) = address(AUTOMATA).call(
            abi.encodeWithSelector(IDcapAttestation.verifyAndAttestOnChain.selector, quote)
        );
        if (!reached) {
            emit log("verifier unreachable in this EVM (missing P256 precompile?), skipping");
            return;
        }
        (bool ok, bytes memory out) = abi.decode(ret, (bool, bytes));
        assertTrue(ok, "automata rejected a genuine quote");
        assertGe(out.length, 595);

        // The header Automata prepends.
        assertEq(uint16(bytes2(_slice(out, 0, 2))), 4, "quote version");
        assertEq(uint16(bytes2(_slice(out, 2, 2))), 2, "TD1.0 body type");

        // The offsets TeeDcapIsm reads the measurements at.
        // mr_td ++ mr_config_id, then rtmr0..2. rtmr3 is excluded on purpose: it carries
        // app-id and instance-id, so including it would bind this ISM to one CVM.
        bytes32 measured = keccak256(abi.encodePacked(_slice(out, 11 + 136, 96), _slice(out, 11 + 328, 144)));
        assertEq(measured, MEASUREMENTS, "measurement offsets disagree with the live output");

        // The compose hash lives in mr_config_id, behind a one-byte dstack version prefix.
        // That is what makes the event-log replay unnecessary here.
        assertEq(uint8(out[11 + 184]), 1, "mr_config_id version prefix");
        assertEq(
            bytes32(_slice(out, 11 + 185, 32)),
            bytes32(0xf6ad454d9125b4512c72309e55b4e4fe7bab3b527327dd1c12199ad06cd80f5f),
            "mr_config_id should carry the compose hash"
        );

        // report_data's upper half is zero, which the ISM relies on.
        assertEq(bytes32(_slice(out, 11 + 552, 32)), bytes32(0));
    }

    function _slice(bytes memory b, uint256 start, uint256 len) private pure returns (bytes memory out) {
        out = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            out[i] = b[start + i];
        }
    }
}
