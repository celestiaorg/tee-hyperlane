/// Which ISM will authorise this transfer when it arrives, read from the destination chain.
///
/// Read live rather than from config on purpose. The address in `.env.local` is whatever was
/// true when the bundle was built, and the relayer's own view comes from a file it loaded at
/// startup: on this deployment those two disagreed with the chain for nineteen hours after a
/// redeploy and nothing surfaced it. The only answer worth showing before you sign is the one
/// the destination mailbox will actually consult, so that is the one this asks for.

import { CHAINS, routerFor, type ChainId, type TokenId, type CosmosChain, type EvmChain } from "./config";

export interface WiredIsm {
  /** The ISM that verifies the message. An address on EVM, a 32 byte id on Celestia. */
  id: string;
  /** Where to review it. Verified source on EVM, the module's own record on Celestia. */
  url: string | null;
  /** How it was reached, when that is not direct. */
  via: string | null;
}

const ZERO_ADDRESS = "0x0000000000000000000000000000000000000000";

// interchainSecurityModule() and defaultIsm(), both zero argument.
const SEL_ISM = "0xde523cf3";
const SEL_DEFAULT_ISM = "0x6e5f516e";

export const shortId = (id: string): string =>
  id.length > 22 ? `${id.slice(0, 10)}…${id.slice(-8)}` : id;

async function ethCall(chain: EvmChain, to: string, data: string): Promise<string> {
  const response = await fetch(chain.rpc, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_call", params: [{ to, data }, "latest"] }),
  });
  if (!response.ok) throw new Error(`${chain.name} rpc returned ${response.status}`);
  const body = await response.json();
  if (body.error) throw new Error(body.error.message ?? "eth_call failed");
  return body.result as string;
}

async function getJson(url: string): Promise<any> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);
  return response.json();
}

/// An EVM destination. The mailbox asks the *recipient* which ISM it wants, and the recipient
/// is the warp router, so that is what gets asked here. A router that names none is answered
/// for by the mailbox default, which is a different contract and worth saying out loud.
async function evmIsm(chain: EvmChain, token: TokenId): Promise<WiredIsm> {
  const router = routerFor(token, chain.id);
  if (!router) throw new Error(`no ${token} router on ${chain.name}`);

  const raw = await ethCall(chain, router, SEL_ISM);
  let id = `0x${raw.slice(-40)}`;
  let via: string | null = null;

  if (id.toLowerCase() === ZERO_ADDRESS) {
    const fallback = await ethCall(chain, chain.mailbox, SEL_DEFAULT_ISM);
    id = `0x${fallback.slice(-40)}`;
    via = "the router names no ISM, so the mailbox default applies";
  }
  // `?tab=contract` opens on the source, which is the reason to follow this link at all.
  return { id, url: `${chain.explorer}/address/${id}?tab=contract`, via };
}

/// A Celestia destination. The token points at a routing ISM, which fans out by origin domain,
/// so the ISM that actually verifies depends on where the message came from. Showing only the
/// routing ISM would hide the one that does the work.
async function celestiaIsm(chain: CosmosChain, token: TokenId, origin: ChainId): Promise<WiredIsm> {
  const tokenId = routerFor(token, "celestia");
  if (!tokenId) throw new Error(`no ${token} token on ${chain.name}`);

  const { tokens } = await getJson(`${chain.rest}/hyperlane/v1/tokens`);
  const record = (tokens ?? []).find((t: any) => t.id === tokenId);
  if (!record?.ism_id) throw new Error(`${chain.name} reports no ISM for this token`);

  const routing = record.ism_id as string;
  const detail = await getJson(`${chain.rest}/hyperlane/v1/isms/${routing}`);
  const routes = detail?.ism?.routes ?? [];
  const domain = CHAINS[origin].domain;
  const match = routes.find((r: any) => Number(r.domain) === domain);

  // No route for this origin means nothing on Celestia will accept the message. Better to say
  // so here than to let it dispatch and sit undelivered.
  if (!match) {
    return {
      id: routing,
      url: `${chain.rest}/hyperlane/v1/isms/${routing}`,
      via: `no route for domain ${domain}, so this transfer cannot be verified on arrival`,
    };
  }
  return {
    id: match.ism as string,
    url: `${chain.rest}/hyperlane/v1/isms/${match.ism}`,
    via: `routing ISM ${shortId(routing)} sends domain ${domain} here`,
  };
}

export async function resolveWiredIsm(
  token: TokenId,
  from: ChainId,
  to: ChainId,
): Promise<WiredIsm> {
  const destination = CHAINS[to];
  return destination.kind === "evm"
    ? evmIsm(destination, token)
    : celestiaIsm(destination, token, from);
}
