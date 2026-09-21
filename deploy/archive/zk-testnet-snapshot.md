# The ZK testnet as it stood before the non-ZK replacement

Recorded 2026-09-16 16:28 UTC from ark (178.199.12.26) and Phala, immediately
before deleting it. Kept so the Groth16 deployment can be reconstructed if the non-ZK
one has to be abandoned.

## Phala CVMs
```
f6231f5329c6c70a8236045504ab28a9b7e782bb  tee-cel-v3   celestia-origin enclave
16882cc467b8d243f0a98ba686d214b910902e02  tee-eth-v3   ethereum and L2 origin enclave
```

## coprocessor.toml
```toml
# Live routes. Celestia is the hub: every route has it on one side.
# Sized by how fast a message should be noticed, now that a scan is one indexed
# query rather than a read of every transaction on the origin.
tick_secs = 6

# One block is the floor: the app hash for H lives in the header at H+1. The rest is
# margin against an RPC serving a head it cannot prove yet, not a reorg allowance -
# Tendermint finality is absolute at one block.
celestia_lag = 2
proof_dir = "/var/lib/tee-hyperlane/proofs"

[[routes]]
name = "celestia-to-sepolia"
routers = [
  "0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE",
  "0xfb611B6f6CE92033960e99C2D65cee4237e64cDD",
]
tee_node_url = "https://f6231f5329c6c70a8236045504ab28a9b7e782bb-8080.dstack-pha-prod9.phala.network"
ism_id = "0x552c240a658f663EdeB5138346a406055eA0e55b"
merkle_tree_address = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.origin]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.destination]
kind = "ethereum"
domain = 11155111
execution_rpc = "https://ethereum-sepolia-rpc.publicnode.com"
mailbox = "0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

[[routes]]
name = "celestia-to-arbitrum"
routers = [
  "0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE",
  "0xb9E5E3eb926EA22B951d2fb7392F9F3D6c704054",
]
tee_node_url = "https://f6231f5329c6c70a8236045504ab28a9b7e782bb-8080.dstack-pha-prod9.phala.network"
ism_id = "0xf87b6f53058824a1Ec30C2c3c5961184e947043D"
merkle_tree_address = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.origin]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.destination]
kind = "ethereum"
domain = 421614
execution_rpc = "https://arbitrum-sepolia-rpc.publicnode.com"
mailbox = "0x598facE78a4302f11E3de0bee1894Da0b2Cb71F8"

[[routes]]
name = "celestia-to-base"
routers = [
  "0xf4197C55C944987E9b10e09C0A47915211769B78",
  "0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B",
]
tee_node_url = "https://f6231f5329c6c70a8236045504ab28a9b7e782bb-8080.dstack-pha-prod9.phala.network"
ism_id = "0x13A28e9E6077cA8ebe0c097864c830f1159905cd"
merkle_tree_address = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.origin]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

[routes.destination]
kind = "ethereum"
domain = 84532
execution_rpc = "https://sepolia.base.org"
mailbox = "0x6966b0E55883d49BFB24539356a2f8A673E02039"

[[routes]]
name = "sepolia-to-celestia"
routers = [
  "0x726f757465725f61707000000000000000000000000000010000000000000000",
  "0x726f757465725f61707000000000000000000000000000020000000000000001",
]
tee_node_url = "https://16882cc467b8d243f0a98ba686d214b910902e02-8080.dstack-pha-prod9.phala.network"
ism_id = "0x726f757465725f69736d000000000000000000000000002a0000000000000011"
merkle_tree_address = "0x0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d"

[routes.origin]
kind = "ethereum"
domain = 11155111
# Reaching back for the tree snapshot at the ISM's trusted height usually exceeds a public
# node's 128-block proof window. This endpoint serves it.
execution_rpc = "https://rpc.sepolia.ethpandaops.io"
beacon_rpc = "https://ethereum-sepolia-beacon-api.publicnode.com"
mailbox = "0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"
merkle_tree_hook = "0x4917a9746A7B6E0A57159cCb7F5a6744247f2d0d"
merkle_tree_base_slot = 103

[routes.destination]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

# Arbitrum back to Celestia. The enclave is Ethereum's: an L2 root is derived from the L1
# state root that light client already verifies, so this needs no third enclave.
#
# Latency is the rollup's, not ours - a BoLD assertion confirms about every 31 minutes, and
# only a confirmed one is trustless.
[[routes]]
name = "arbitrum-to-celestia"
routers = [
  "0x726f757465725f61707000000000000000000000000000010000000000000000",
  "0x726f757465725f61707000000000000000000000000000020000000000000001",
]
# An L2-origin ISM carries the L2 head's timestamp, which says nothing about which L1
# checkpoint its store was built from, so the search has nothing to derive it from.
checkpoint = "0x4d284c793db53912261a9b4c6a8590ffc99a24d50fb19afbd64673a7b507eb31"
tee_node_url = "https://16882cc467b8d243f0a98ba686d214b910902e02-8080.dstack-pha-prod9.phala.network"
ism_id = "0x726f757465725f69736d000000000000000000000000002a0000000000000012"
merkle_tree_address = "0x000000000000000000000000ad34a66bf6db18e858f6b686557075568c6e031c"

[routes.origin]
kind = "ethereum_l2"
domain = 421614
l2_rpc = "https://arb-sepolia.g.alchemy.com/v2/ALCHEMY_KEY"
# Alchemy's free tier caps eth_getLogs at ten blocks and a confirmed L2 head moves
# thousands at a time; publicnode serves the whole span. Both are untrusted.
logs_rpc = "https://arbitrum-sepolia-rpc.publicnode.com"
rollup = "arbitrum"
l1_anchor_contract = "0x042B2E6C5E99d4c521bd49beeD5E99651D9B0Cf4"
mailbox = "0x598facE78a4302f11E3de0bee1894Da0b2Cb71F8"
merkle_tree_hook = "0xAD34A66Bf6dB18E858F6B686557075568c6E031C"
merkle_tree_base_slot = 151

[routes.origin.l1]
kind = "ethereum"
domain = 11155111
execution_rpc = "https://rpc.sepolia.ethpandaops.io"
beacon_rpc = "https://ethereum-sepolia-beacon-api.publicnode.com"
mailbox = "0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

[routes.destination]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"

# Base back to Celestia. Same shape as Arbitrum, but Base's dispute window is five days, so a
# transfer out of Base is a multi-day trip. That is the rollup's parameter, not ours.
[[routes]]
name = "base-to-celestia"
routers = [
  "0x726f757465725f61707000000000000000000000000000010000000000000000",
  "0x726f757465725f61707000000000000000000000000000020000000000000001",
]
# An L2-origin ISM carries the L2 head's timestamp, which says nothing about which L1
# checkpoint its store was built from, so the search has nothing to derive it from.
checkpoint = "0x4d284c793db53912261a9b4c6a8590ffc99a24d50fb19afbd64673a7b507eb31"
tee_node_url = "https://16882cc467b8d243f0a98ba686d214b910902e02-8080.dstack-pha-prod9.phala.network"
ism_id = "0x726f757465725f69736d000000000000000000000000002a0000000000000013"
merkle_tree_address = "0x00000000000000000000000086fb9f1c124fb20ff130c41a79a432f770f67afd"

[routes.origin]
kind = "ethereum_l2"
domain = 84532
rollup = "base"
l2_rpc = "https://base-sepolia.g.alchemy.com/v2/ALCHEMY_KEY"
# Alchemy's free tier caps eth_getLogs at ten blocks and a confirmed L2 head moves
# thousands at a time; publicnode serves the whole span. Both are untrusted.
logs_rpc = "https://sepolia.base.org"
l1_anchor_contract = "0x2fF5cC82dBf333Ea30D8ee462178ab1707315355"
mailbox = "0x6966b0E55883d49BFB24539356a2f8A673E02039"
merkle_tree_hook = "0x86fb9F1c124fB20ff130C41a79a432F770f67AFD"
merkle_tree_base_slot = 151

[routes.origin.l1]
kind = "ethereum"
domain = 11155111
execution_rpc = "https://rpc.sepolia.ethpandaops.io"
beacon_rpc = "https://ethereum-sepolia-beacon-api.publicnode.com"
mailbox = "0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

[routes.destination]
kind = "celestia"
domain = 1297040200
rpc = "https://rpc-mocha.pops.one"
grpc = "https://grpc-mocha.pops.one"
mailbox_id = "0x68797065726c616e650000000000000000000000000000000000000000000000"
merkle_tree_hook_id = "0x726f757465725f706f73745f6469737061746368000000030000000000000000"
```

