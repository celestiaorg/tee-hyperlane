# INTERACT

Using a running bridge: sending transfers, watching them land, and checking by hand what the
UI is telling you.

[DEPLOY.md](DEPLOY.md) stands one up. [MAINTAIN.md](MAINTAIN.md) keeps it alive.

---

## What is exposed

One host serves everything that is not an enclave, including the chain. Only 3000, 3001 and
3002 are reachable; the chain's own ports are proxied rather than exposed.

```
:3000  bridge UI, and the only public way to reach the chain
       /rpc                      Celestia RPC        (26657)
       /rest                     Celestia REST       (1317)
       /api                      attestation API
       /evm/{sepolia,arbitrum,base}/   EVM JSON-RPC
       /tx/<hash>                transaction view
:3001  relayer dashboard and JSON API
:3002  gas oracle dashboard and JSON API
```

Every proxied path answers on both spellings, with and without the trailing slash.

---

## The UI

Open `http://<host>:3000`. It works with both wallets at once, which you need, because every
route has a Celestia side and an EVM side.

### Keplr

Keplr cannot add a custom chain from its own settings. **Open the UI and connect**: it calls
`experimentalSuggestChain` with the values from `.env.local`, which is what registers
`teeism-local` in the wallet. Nothing to configure by hand.

Import the mnemonic from `CELESTIA_MNEMONIC` in `devnet/.env`. **Account 0 is the funded one.** Genesis
accounts derive from that fixed mnemonic, so the funded address survives a rebuild of the
chain.

Keplr reaches the chain through the gateway, not directly, because the RPC port is not
exposed.

### MetaMask

Add the three testnets normally, or let the UI prompt. It points them at
`http://<host>:3000/evm/<chain>/` so that everything crosses one origin.

### Sending

Pick an origin, a destination, an asset and an amount. The UI quotes the gas the paymaster
will charge and shows the route's expected time.

> Sending **USDC from Sepolia** needs an ERC20 `approve` to the collateral router first; the
> UI does this as a separate transaction. Sending a synthetic needs no approval, because it is
> burned rather than transferred.

### The four steps the UI shows

| step | means |
|---|---|
| Dispatched | the origin mailbox accepted it and put its id in the merkle tree |
| Attested by enclave | a TDX enclave verified the origin state containing it |
| Authorised by ISM | the destination ISM accepted the quote and allowed this id |
| Delivered | the destination mailbox processed it |

Delivery is not inferred from the relayer. **Check** queries the destination mailbox directly,
so the UI's answer is the chain's answer.

Expanding a transfer shows the attestation for the batch it landed in: the origin block, the
attested state root, the enclave's image and OS measurements, and the TDX quote itself. That
is the point of the bridge, and it is the only thing the UI goes out of its way to display.

If a control says an asset is not available on a route, that route's `VITE_*_USDC_ROUTER` is
empty in `.env.local`. That is the deliberate signal for "not deployed here", not a bug.

---

## Getting test funds

The **Faucet** tab grants **1000 TIA, once per address**. Connect Keplr and press claim; it
sends to the connected address and lands in the next block.

```sh
curl -s $B/api/faucet                       # {"enabled":true,"amountTia":1000}
curl -s $B/api/faucet/<celestia1...>        # {"claimed":false}
curl -s -X POST $B/api/faucet -H 'content-type: application/json' \
     -d '{"address":"<celestia1...>"}'      # {"tx_hash":"...","amount_tia":1000}
```

A second claim for the same address returns 409, and the tab says so before offering the
button. The claim is recorded on the host, not in the browser, so clearing site data or
switching browser does not grant a second one.

If the tab reports the faucet is not configured, the API has no keyring to sign with. See
DEPLOY.md step 11b.

---

## What each route should take

Measured end to end on the live deployment, not estimated. There is no proving anywhere, so
the only wait is origin finality plus one transaction.

```
Celestia -> Arbitrum    11-18 s
Celestia -> Base        13-18 s
Celestia -> Sepolia     14-29 s     the spread is one Sepolia block
Celestia -> Eden        ~30 s
Sepolia  -> Celestia    ~15 min     Ethereum finality, two epochs
Eden     -> Celestia    1-2 min     Eden's DA posting interval
Arbitrum -> Celestia    ~1h 40m     see below
Base     -> Celestia    ~5 days     the dispute window
```

Arbitrum's figure is not its challenge period, which is 20 L1 blocks or about four minutes.
A new confirmed root lands every ~31 minutes, each covering ~7,500 L2 blocks, and the newest
one is already ~1h40m behind Eden's head because a validator asserts over data it already
treats as settled on L1. So a message waits the standing lag plus up to one cycle.

Eden is fast because there is nothing to wait out: its sequencer posts signed headers to
Celestia about once a minute, and once a header is in a Celestia block the light client has
verified, the route can attest it.

**None of the slow ones is our latency.** Celestia to anywhere is fast because Celestia
finalises in a block. The reverse waits on the origin proving itself, and for the two
optimistic rollups that means a challenge or dispute window. Base is the extreme case and it
is entirely Base's: five days plus about three minutes, which is when the dispute game
covering your dispatch block resolves.

