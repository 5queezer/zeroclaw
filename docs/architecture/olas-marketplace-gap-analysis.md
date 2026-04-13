# Hrafn / Olas Marketplace Gap Analysis & Integration Plan

## Context

[Olas](https://olas.network) is a decentralized autonomous agent ecosystem with an on-chain marketplace, staking rewards (Proof of Active Agent), and an agent-to-agent service economy (Mech Marketplace). To be competitive on Olas, Hrafn needs blockchain/web3 capabilities it currently lacks entirely.

This document identifies the gaps and proposes a phased integration strategy using the **Olas SDK wrapper path** (framework-agnostic Docker packaging) rather than rewriting as a Python Open Autonomy FSM app.

### Two Paths to Olas

| Path | Approach | Feasibility |
|---|---|---|
| **Open Autonomy** | Rewrite as Python FSM app with Tendermint ABCI consensus | Not feasible for Hrafn |
| **Olas SDK Wrapper** | Package existing agent as Docker image with Olas config files | **Recommended** |

The Olas SDK wrapper approach ([olas-sdk-starter](https://github.com/valory-xyz/olas-sdk-starter)) allows any agent framework to register on the marketplace by providing standardized configuration (`aea-config.yaml`, `service.yaml`) and a Docker image. Hrafn runs as a **sovereign agent** (single-operator) initially, with optional upgrade to decentralized (multi-operator) mode later.

---

## Gap Analysis Summary

### What Hrafn Already Has (Strengths)

| Capability | Hrafn Location | Olas Relevance |
|---|---|---|
| Docker runtime adapter | `src/runtime/docker.rs` | Required for Olas deployment |
| A2A protocol v1.0 | `src/gateway/a2a.rs` | Maps to agent-to-agent Mech interactions |
| ACP protocol v0.2.0 | `src/gateway/acp.rs` | Complementary agent communication |
| HTTP/WS gateway (axum) | `src/gateway/mod.rs` | Service endpoint for sovereign agents |
| 115+ tools via `Tool` trait | `src/tools/traits.rs` | Maps directly to Mech tool offerings |
| ECDSA P-256 signing | `src/verifiable_intent/crypto.rs` | Cryptographic foundation (Olas uses secp256k1) |
| SOP workflows | `src/sop/types.rs` | Basis for FSM-like Mech task orchestration |
| Node transport (HMAC-SHA256) | `src/nodes/transport.rs` | Inter-agent authenticated communication |
| WASM plugins (extism) | `src/plugins/` | Extensibility for Mech tool packages |
| 13 LLM providers | `src/providers/` | AI task execution backbone |
| Security policy engine | `src/security/policy.rs` | Transaction approval gating |
| Cost tracking | `src/cost/` | Budget enforcement for on-chain operations |
| Daemon supervisor | `src/daemon/mod.rs` | Long-running Mech listener hosting |
| Cron scheduler | `src/cron/` | PoAA checkpoint scheduling |
| Observability (Prom + OTel) | `src/observability/` | Staking/Mech metrics |

### Critical Gaps (Must Have)

| Gap | Impact | Difficulty |
|---|---|---|
| **No EVM wallet / signing** | Cannot self-custody, sign transactions, or hold tokens | High |
| **No OLAS token handling** | Cannot participate in staking or earn rewards | High |
| **No on-chain registration** | Cannot mint NFTs or register on marketplace | Medium |
| **No IPFS integration** | Cannot publish packages to Olas registry | Low |
| **No Olas SDK packaging** | Cannot be listed on marketplace at all | Low |
| **No Mech request/response protocol** | Cannot earn revenue fulfilling AI tasks | High |
| **No Safe (account abstraction)** | Cannot meet Olas self-custody standard | Medium |

### Important Gaps (Needed for Competitiveness)

| Gap | Impact | Difficulty |
|---|---|---|
| No Tendermint/BFT consensus | Blocks decentralized (multi-operator) mode | Very High |
| No ACN/libp2p P2P networking | Cannot use Olas native peer discovery | Very High |
| No deterministic state replication | Cannot run as replicated multi-agent service | Very High |
| No PoAA checkpoint mechanism | Cannot prove liveness for staking rewards | Medium |

### Non-Gaps (Olas Doesn't Require for SDK Path)

- FSM-based architecture (only needed for Open Autonomy, not SDK wrapper)
- Python runtime (Olas SDK wraps any framework)
- Tendermint consensus (not needed for sovereign agents)

---

## Implementation Plan

### Phase 0: Olas SDK Packaging (Quick Win)

**Goal:** Get Hrafn listed on marketplace.olas.network as a sovereign agent.
**Risk tier:** Low (config/docs only, no `src/` changes)
**Feature flag:** None needed

#### Files to create

1. **`olas/Dockerfile.olas`** -- Thin wrapper atop existing Hrafn release image
   - Base: `ghcr.io/5queezer/hrafn:latest`
   - Entrypoint: start daemon mode, expose gateway port
   - ENV vars: `SAFE_CONTRACT_ADDRESS`, `ALL_PARTICIPANTS`, `OLAS_SERVICE_ID`
   - Health endpoint compatible with Olas service monitoring

2. **`olas/aea-config.yaml`** -- Agent blueprint config for Olas registry
   ```yaml
   agent_name: hrafn
   author: 5queezer
   version: 0.1.0
   license: Apache-2.0
   description: "Rust autonomous agent runtime -- multi-provider LLM, 15+ channels, 115+ tools, SOP workflows"
   ```

3. **`olas/service.yaml`** -- Service definition for marketplace registration
   - Agent reference: `5queezer/hrafn:0.1.0:<ipfs_hash>`
   - Number of agents: 1 (sovereign mode)
   - Docker image reference with `oar-` prefix naming convention (Olas Agent Runtime convention: namespace = author, image = `oar-<blueprint>`, tag = package hash)

4. **`olas/docker-compose-olas.yml`** -- Compose file for Olas deployment

5. **`olas/README.md`** -- Operator instructions for minting and registration

#### Registration workflow

```
autonomy packages sync --update-packages
autonomy packages lock          # populates packages.json
autonomy push-all               # push to IPFS registry
# Then mint at marketplace.olas.network
```

---

### Phase 1: EVM Wallet Integration

**Goal:** Self-custody via Ethereum wallet + Safe account abstraction.
**Risk tier:** High (security-sensitive key management)
**Feature flag:** `olas` (new, not in `default`)

#### Dependency

Use `alloy` (not deprecated `ethers-rs`): modular, fine-grained feature flags, active development.

```toml
alloy = { version = "1.8", optional = true, default-features = false, features = [
    "signer-local", "provider-http", "contract", "network", "sol-types",
] }
```

#### New modules

| File | Purpose |
|---|---|
| `src/wallet/mod.rs` | Module root, gated by `#[cfg(feature = "olas")]` |
| `src/wallet/traits.rs` | `Wallet` trait: `address()`, `sign_message()`, `sign_transaction()`, `balance()` |
| `src/wallet/keystore.rs` | Encrypted EVM key storage, reusing `SecretStore` (ChaCha20-Poly1305). V1 uses raw private key; HD wallet / BIP-39 mnemonic support deferred to v2. |
| `src/wallet/signer.rs` | Wrapper around `alloy::signers::local::PrivateKeySigner` |
| `src/wallet/provider.rs` | JSON-RPC provider config (RPC URL, chain ID) for Gnosis and Ethereum |
| `src/wallet/safe.rs` | Safe transaction service API (1-of-1 multisig for sovereign agents) |
| `src/wallet/tx_manager.rs` | Nonce sequencing, EIP-1559 gas estimation, retry/resubmission for dropped transactions |
| `src/tools/evm_wallet.rs` | `Tool` impl: `balance`, `sign_message`, `send_transaction`, `call_contract` |

#### Existing files to modify

- `Cargo.toml` -- Add `olas = ["dep:alloy"]` feature flag
- `src/lib.rs` -- Add `#[cfg(feature = "olas")] pub mod wallet;`
- `src/config/schema.rs` -- Add `OlasConfig` section
- `src/tools/mod.rs` -- Register `EvmWalletTool` in `all_tools_with_runtime()`

#### Config schema

```rust
pub struct OlasConfig {
    pub enabled: bool,                    // default: false
    pub rpc_url: Option<String>,          // e.g. "https://rpc.gnosischain.com"
    pub chain_id: u64,                    // default: 100 (Gnosis)
    pub keystore_path: Option<String>,    // default: ~/.hrafn/.evm_key
    pub safe_address: Option<String>,     // Safe multisig address
    pub service_id: Option<u64>,          // Olas service registry ID
    pub staking: OlasStakingConfig,
    pub mech: OlasMechConfig,
}

pub struct OlasMechConfig {
    pub enabled: bool,                    // default: false
    pub contract_address: Option<String>, // Mech marketplace contract (Gnosis: 0x735F...0bB)
    pub max_pending_requests: u32,        // default: 10
    pub result_timeout_secs: u64,         // default: 300
    pub ipfs_pinning_url: Option<String>, // Pinata/Storacha endpoint
    pub ipfs_api_key: Option<String>,     // Pinning service API key
}

pub struct OlasStakingConfig {
    pub enabled: bool,                    // default: false
    pub service_registry: Option<String>, // Service Registry contract address
    pub staking_contract: Option<String>, // Staking contract address
    pub checkpoint_cron: Option<String>,  // default: "0 */4 * * *"
    pub auto_claim_rewards: bool,         // default: false
}
```

Note: The checkpoint cron interval should be derived from the staking contract's `livenessRatio * epochLength` parameters, not hardcoded. The `"0 */4 * * *"` default is a conservative starting point; operators must verify against their chosen staking program.

---

### Phase 2: Mech Protocol Integration

**Goal:** Accept and fulfill AI task requests on the Olas Mech marketplace for revenue.
**Risk tier:** High (gateway + tools + security boundary)
**Feature flag:** Reuses `olas` (depends on wallet from Phase 1)

#### Architecture mapping

| Mech Concept | Hrafn Equivalent | Integration Point |
|---|---|---|
| Mech request (on-chain event) | New `SopTrigger::OnChain` | `src/sop/types.rs` |
| Task execution | Agent tool loop | `src/agent/loop_.rs` |
| Task lifecycle tracking | A2A `TaskStore` pattern | `src/gateway/a2a.rs` |
| Result delivery (on-chain + IPFS) | New responder module | `src/mech/responder.rs` |
| Mech tool packages | Existing `Tool` trait impls | `src/tools/traits.rs` |

#### Mech modules

| File | Purpose |
|---|---|
| `src/mech/mod.rs` | Module root, gated by `#[cfg(feature = "olas")]` |
| `src/mech/types.rs` | `MechRequest`, `MechResponse`, `MechTaskStatus` |
| `src/mech/contracts.rs` | ABI bindings via `alloy::sol!` for Mech marketplace (Gnosis: `0x735FAAb1c4Ec41128c367AFb5c3baC73509f70bB`) |
| `src/mech/listener.rs` | Poll/subscribe to on-chain events for incoming requests |
| `src/mech/executor.rs` | Map Mech request to scoped agent execution |
| `src/mech/responder.rs` | Hash result, upload to IPFS, submit delivery transaction |
| `src/mech/ipfs.rs` | IPFS upload via pinning service API (uses existing `reqwest`) |
| `src/tools/mech_tool.rs` | `Tool` impl for Mech marketplace interaction |

#### IPFS strategy

Use pinning service HTTP API (**Pinata** or **Storacha**, the successor to web3.storage which was sunset Jan 2024) -- no new dependencies needed beyond `reqwest`.

---

### Phase 3: OLAS Staking (PoAA)

**Goal:** Stake OLAS tokens and earn rewards via Proof of Active Agent checkpoints.
**Risk tier:** High (on-chain financial operations)
**Feature flag:** Reuses `olas`

#### Staking modules

| File | Purpose |
|---|---|
| `src/staking/mod.rs` | Module root, gated by `#[cfg(feature = "olas")]` |
| `src/staking/registry.rs` | Olas Service Registry contract interaction |
| `src/staking/checkpoint.rs` | Periodic `checkpoint()` -- integrates with `src/cron/` |
| `src/staking/rewards.rs` | Query and claim accumulated OLAS rewards |
| `src/staking/contracts.rs` | ABI bindings for staking contracts |
| `src/tools/staking_tool.rs` | `Tool` impl: staking status, manual checkpoint, view rewards |

#### Cron integration

PoAA checkpoints map naturally to Hrafn's existing cron system. The interval should be derived from the staking contract's `livenessRatio * epochLength` and must be shorter than `maxAllowedInactivity` to avoid eviction:

```toml
[[cron.jobs]]
name = "olas_checkpoint"
expression = "0 */4 * * *"  # Conservative default; adjust per staking program
```

---

### Phase 4: Multi-Agent Consensus (Future, Optional)

**Goal:** Enable decentralized (multi-operator) mode with Tendermint BFT consensus.
**Recommendation:** **Defer.** Sovereign mode is sufficient for marketplace listing, Mech fulfillment, and staking rewards.

If pursued later, the **sidecar Tendermint** approach (Docker sidecar communicating via ABCI over localhost) is most pragmatic.

---

## Cross-Cutting Concerns

- **Binary size:** `alloy` with minimal features adds ~2-4 MB behind feature flag. Not in `default`.
- **Security:** EVM keys encrypted at rest via `SecretStore`. Transactions gated by `SecurityPolicy` autonomy level. V1 uses raw private key; HD wallet (BIP-39/BIP-44) and key rotation deferred to v2.
- **Transaction management:** A dedicated `src/wallet/tx_manager.rs` module handles nonce sequencing (critical when Mech responses and staking checkpoints fire concurrently), EIP-1559 gas estimation with fee caps, and automatic retry/resubmission of dropped transactions with nonce bumping.
- **Observability:** Prometheus counters + OTel spans for Mech requests, staking checkpoints, transaction lifecycle.
- **Config migration:** All new sections use `#[serde(default)]` with `enabled: false`. Zero impact on existing deployments.
- **PR discipline:** One concern per PR, conventional commits, size S/M.

## PR Sequence

| # | Title | Risk |
|---|---|---|
| 0 | `docs: Olas marketplace gap analysis` | Low |
| 1 | `feat(olas): Olas SDK packaging -- Dockerfile, aea-config, service.yaml` | Low |
| 2 | `feat(olas): alloy dependency + feature flag + OlasConfig schema` | Low |
| 3 | `feat(olas): wallet module -- keystore, signer, provider` | High |
| 4 | `feat(olas): evm_wallet tool registration` | High |
| 5 | `feat(olas): Safe account abstraction integration` | High |
| 6 | `feat(olas): mech types + contract ABI bindings` | Medium |
| 7 | `feat(olas): mech listener + daemon integration` | High |
| 8 | `feat(olas): mech executor + IPFS responder` | High |
| 9 | `feat(olas): SopTrigger::OnChain variant + mech_tool` | Medium |
| 10 | `feat(olas): staking module + checkpoint cron` | High |
| 11 | `feat(olas): staking_tool` | High |
| 12 | `chore(ci): add olas to ci-all feature flag` | Low |
