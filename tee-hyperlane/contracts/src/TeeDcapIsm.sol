// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {MerkleLib} from "@hyperlane-xyz/core/contracts/libs/Merkle.sol";

/// Automata's deployed DCAP quote verifier.
///
/// This is the whole of the cryptography, and none of it is ours. It checks Intel's signature
/// chain, the PCK certificate chain, the CRLs and the TCB status, then hands back the parsed
/// TD report. Deployed and maintained by Automata; addresses are in their network registry.
interface IDcapAttestation {
    function verifyAndAttestOnChain(bytes calldata rawQuote)
        external
        payable
        returns (bool success, bytes memory output);
}

interface IInterchainSecurityModule {
    function moduleType() external view returns (uint8);
    function verify(bytes calldata metadata, bytes calldata message) external returns (bool);
}

/// A Hyperlane ISM that admits messages attested by a TDX enclave.
///
/// The counterpart of Celestia's `x/teeism`, and deliberately the same shape: one submission
/// advances the trusted state and records the origin merkle tree's root at that state, because
/// there is only ever one quote and it covers both. A message then proves itself at delivery,
/// with a merkle branch against any root this ISM has attested, so the attestation is the same
/// size whether the origin block carried no messages or thousands.
///
/// This contract verifies no cryptography. `verifyAndAttestOnChain` above does that, exactly
/// as `google/go-tdx-guest` does on the Celestia side. What is left here is the part that is
/// specific to this bridge: is it our enclave, does the quote commit to this payload, and is
/// the transition legal.
///
/// The one structural difference from the Celestia module is where collateral lives. There it
/// travels in the transaction; Automata keeps it in an on-chain PCCS, so the platform's TCB
/// record must have been published on this chain. That is a data prerequisite, not a trust
/// one: the record is signed by Intel and checked on upload.
///
/// Which enclave it admits is the owner's to change (`setEnclave`), so that a platform update
/// under the enclave (a new OS image, new firmware) or a replacement enclave is a transaction
/// rather than a new ISM. That is emergency control over the bridge, held by one key for now.
contract TeeDcapIsm is IInterchainSecurityModule, Ownable {
    // ---------------------------------------------------------------- layout
    //
    // Automata returns abi.encodePacked(uint16 version, uint16 bodyType, uint8 tcbStatus,
    // bytes6 fmspc, bytes quoteBody, ...), with the TD report body at offset 11, for v4 and v5
    // quotes alike. A TD 1.5 body is a TD 1.0 body with 64 bytes appended, so every offset below
    // holds for both. Verified against a live v4 quote and a live call to their verifier.
    uint256 private constant BODY = 11;
    uint256 private constant OFF_TD_ATTRIBUTES = BODY + 120;
    /// mr_td and mr_config_id are adjacent, as are rtmr0..2, so the pinned measurements are
    /// two contiguous runs rather than five slices.
    uint256 private constant OFF_MRTD = BODY + 136; // mr_td ++ mr_config_id, 96 bytes
    uint256 private constant LEN_MRTD = 96;
    uint256 private constant OFF_RTMR0 = BODY + 328; // rtmr0 ++ rtmr1 ++ rtmr2, 144 bytes
    uint256 private constant LEN_RTMR = 144;
    uint256 private constant OFF_REPORT_DATA = BODY + 520;
    uint256 private constant MIN_OUTPUT = BODY + 584;

    uint16 private constant QUOTE_VERSION_4 = 4;
    uint16 private constant QUOTE_VERSION_5 = 5;
    uint16 private constant BODY_TYPE_TD10 = 2;
    uint16 private constant BODY_TYPE_TD15 = 3;

    /// TCB levels this bridge accepts, matching the Celestia module's allowlist.
    /// 0 is OK, 1 is SW_HARDENING_NEEDED. Anything higher means Intel has published a reason
    /// the platform may be compromised, and a bridge that keeps minting against such a
    /// platform is trading other people's funds for its own uptime. That includes v5's
    /// TD_RELAUNCH_ADVISED (8, 9): the CVM has to be relaunched before it attests again.
    uint8 private constant TCB_OK = 0;
    uint8 private constant TCB_SW_HARDENING_NEEDED = 1;

    /// bit 0 of TD_ATTRIBUTES. A debug TD is transparent to its host, so its measurements
    /// prove nothing about what actually ran.
    uint8 private constant TD_DEBUG = 0x01;

    // ---------------------------------------------------------------- ism state
    uint256 private constant STATE_BYTES = 116;
    /// `prev_state(116) new_state(116) merkle_tree(32) attested_at(8) tree_root(32) tree_count(4)`
    uint256 private constant ATTESTED_UPDATE_BYTES = 2 * STATE_BYTES + 32 + 8 + 32 + 4;
    /// `root(32) index(4) proof(32 * 32)`
    uint256 private constant METADATA_BYTES = 32 + 4 + 32 * 32;

    /// How far an attestation that brings no new leaves must move the origin's clock. Such an
    /// attestation keeps a quiet route's trusted state close to its origin's head; it cannot
    /// hide a message, because a message proves itself against the tree root, and a root that
    /// has not changed holds exactly the leaves it held. The interval bounds how often anyone,
    /// through the public enclave, can advance this ISM with nothing in it. The Celestia module
    /// holds the same bound.
    uint64 public constant EMPTY_BATCH_MIN_INTERVAL = 30 minutes;

    /// Hyperlane's module type for this bridge's ISMs, matching the Celestia module id.
    uint8 public constant MODULE_TYPE = 43;

    /// Which revision of this contract is deployed. `teeism isms evm` replaces an ISM whose
    /// `VERSION` differs even when it pins the right enclave. 2 accepts v5 quotes; the first
    /// deployments have no `VERSION` at all. 3 lets the owner re-pin the enclave. 4 bounds how
    /// often a batch with no messages may advance the state. 5 attests the origin tree's root
    /// and has each message prove itself at delivery, instead of carrying every message id.
    uint8 public constant VERSION = 5;

    IDcapAttestation public immutable dcap;

    /// keccak256(mr_td ++ mr_config_id ++ rtmr0 ++ rtmr1 ++ rtmr2).
    ///
    /// Everything here is signed by Intel as part of the TD report, and every field is a
    /// property of the code and the environment rather than of the machine it ran on:
    ///
    ///   mr_td         the dstack OS image and the VM's vCPU/memory shape
    ///   mr_config_id  0x01 ++ compose-hash ++ padding, which pins our container image
    ///                 digest and the whole compose file
    ///   rtmr0..2      the boot sequence
    ///
    /// rtmr3 is deliberately absent. dstack measures `app-id` and `instance-id` into it, so
    /// it differs for every CVM; pinning it would tie this ISM to one instance and break on
    /// the first enclave replacement. Leaving it out costs nothing, because `mr_config_id`
    /// already carries the compose hash that the Celestia module has to replay the event log
    /// to recover.
    bytes32 public enclaveMeasurements;

    /// The identity digest the attested state must carry, which is the same 32 bytes the
    /// Celestia ISM pins. Redundant with the measurements above and kept anyway, so the two
    /// chains name the same enclave in a form a human can compare.
    bytes32 public identityDigest;

    /// Origin merkle tree hook, as a Hyperlane message addresses it.
    bytes32 public immutable merkleTreeAddress;

    address public immutable mailbox;

    /// How far ahead of the attested head this chain's clock may be before a submission is
    /// refused. The Celestia module bounds the same way.
    uint256 public immutable maxQuoteSkew;

    bytes public state;

    /// The origin tree's root and leaf count at `state`, as the last attestation proved them.
    bytes32 public treeRoot;
    uint32 public treeCount;
    /// Every root an attestation has ever carried. A message proves itself against any of
    /// them, so a proof built against one root stays good after the ISM moves on, and an
    /// attestation landing ahead of the relayer's costs nobody a delivery.
    mapping(bytes32 => bool) public attestedRoots;

    event StateAdvanced(
        bytes32 indexed stateRoot, uint64 height, uint64 timestamp, bytes32 treeRoot, uint32 treeCount
    );
    event EnclaveChanged(
        bytes32 previousMeasurements, bytes32 measurements, bytes32 previousIdentity, bytes32 identity
    );

    /// The verifier refused the quote. `code` is Automata's four-letter reason, kept raw so
    /// the failure path stays cheap; call `describeQuoteError` off chain to expand it.
    error QuoteRejected(bytes code);
    error MalformedOutput();
    error UnsupportedQuote(uint16 version, uint16 bodyType);
    error TcbNotAcceptable(uint8 status);
    error DebugEnclave();
    error WrongEnclave(bytes32 expected, bytes32 measured);
    error PayloadNotAttested();
    error ReportDataNotPadded();
    error MalformedPayload();
    error TrustedStateMismatch();
    error OriginDomainChanged();
    error HeightNotAdvanced();
    error TimestampWentBackwards();
    error StateRootUnchanged();
    error IdentityChanged();
    error MerkleTreeMismatch();
    error ClockBehindAttestation(uint64 blockTime, uint64 attestedAt);
    error AttestationTooOld(uint64 blockTime, uint64 attestedAt, uint256 maxSkew);
    error AttestedBeforeState(uint64 attestedAt, uint64 stateTimestamp);
    error EmptyBatchTooSoon(uint64 previous, uint64 next, uint64 minInterval);
    error NotMailbox();

    constructor(
        IDcapAttestation _dcap,
        bytes32 _enclaveMeasurements,
        bytes32 _identityDigest,
        bytes32 _merkleTreeAddress,
        address _mailbox,
        bytes memory _genesisState,
        uint256 _maxQuoteSkew
    ) Ownable() {
        if (_genesisState.length != STATE_BYTES) revert MalformedPayload();
        // The genesis state has to name the enclave this ISM pins, or the first attestation
        // would be refused and the ISM would be dead on arrival.
        if (_readBytes32(_genesisState, 84) != _identityDigest) revert IdentityChanged();

        dcap = _dcap;
        enclaveMeasurements = _enclaveMeasurements;
        identityDigest = _identityDigest;
        merkleTreeAddress = _merkleTreeAddress;
        mailbox = _mailbox;
        state = _genesisState;
        maxQuoteSkew = _maxQuoteSkew;
    }

    /// Admit a different enclave from now on. The trusted state keeps its root, height and
    /// light client store; only the identity digest it carries is swapped, because the enclave
    /// copies that forward into every state it attests. So nothing in flight is lost.
    function setEnclave(bytes32 _enclaveMeasurements, bytes32 _identityDigest) external onlyOwner {
        emit EnclaveChanged(enclaveMeasurements, _enclaveMeasurements, identityDigest, _identityDigest);
        enclaveMeasurements = _enclaveMeasurements;
        identityDigest = _identityDigest;
        bytes memory current = state;
        assembly {
            mstore(add(add(current, 32), 84), _identityDigest)
        }
        state = current;
    }

    function moduleType() external pure returns (uint8) {
        return MODULE_TYPE;
    }

    /// Advance the trusted state and record the origin tree's root the quote covers.
    ///
    /// `payload` is the canonical attested update the enclave hashed into `report_data`:
    ///   prev_state(116) new_state(116) merkle_tree(32) attested_at(8) tree_root(32) tree_count(4)
    function submitAttestation(bytes calldata quote, bytes calldata payload) external payable {
        bytes memory out = _verifiedReport(quote);
        _requireOurEnclave(out);
        _requireCommitsTo(out, payload);
        _apply(payload);
    }

    /// Hyperlane calls this as it processes a message. `metadata` names an attested root and
    /// carries the message's merkle branch to it:
    ///   root(32) index(4) proof(32*32)
    /// Anything that does not fold the message's id to a root this ISM attested is refused.
    /// Nothing is consumed here: the mailbox itself refuses to process a message id twice.
    function verify(bytes calldata metadata, bytes calldata message) external view returns (bool) {
        if (msg.sender != mailbox) revert NotMailbox();
        if (metadata.length != METADATA_BYTES) return false;
        bytes32 root = bytes32(metadata[0:32]);
        if (!attestedRoots[root]) return false;
        uint32 index = uint32(bytes4(metadata[32:36]));
        bytes32[32] memory proof;
        for (uint256 i = 0; i < 32; i++) {
            proof[i] = bytes32(metadata[36 + i * 32:68 + i * 32]);
        }
        return MerkleLib.branchRoot(keccak256(message), proof, index) == root;
    }

    // ---------------------------------------------------------------- steps

    /// Intel's verdict on the hardware, by way of Automata.
    function _verifiedReport(bytes calldata quote) private returns (bytes memory out) {
        bool ok;
        (ok, out) = dcap.verifyAndAttestOnChain{value: msg.value}(quote);
        // On failure the output carries their four-letter reason, such as "TCBR" when the
        // platform's TCB record is missing from this chain's PCCS. Surfacing it beats a bare
        // revert, because the two causes need different fixes.
        if (!ok) revert QuoteRejected(out);
        if (out.length < MIN_OUTPUT) revert MalformedOutput();

        uint16 version = uint16(bytes2(_slice(out, 0, 2)));
        uint16 bodyType = uint16(bytes2(_slice(out, 2, 2)));
        // v4 carries a TD 1.0 body; v5 carries TD 1.0 or TD 1.5. Both are TDX, never SGX.
        bool v4 = version == QUOTE_VERSION_4 && bodyType == BODY_TYPE_TD10;
        bool v5 = version == QUOTE_VERSION_5 && (bodyType == BODY_TYPE_TD10 || bodyType == BODY_TYPE_TD15);
        if (!v4 && !v5) revert UnsupportedQuote(version, bodyType);

        uint8 tcbStatus = uint8(out[4]);
        if (tcbStatus != TCB_OK && tcbStatus != TCB_SW_HARDENING_NEEDED) {
            revert TcbNotAcceptable(tcbStatus);
        }
    }

    /// Our verdict on the software.
    function _requireOurEnclave(bytes memory out) private view {
        if (uint8(out[OFF_TD_ATTRIBUTES]) & TD_DEBUG != 0) revert DebugEnclave();

        bytes32 measured = keccak256(
            abi.encodePacked(_slice(out, OFF_MRTD, LEN_MRTD), _slice(out, OFF_RTMR0, LEN_RTMR))
        );
        if (measured != enclaveMeasurements) revert WrongEnclave(enclaveMeasurements, measured);
    }

    /// The quote must commit to exactly this payload.
    function _requireCommitsTo(bytes memory out, bytes calldata payload) private pure {
        bytes memory reportData = _slice(out, OFF_REPORT_DATA, 64);
        if (bytes32(_slice(reportData, 0, 32)) != sha256(payload)) revert PayloadNotAttested();
        // The enclave leaves the second half zero. Refusing anything else keeps the attested
        // payload the only thing report_data can carry.
        if (bytes32(_slice(reportData, 32, 32)) != bytes32(0)) revert ReportDataNotPadded();
    }

    /// Decode the attested update, check it against this ISM, and store it.
    function _apply(bytes calldata payload) private {
        // Fixed size, so one encoding maps to one update.
        if (payload.length != ATTESTED_UPDATE_BYTES) revert MalformedPayload();

        bytes calldata prev = payload[0:STATE_BYTES];
        bytes calldata next = payload[STATE_BYTES:2 * STATE_BYTES];

        // Replay protection is the state chain itself: an attestation names the state it
        // starts from, so once the state advances every earlier attestation is stranded.
        if (keccak256(prev) != keccak256(state)) revert TrustedStateMismatch();

        uint256 o = 2 * STATE_BYTES;
        if (bytes32(payload[o:o + 32]) != merkleTreeAddress) revert MerkleTreeMismatch();
        o += 32;
        uint64 attestedAt = uint64(bytes8(payload[o:o + 8]));
        o += 8;
        bytes32 root = bytes32(payload[o:o + 32]);
        o += 32;
        uint32 count = uint32(bytes4(payload[o:o + 4]));

        _requireLegalTransition(prev, next);
        _requireFresh(attestedAt, uint64(bytes8(next[44:52])));
        // A root that has not moved brought no leaves; that is the heartbeat, and it is
        // bounded. A root that has moved is never held back.
        if (root == treeRoot) {
            uint64 previousTime = uint64(bytes8(prev[44:52]));
            uint64 nextTime = uint64(bytes8(next[44:52]));
            if (nextTime < previousTime + EMPTY_BATCH_MIN_INTERVAL) {
                revert EmptyBatchTooSoon(previousTime, nextTime, EMPTY_BATCH_MIN_INTERVAL);
            }
        }

        state = next;
        treeRoot = root;
        treeCount = count;
        attestedRoots[root] = true;

        emit StateAdvanced(bytes32(next[0:32]), uint64(bytes8(next[36:44])), uint64(bytes8(next[44:52])), root, count);
    }

    /// The rules every attested transition must satisfy, identical to the Celestia module's.
    ///
    /// Checked here rather than taken on the enclave's word. The enclave checks them too, but
    /// an ISM that trusts its enclave to be correct as well as honest has no defence against
    /// an enclave bug.
    function _requireLegalTransition(bytes calldata prev, bytes calldata next) private view {
        if (bytes4(prev[32:36]) != bytes4(next[32:36])) revert OriginDomainChanged();
        if (uint64(bytes8(next[36:44])) <= uint64(bytes8(prev[36:44]))) revert HeightNotAdvanced();
        if (uint64(bytes8(next[44:52])) < uint64(bytes8(prev[44:52]))) revert TimestampWentBackwards();
        if (bytes32(next[0:32]) == bytes32(prev[0:32])) revert StateRootUnchanged();
        if (bytes32(next[84:116]) != identityDigest) revert IdentityChanged();
    }

    function _requireFresh(uint64 attestedAt, uint64 stateTimestamp) private view {
        // Bounded both ways. Without the lower bound a submitter could claim a time in the
        // past; without the upper one a quote could be replayed indefinitely.
        if (block.timestamp < attestedAt) {
            revert ClockBehindAttestation(uint64(block.timestamp), attestedAt);
        }
        if (block.timestamp > attestedAt + maxQuoteSkew) {
            revert AttestationTooOld(uint64(block.timestamp), attestedAt, maxQuoteSkew);
        }
        // The attested time must also be at least as new as the state it carries, or an L2
        // origin could pair a fresh L1 header with an arbitrarily old L2 root.
        if (attestedAt < stateTimestamp) revert AttestedBeforeState(attestedAt, stateTimestamp);
    }

    /// Expand the verifier's four-letter refusal into something actionable.
    ///
    /// Automata returns codes like "TCBR" and nothing else, which says what failed but not
    /// what to do about it. Most of these mean one specific thing for this deployment: the
    /// collateral in our PCCS has expired or was never published. Intel's TCB info, QE
    /// identity and PCK CRLs are valid for thirty days, so this is expected to fire roughly
    /// monthly until someone republishes them.
    ///
    /// `pure` and external: it costs nothing to call off chain, and keeping it out of the
    /// revert path keeps failures cheap.
    function describeQuoteError(bytes calldata code) external pure returns (string memory) {
        bytes32 c = keccak256(code);

        // Collateral problems. These are the ones an operator can fix by republishing.
        if (c == keccak256("TCBR")) {
            return "TCB status revoked or missing: this chain's PCCS has no TCB info for the enclave's FMSPC, or the platform is revoked. Publish the current TCB info for that FMSPC.";
        }
        if (c == keccak256("TCBCH")) {
            return "TCB content hash mismatch: the stored TCB info is stale. Republish the current TCB info for this FMSPC.";
        }
        if (c == keccak256("QEIDCH") || c == keccak256("QEIDVE")) {
            return "QE identity missing or stale: republish the current QE identity.";
        }
        if (c == keccak256("PCKCRLM")) {
            return "PCK CA CRL missing: publish the platform and processor PCK CRLs.";
        }
        if (c == keccak256("PCKCRLH")) {
            return "PCK CA CRL stale: republish the platform and processor PCK CRLs.";
        }
        if (c == keccak256("ROOTCRLH")) {
            return "Root CA CRL stale: republish the Intel root CA CRL (valid about a year).";
        }
        if (c == keccak256("ROOTH")) {
            return "Root CA certificate mismatch: the wrong Intel root CA is seeded in this PCCS.";
        }
        if (c == keccak256("SIGNH")) {
            return "TCB signing certificate mismatch: reseed the TCB signing certificate.";
        }

        // Quote problems. These mean the enclave or its quote is wrong, not the collateral.
        if (c == keccak256("TEE")) return "Unknown TEE type: not a TDX quote.";
        if (c == keccak256("QHV")) return "Quote version mismatch: this ISM expects TDX quote v4.";
        if (c == keccak256("QHS") || c == keccak256("QBS") || c == keccak256("ADS")) {
            return "Quote size mismatch: the submitted quote is truncated or malformed.";
        }
        if (c == keccak256("QBF") || c == keccak256("ADF")) return "Quote body or auth data failed to parse.";
        if (c == keccak256("TD10F")) return "Failed to parse the TD1.0 report body.";
        if (c == keccak256("TD15F")) return "Failed to parse the TD1.5 report body.";
        if (c == keccak256("TDMF")) return "TDX module check failed.";
        if (c == keccak256("TDRF")) return "TDX relaunch check failed: the TD should be restarted.";
        if (c == keccak256("QEF")) return "QE report failed to parse.";
        if (c == keccak256("QEVE")) return "QE report verification failed.";
        if (c == keccak256("X509VE")) return "X.509 certificate chain verification failed: the PCK chain in the quote is bad or revoked.";
        if (c == keccak256("ATTVE")) return "Quote attestation signature verification failed.";
        if (c == keccak256("QHATTF")) return "Quote attestation type not supported.";
        if (c == keccak256("QEVEN")) return "Quote enclave vendor id not supported.";
        if (c == keccak256("OUTS")) return "Invalid output size from the verifier.";

        return "Unrecognised verifier error code.";
    }

    // ---------------------------------------------------------------- helpers

    function _slice(bytes memory b, uint256 start, uint256 len) private pure returns (bytes memory out) {
        out = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            out[i] = b[start + i];
        }
    }

    function _readBytes32(bytes memory b, uint256 start) private pure returns (bytes32 v) {
        assembly {
            v := mload(add(add(b, 32), start))
        }
    }
}