A Base transfer sitting for days is **normal**. See MAINTAIN.md for how to confirm it is
waiting rather than stuck.

---

## Sending from the command line

```sh
tee-hyperlane send --route celestia-to-sepolia --token TIA --amount 1000000 --to 0x…
tee-hyperlane verify --message-id 0x…
tee-hyperlane status
```

`verify` is the authoritative answer to "did it arrive": it reports whether the id was
consumed from the ISM. A consumed id cannot be replayed.

---

## Watching a transfer

The relayer dashboard at `:3001` shows every route's stage. The JSON behind it:

```sh
curl -s http://<host>:3001/api/status | jq -r '.[] | "\(.name)  \(.height)  \(.stage)"'
```

Stages are `attesting` and `submitting`. A route with neither is idle, which is the normal
state between batches.

Follow one transfer in the log:

```sh
journalctl -u teeism-relayer -f | grep -E 'attesting|submitting|delivered|scan failed'
```

A healthy cycle for one route reads:

```
commands::ethereum_l2: attesting l2 rollup="base" height=46890863 trusted=46889663 leaves=2
tasks: submitting route=base-to-celestia destination=1297040299
tasks: batch delivered route=base-to-celestia
```

> `leaves=N` is how many leaves are in **this batch**, not how many are in the tree. On the
> shared canonical mailboxes most of them belong to other people. A batch delivering with none
> of your messages in it is normal.

---

## Checking by hand

**Did a message arrive on Celestia?**

```sh
celestia-appd query bank balances <addr> --node http://localhost:26657
```

A synthetic shows as the denom `hyperlane/<token id>`.

Or look for the delivery event directly:

```sh
curl -s "http://localhost:26657/tx_search?query=%22message.action%3D%27%2Fhyperlane.core.v1.MsgProcessMessage%27%22&per_page=20&order_by=%22desc%22" \
  | jq -r '.result.txs[].tx_result.events[]
           | select(.type=="hyperlane.warp.v1.EventReceiveRemoteTransfer")
           | .attributes'
```

**What does an ISM currently trust?**

```sh
celestia-appd query teeism ism <ism-id> --node http://localhost:26657 -o json
cast call <TeeDcapIsm> "state()(bytes)" --rpc-url <rpc>
```

The 116-byte state decodes as:

```
[  0: 32] state_root
[ 32: 36] origin_domain     u32 BE
[ 36: 44] height            u64 BE
[ 44: 52] timestamp         u64 BE
[ 52: 84] lc_store_commit
[ 84:116] identity_digest
```

`height` and `timestamp` are the useful pair: they say how far the ISM has followed the origin
and as of when. A timestamp hours behind on a Celestia-origin route means something is wrong.
The same on a Base-origin route means the dispute window, and is expected.

**Did an EVM delivery happen?**

```sh
cast call <synthetic router> "balanceOf(address)(uint256)" <addr> --rpc-url <rpc>
```

**What is a transfer being charged?** The gas oracle dashboard is at `:3002`, and

```sh
gas-oracle --config .state/gas-oracle.toml --once
```

prints one round without writing a service restart's worth of state.

---

## What it costs

Per transfer, at 1.1 gwei on Sepolia, 0.006 on Base and 0.25 on Arbitrum:

```
                              gas        sepolia     base        arbitrum
attestation                5,331,900    0.005900    0.000031    0.001330
message delivery             124,000    0.000140    0.000001    0.000030
end to end                              0.006011    0.000032    0.001366
```

On Celestia an attestation is 343,592 gas and a delivery 117,337, carrying 7,785 bytes of
quote, event log and Intel collateral.

An attestation covers a whole **batch**, so these are per batch and not per message. The
enclaves themselves are cheap because they prove nothing: two `tdx.small` instances at
$0.0608/hr, $2.92/day for both.

Fees are charged to the sender and accrue to the paymaster's beneficiary: on Celestia in
`utia` to the IGP owner, on the EVM chains to the relayer key.

---

## If something looks wrong

Work down this list before assuming a fault.

1. **Is the route simply waiting?** Check the expected times above. Base is days by design.
2. **Is the batch yours?** A delivered batch with no balance change means its leaves belonged
   to other users of a shared mailbox.
3. **Does the ISM's `timestamp` advance?** If yes, the route is healthy and your message is
   queued behind finality. If no, something is stuck.
4. **Ask the chain why it refused.** On EVM,
   `cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)` expands the
   four-letter code. Codes naming collateral mean the monthly job is overdue.
5. **Check the relayer log** for `scan failed`, which names the endpoint that refused.

MAINTAIN.md has the full diagnosis tree, including the cases that need intervention.

---

## Running the UI locally

```sh
cd bridge-app
npm install
npm run dev     # http://localhost:5173
npm run build   # -> dist/, which the gateway serves in production
```

`src/config.ts` reads every address from the `VITE_*` environment, defaulting `VITE_RELAYER_API`
to `/api`, which is what the gateway proxies so that no CORS is involved. A route whose warp
router is unset is hidden rather than offered as something that will fail.
