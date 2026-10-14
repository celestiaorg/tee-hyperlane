# INTERACT

`B` is the gateway, e.g. `B=http://<host>:3000`. `celestia-appd` commands run on the host.

## Endpoints

```
:3000  the UI, and the only public way to reach the chain
       /rpc  /rest            Celestia RPC and REST
       /api                   relayer API
       /evm/<chain>/          EVM JSON-RPC for sepolia, arbitrum, base, eden
       /tx/<hash>             transaction view
:3001  relayer dashboard       :3002  gas oracle dashboard
```

## Wallets

- **Keplr:** connect in the UI, import `CELESTIA_MNEMONIC` from `devnet/.env`. Account 0 is funded.
- **MetaMask:** let the UI add the networks.

## Test funds

**Faucet** tab: 1000 TIA, once per address. CLI:

```sh
curl -s $B/api/faucet                                   # {"enabled":true,"amountTia":1000}
curl -s -X POST $B/api/faucet -H 'content-type: application/json' \
  -d '{"address":"celestia1..."}'                       # {"tx_hash":"...","amount_tia":1000}
```

## Sending

**UI:** pick origin, destination, asset, amount. No `approve` needed.

**From Celestia:**

```sh
celestia-appd tx warp transfer <celestia token id> <destination domain> \
  0x000000000000000000000000<evm address without 0x> <amount> \
  --from <key> --chain-id teeism-local --node tcp://localhost:26657 \
  --gas auto --gas-adjustment 1.5 --gas-prices 0.002utia --max-hyperlane-fee 10000000utia -y
```

Don't re-run if `jq` fails: it's already broadcast. Read the `txhash` line.

**From an EVM chain:**

```sh
cast send <router> "transferRemote(uint32,bytes32,uint256)(bytes32)" \
  1297040299 0x000000000000000000000000<celestia address as 20 hex bytes> <amount> \
  --value $(cast call <router> "quoteGasPayment(uint32)(uint256)" 1297040299 --rpc-url <rpc>) \
  --rpc-url <rpc> --private-key $EVM_PRIVATE_KEY
```

Domains: Celestia `1297040299`, Sepolia `11155111`, Arbitrum `421614`, Base `84532`, Eden
`3735928814`. Addresses: [README.md](../README.md#deployments).

## How long it takes

```
Celestia -> Arbitrum, Base, Sepolia   under 30 s
Celestia -> Eden                      ~30 s
Eden     -> Celestia                  1-4 min     waiting for Eden to post its headers to mocha
Sepolia  -> Celestia                  ~15 min     Ethereum finality
Arbitrum -> Celestia                  under 1 min the next block its sequencer signs
Base     -> Celestia                  under 1 min the next block its sequencer signs
```

Slower than this: [MAINTAIN](MAINTAIN.md#waiting-or-stuck).

## Did it arrive

```sh
cast call <mailbox> "delivered(bytes32)(bool)" <message id> --rpc-url <rpc>
celestia-appd query hyperlane delivered <mailbox id> <message id> --node tcp://localhost:26657
```

- Query the destination mailbox. On Celestia, only for messages *into* Celestia (outbound ids read `true`).
- Or check the balance: EVM `cast call <router> "balanceOf(address)(uint256)" <you> --rpc-url <rpc>`; Celestia denom `hyperlane/<token id>`.

## Watching it

```sh
teeism status                                              # every route's ISM height and state age
curl -s $B/api/v1/routes | jq '.[] | {name, status, ism: .ismReading.height, head: .originHead.height}'
journalctl -u teeism-relayer -f | grep -E 'attesting|delivering|delivered|route failed'
```

Healthy:

```
route: attesting route=base-to-celestia from=46889663 to=46890863 leaves=2
destination: delivering id=...
route: batch delivered route=base-to-celestia height=46890863
```

## What an ISM trusts

```sh
cast call <TeeDcapIsm> "state()(bytes)" --rpc-url <rpc>
celestia-appd query teeism ism <ism id> --node tcp://localhost:26657 -o json
```

```
[  0: 32] state_root     [ 32: 36] origin_domain   [ 36: 44] height
[ 44: 52] timestamp      [ 52: 84] lc_store_commit [ 84:116] identity_digest
```

## Cost

At 1.1 gwei on Sepolia, 0.006 on Base, 0.25 on Arbitrum:

```
                      gas         sepolia    base       arbitrum    (ETH)
attestation           5,331,900   0.005900   0.000031   0.001330
delivery, per message   124,000   0.000140   0.000001   0.000030
```

Celestia: attestation 343,592 gas, delivery 117,337. Enclaves: $4.38/day. A quiet route also attests once an hour with no messages ([MAINTAIN](MAINTAIN.md#heartbeat)): one attestation's gas per hour. Current quotes: `:3002`, or one round:
`~/tee-ism-nonzk/tee-hyperlane/target/release/gas-oracle --config ~/tee-ism-nonzk/devnet/.state/gas-oracle.toml --once`.

## Running the UI locally

```sh
cd bridge-app && npm install && npm run dev     # http://localhost:5173
```

Reads `VITE_*` from `.env.local`, which `teeism ui` writes ([DEPLOY step 13](DEPLOY.md#13-ui-and-gateway)).
