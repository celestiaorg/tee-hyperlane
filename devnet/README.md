# Local TEE ISM devnet

A Celestia chain, a Hyperlane deployment on it, a TEE ISM, and a relayer, all on this machine.
The one exception is the enclave: a TDX quote has to come from real Intel hardware, so that
piece runs on Phala Cloud and is billed by the hour, which is why `make stop` deletes it.

```
make init    build what is missing, start the chain, deploy the enclave, create the ISM
make start   run the relayer and the bridge UI on localhost:3000
make stop    delete the enclave, stop the chain, prune state
make status  show what is deployed and where
```

## What the ISM checks

In order, none skippable:

1. **Intel's verdict on the hardware.** The quote's signature chain, the PCK certificate
   chain, the CRLs and the TCB status, verified with [`google/go-tdx-guest`][gtg] against
   collateral carried in the transaction, at *block time*.
2. **The TD is not in debug mode.** A debug TD is transparent to its host, so its
   measurements prove nothing about what ran.
3. **Our verdict on the software.** The dstack event log must replay to the RTMRs the
   hardware signed, and `mr_td`, `compose-hash`, `os-image-hash`, `mr-kms` and `key-provider`
   must match what the ISM pins.
4. **The payload.** `report_data` must commit to it, the transition must be legal, and the
   clock must be anchored to attested chain time.

Collateral travels in the transaction because consensus cannot make network calls: every
validator has to reach the same verdict from the same bytes. That costs nothing in security,
since each artifact is signed by Intel and those signatures are checked during verification
exactly as they would be had they arrived over the wire. Block time is the clock for the same
reason, and it is what makes certificate and CRL validity windows deterministic.

`google/go-tdx-guest` rather than Phala's `dcap-qvl` Go bindings because the chain's
Dockerfile builds with `CGO_ENABLED=0`, and CGO in a consensus path is a liability besides.

## Layout

```
celestia/        the chain: compose file and a genesis-on-first-run entrypoint
enclave/         the measured compose file the enclave's identity is derived from
scripts/         one numbered script per init step, plus start, stop and status
.state/          chain data, keyring, built binaries, generated config (gitignored)
.state/out/      every deployed id, one file each, read back by later steps
```

`.state/out` is the only thing later steps and the relayer read. Nothing parses transaction
logs twice.

## Notes

- The chain gets a fresh genesis on every `make init`, because `make stop` prunes its data.
  Ids are minted at genesis, so they change between runs; `make start` regenerates both the
  relayer config and the UI's `.env.local` from what `make init` actually deployed.
- The devnet's enclave compose file is deliberately not byte-identical to the testnet's. Its
  hash is measured into RTMR3, so a devnet enclave can never satisfy a testnet ISM's identity,
  or the other way round.
- The devnet chain's Hyperlane domain is not Mocha's, so a devnet router and a testnet router
  can never be confused for one another.
- The keyring in `.state` holds throwaway devnet keys. It is gitignored anyway.
- An Ethereum origin needs an archive endpoint. The block the enclave attests is the finalized
  head, and every free Sepolia endpoint measured serves `eth_getProof` for the chain head
  alone, so both the historical read and the attested one fall outside its window. Set
  `SEPOLIA_ARCHIVE`, or `ALCHEMY_API_KEY`, or write the key to `.state/alchemy-key`.

[gtg]: https://pkg.go.dev/github.com/google/go-tdx-guest
