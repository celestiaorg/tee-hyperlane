# The non-ZK deployment, end to end

One machine runs the Celestia chain, the relayer, the attestation API, the gas oracle and the
UI. Two Phala CVMs hold the enclaves. Nothing on this machine is trusted by any ISM.

This replaces the Groth16 deployment described in `README.md`. The difference that matters
here: there is no prover, so a transfer costs one attestation and one transaction instead of
two proofs and two transactions.

## What runs

| unit | port | what it does |
|---|---|---|
| `teeism-celestia` (docker) | 26657, 1317, 9090 | the chain itself |
| `teeism-gateway` (docker) | 3000 | UI, and the only public way to reach the chain |
| `teeism-relayer.service` | | scans all six routes, attests, submits |
| `teeism-api.service` | 3001 | attestation lookup for the UI, and its dashboard |
| `teeism-gas-oracle.service` | 3002 | keeps the IGP and the StorageGasOracles current |

Only 80 and 3000 are forwarded from the outside on this host, and 80 is held by an unrelated
service. That is why the chain is reached through the gateway rather than on 26657 directly,
and it is what makes Keplr work against a bare IP.

## Bringing it up

```sh
devnet/scripts/10-celestia-up.sh      # chain, from a fixed mnemonic
devnet/scripts/20-celestia-hyperlane.sh
devnet/scripts/30-enclave-up.sh       # x2, one per origin family
devnet/scripts/40-create-ism.sh       # sepolia-origin ism on celestia
devnet/scripts/50-warp-celestia.sh
devnet/scripts/80-evm-isms.sh         # one TeeDcapIsm per evm chain
devnet/scripts/90-evm-warp.sh         # routers, both directions
```

Then the pieces `make init` does not cover, below.

## Warp routers

A router is reused across deployments; only its ISM and its enrollments change. For each EVM
chain:

```sh
cast send $ROUTER "setInterchainSecurityModule(address)" $NEW_ISM
cast send $ROUTER "enrollRemoteRouter(uint32,bytes32)" 1297040299 $CELESTIA_TOKEN_ID
```

and on Celestia, once per EVM domain:

```sh
celestia-appd tx warp enroll-remote-router $TOKEN $DOMAIN $ROUTER_32BYTE 50000
```

The router address must be the 32-byte form. The CLI rejects a bare 20-byte address rather
than padding it.

`90-evm-warp.sh` does all of this, and will repoint an enrollment that already exists rather
than failing, because the chain refuses to enroll a domain twice.

### One ISM is not enough on the Celestia side

Three EVM origins deliver into one Celestia token, and each TeeDcapIsm pins exactly one
`origin_domain`. A single ISM therefore rejects two of the three. A routing ISM fans them out:

```sh
celestia-appd tx hyperlane ism create-routing
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 11155111 $SEPOLIA_ORIGIN_ISM
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 421614   $ARBITRUM_ORIGIN_ISM
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 84532    $BASE_ORIGIN_ISM
celestia-appd tx warp set-token $TOKEN --ism-id $ROUTING
celestia-appd tx hyperlane mailbox set $MAILBOX --default-ism $ROUTING
```

The Celestia-to-EVM direction hides this, because there each destination has its own ISM.

## The paymaster

Without it the UI cannot quote a fee and every transfer is free, which is not what the
testnet is meant to model.

```sh
celestia-appd tx hyperlane hooks igp create utia
celestia-appd tx hyperlane hooks igp set-owner $IGP --new-owner $ORACLE_KEY
celestia-appd tx hyperlane mailbox set $MAILBOX --required-hook $MERKLE --default-hook $IGP
```

`required-hook` is the merkle tree hook, so every dispatch inserts a leaf. `default-hook` is
the IGP, so every dispatch pays. Getting these the wrong way round means either no leaves or
no fees, and neither fails loudly.

The oracle signs Celestia with a key named `bridge`, which is not one of the three genesis
accounts. Derive it from the same mnemonic and fund it:

```sh
celestia-appd keys add bridge --recover --account 3
celestia-appd tx bank send user $BRIDGE 5000000000utia
```

The IGP must then be owned by that key, or every push fails with "is not the owner".

### The EVM side of the same thing

Each EVM chain has its own IGP and StorageGasOracle, deployed once and reused. The oracle
service keeps their prices current, but the IGP's *routing* is manual and has to name this
deployment's Celestia domain:

```sh
cast send $IGP "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(1297040299,($STORAGE_GAS_ORACLE,800000))]"
```

Without it `transferRemote` reverts with `IGP: no gas oracle for domain 1297040299`, which
reads like a broken oracle and is actually an unconfigured IGP. The addresses are in
`deploy/DEPLOYMENT.md`; they cannot be read off the routers, because the routers' hooks are
aggregation hooks that do not expose their members.

## Verifying the ISMs

Sourcify, which needs no API key and recovers the constructor arguments itself:

```sh
forge verify-contract $ISM src/TeeDcapIsm.sol:TeeDcapIsm --verifier sourcify --chain $CHAIN_ID
```

All three are `exact_match` on both creation and runtime bytecode. Etherscan needs an API key
and is rate limited through Sourcify's relay.