## systemd units
```
########## tee-hyperlane.service
# /etc/systemd/system/tee-hyperlane.service
# The coprocessor: attests, proves, relays. Runs on one machine alongside the bridge UI.
#
# Proving is CPU-bound and local by policy, so this wants real cores, not a burstable VM.
[Unit]
Description=TEE Hyperlane coprocessor
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bridge
WorkingDirectory=/opt/tee-hyperlane
Environment=TEE_HYPERLANE_DEPLOY_DIR=/opt/tee-hyperlane/deploy
Environment=TEE_HYPERLANE_ELF_DIR=/opt/tee-hyperlane/elf
Environment=CELHOME=/var/lib/tee-hyperlane/celhome
Environment=SP1_PROVER=cpu
# SHARD_SIZE and SHARD_BATCH_SIZE are deliberately not set. SP1 already picks them from the
# machine's total memory - 2^19 with batch 1 below 33 GB - and overriding them upward is how
# a small box ends up in swap. They only affect core proving anyway; the recursion prover
# hardcodes 2^22, which is what actually sets the peak.
# ProtectHome hides /home, and SP1 caches its groth16 artifacts under $HOME.
Environment=HOME=/var/lib/tee-hyperlane
ExecStart=/opt/tee-hyperlane/bin/tee-hyperlane --config /opt/tee-hyperlane/coprocessor.toml run
Restart=always
RestartSec=30

# The coprocessor is untrusted by design; give it nothing it does not need.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/tee-hyperlane

[Install]
WantedBy=multi-user.target

# /etc/systemd/system/tee-hyperlane.service.d/memory.conf
# Proving is one long-lived process running many proofs back to back, and glibc's default
# per-thread arenas (up to 8 x ncores) never hand the freed traces back. The first proof
# after a restart fit in RAM at 38 minutes; the third was 16.6 GB anonymous on a 13.8 GB box
# and had spent three hours paging. These bound the retention rather than the workload - no
# SP1 parameter changes, so proofs are identical, only the memory behaviour differs.
[Service]
Environment=MALLOC_ARENA_MAX=2
Environment=MALLOC_TRIM_THRESHOLD_=134217728
Environment=MALLOC_MMAP_THRESHOLD_=134217728
Environment=MALLOC_TOP_PAD_=134217728
########## tee-hyperlane-api.service
# /etc/systemd/system/tee-hyperlane-api.service
# Serves route status and attestations. Read-only over the proof store the coprocessor writes.
[Unit]
Description=TEE Hyperlane attestation API
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bridge
WorkingDirectory=/opt/tee-hyperlane
ExecStart=/opt/tee-hyperlane/bin/tee-hyperlane \
  --config /opt/tee-hyperlane/coprocessor.toml \
  serve --listen 0.0.0.0:3001 --proof-dir /var/lib/tee-hyperlane/proofs
Restart=always
RestartSec=10

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/tee-hyperlane

[Install]
WantedBy=multi-user.target
########## bridge-ui.service
# /etc/systemd/system/bridge-ui.service
# The bridge UI, with the attestation API and Celestia's REST proxied onto one origin.
#
# Serving this in-process rather than behind nginx keeps a deployment to three binaries and
# avoids fighting whatever else the host already serves on :80.
[Unit]
Description=TEE Bridge UI
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bridge
ExecStart=/opt/tee-hyperlane/bin/tee-hyperlane serve-ui \
  --listen 0.0.0.0:3000 \
  --dir /opt/bridge-app \
  --api http://127.0.0.1:3001 \
  --celestia-rest https://api-mocha.pops.one
Restart=always
RestartSec=10

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
########## gas-oracle.service
# /etc/systemd/system/gas-oracle.service
# Keeps the Celestia IGP's destination gas configs current, and serves a page showing what it
# pushed. Independent of the relayer: if this stops, quotes go stale but nothing breaks.
[Unit]
Description=Hyperlane gas oracle
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bridge
WorkingDirectory=/opt/tee-hyperlane
Environment=CELHOME=/var/lib/tee-hyperlane/celhome
ExecStart=/opt/tee-hyperlane/bin/gas-oracle \
  --config /opt/tee-hyperlane/gas-oracle.toml \
  --listen 0.0.0.0:3002
Restart=always
RestartSec=60

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/tee-hyperlane

[Install]
WantedBy=multi-user.target
```
