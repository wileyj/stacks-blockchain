# RPC Endpoints

### POST /v2/transactions

This endpoint is for posting _raw_ transaction data to the node's mempool.

Rejections result in a 400 error, with JSON data in the form:

```json
{
  "error": "transaction rejected",
  "reason": "BadNonce",
  "reason_data": {
    "actual": 3,
    "expected": 0,
    "is_origin": true,
    "principal": "ST2MVNFYF6H9DCMAV3HVNHTJVVE3CFWT1JYMH1EZB"
  },
  "txid": "0x4068179cb9169b969c80518d83890f8b808a70ab998dd227149221be9480a616"
}
```

Possible values for the "reason" field and "reason_data" field are:

* `Serialization`
   * The `reason_data` field will be an object containing a `message`
     string detailing the serialization error
* `Deserialization`
   * The `reason_data` field will be an object containing a `message`
     string detailing the deserialization error
* `EstimatorError`
   * The `reason_data` field will be an object containing a `message`
     string detailing the error
* `SignatureValidation`
   * The `reason_data` field will be an object containing a `message`
     string detailing the signature validation error
* `BadNonce`
   * The `reason_data` field will be an object containing:
     * `expected` - a number representing the expected nonce,
     * `actual` - a number representing the actual nonce,
     * `is_origin` - a boolean representing whether the nonce error
       occurred on the 'origin' or 'sponsor' of the transaction,
     * `principal` - a string representing the principal address
       that had the bad nonce
* `FeeTooLow`
   * The `reason_data` field will be an object containing:
     * `expected` - a number representing the minimum expected fee,
     * `actual` - a number representing the supplied fee
* `NotEnoughFunds`
   * The `reason_data` field will be an object containing:
     * `expected` - a hex string representing the expected
       number of microstacks
     * `actual` - a hex string representing the actual
       number of microstacks the account possesses
* `NoSuchContract`
* `NoSuchPublicFunction`
* `BadFunctionArgument`
   * The `reason_data` field will be an object containing a `message`
     string detailing why the supplied argument was bad.
* `ContractAlreadyExists`
   * The `reason_data` field will be an object containing a `contract_identifier`
     string representing the contract identifier that would be duplicated.
* `PoisonMicroblocksDoNotConflict`
* `PoisonMicroblockHasUnknownPubKeyHash`
* `PoisonMicroblockIsInvalid`
* `BadAddressVersionByte`
* `NoCoinbaseViaMempool`
* `NoTenureChangeViaMempool`
* `ServerFailureNoSuchChainTip`
* `ServerFailureDatabase`
   * The `reason_data` field will be an object containing a `message`
     string detailing why the server had a database error
* `ServerFailureOther`
   * The `reason_data` field will be an object containing a `message`
     string providing more detail on the server failure

Reason types without additional information will not have a
`reason_data` field.

### GET /v2/pox

Get current PoX-relevant information. See OpenAPI [spec](./rpc/openapi.yaml) for details.

### GET /v2/headers/[Count]

Get a given number of ancestral Stacks block headers, in order from newest to
oldest.  If the `?tip=` query parameter is given, the headers will be loaded
from the block identified by the tip.  If no `?tip=` query parameter is given,
then the canonical Stacks chain tip will be used.  The first header in the list
is the header of the `?tip=` query parameter (or the canonical tip of the blockchain);
the second header is the parent block's header; the third header is the
grandparent block's header, and so on. [Count] determines how many headers, including this first header, to return.

Up to 2100 headers (one PoX reward cycle) may be returned by this endpoint.
Callers who wish to download more headers will need to issue this query
multiple times, with a `?tip=` query parameter set to the index block hash of
the earliest header received.

Returns a
[SIP-003](https://github.com/stacksgov/sips/blob/main/sips/sip-003/sip-003-peer-network.md)-encoded
vector with length up to [Count] that contains a list of the following SIP-003-encoded
structures:

```rust
struct ExtendedStacksHeader {
    consensus_hash: ConsensusHash,
    header: StacksBlockHeader,
    parent_block_id: StacksBlockId,
}
```

Where `ConsensusHash` is a 20-byte byte buffer.

Where `StacksBlockId` is a 32-byte byte buffer.

Where `StacksBlockHeader` is the following SIP-003-encoded structure:

```rust
struct StacksBlockHeader {
    version: u8,
    total_work: StacksWorkScore,
    proof: VRFProof,
    parent_block: BlockHeaderHash,
    parent_microblock: BlockHeaderHash,
    parent_microblock_sequence: u16,
    tx_merkle_root: Sha512Trunc256Sum,
    state_index_root: TrieHash,
    microblock_pubkey_hash: Hash160,
}
```

Where `BlockHeaderHash`, `Sha512Trunc256Sum`, and `TrieHash` are 32-byte byte
buffers.

Where `Hash160` is a 20-byte byte buffer.

Where `StacksWorkScore` and `VRFProof` are the following SIP-003-encoded structures:

```rust
struct StacksWorkScore {
    burn: u64,
    work: u64,
}
```

```rust
struct VRFProof {
    Gamma: [u8; 32]
    c: [u8; 16]
    s: [u8; 32]
}
```

The interpretation of most these fields is beyond the scope of this document (please
see
[SIP-005](https://github.com/stacksgov/sips/blob/main/sips/sip-005/sip-005-blocks-and-transactions.md)
for details).  However, it is worth pointing out that `parent_block_id` is a
valid argument to the `?tip=` query parameter.  If the caller of this API
endpoint wants to receive more than 2100 contiguous headers, it would use the
oldest header's `parent_block_id` field from the previous call as the `?tip=`
argument to the next call in order to fetch the next batch of ancestor headers.

This API endpoint may return a list of zero headers if `?tip=` refers to the
hash of the Stacks genesis block.

This API endpoint will return HTTP 404 if the `?tip=` argument is given but
refers to a nonexistent Stacks block, or a Stacks block that has not yet been
processed by the node.

The `?tip=` argument may refer to a Stacks block that is not on the canonical
fork.  In this case, this endpoint behaves as described above, except that
non-canonical headers will be returned instead.

### GET /v2/accounts/[Principal]

Get the account data for the provided principal.
The principal string is either a Stacks address or a Contract identifier (e.g., 
`SP31DA6FTSJX2WGTZ69SFY11BH51NZMB0ZW97B5P0.get-info`

Returns JSON data in the form:

```json
{
 "balance": "0x100..",
 "nonce": 1,
 "balance_proof": "0x01fa...",
 "nonce_proof": "0x01ab...",
}
```

Where balance is the hex encoding of a unsigned 128-bit integer
(big-endian), nonce is a unsigned 64-bit integer, and the proofs are
provided as hex strings.

For non-existent accounts, this _does not_ 404, rather it returns an
object with balance and nonce of 0.

This endpoint also accepts a querystring parameter `?proof=` which when supplied `0`, will return the
JSON object _without_ the `balance_proof` or `nonce_proof` fields.

### GET /v2/data_var/[Stacks Address]/[Contract Name]/[Var Name]

Attempt to vetch a data var from a contract. The contract is identified with [Stacks Address] and
 [Contract Name] in the URL path. The variable is identified with [Var Name].
 
Returns JSON data in the form:

```json
{
 "data": "0x01ce...",
 "proof": "0x01ab...",
}
```

Where data is the hex serialization of the variable value.

This endpoint also accepts a querystring parameter `?proof=` which when supplied `0`, will return the
JSON object _without_ the `proof` field.

### GET /v2/constant_val/[Stacks Address]/[Contract Name]/[Constant Name]
Attempt to fetch a constant from a contract. The contract is identified with [Stacks Address] and 
 [Contract Name] in the URL path. The constant is identified with [Constant Name].

Returns JSON data in the form:

```json
{
  "data": "0x01ce...",
}
```

Where data is the hex serialization of the constant value.

### POST /v2/map_entry/[Stacks Address]/[Contract Name]/[Map Name]

Attempt to fetch data from a contract data map. The contract is identified with [Stacks Address] and
 [Contract Name] in the URL path. The map is identified with [Map Name].
 
The _key_ to lookup in the map is supplied via the POST body. This should be supplied as the hex string
serialization of the key (which should be a Clarity value). Note, this is a _JSON_ string atom.

Returns JSON data in the form:

```json
{
 "data": "0x01ce...",
 "proof": "0x01ab...",
}
```

Where data is the hex serialization of the map response. Note that map responses are Clarity _option_ types,
for non-existent values, this is a serialized `none`, and for all other responses, it is a serialized `(some ...)`
object.

This endpoint also accepts a querystring parameter `?proof=` which when supplied `0`, will return the
JSON object _without_ the `proof` field.

### GET /v2/fees/transfer

Get an estimated fee rate for STX transfer transactions. This a a fee rate / byte, and is returned as a JSON integer.

### GET /v2/contracts/interface/[Stacks Address]/[Contract Name]

Fetch the contract interface for a given contract, identified by [Stacks Address] and [Contract Name].

This returns a JSON object of the form:

```json
{
  "functions": [
    {
      "name": "exotic-block-height",
      "access": "private",
      "args": [
        {
          "name": "height",
          "type": "uint128"
        }
      ],
      "outputs": {
        "type": "bool"
      }
    },
    {
      "name": "update-info",
      "access": "public",
      "args": [],
      "outputs": {
        "type": {
          "response": {
            "ok": "bool",
            "error": "none"
          }
        }
      }
    },
    {
      "name": "get-exotic-data-info",
      "access": "read_only",
      "args": [
        {
          "name": "height",
          "type": "uint128"
        }
      ],
      "outputs": {
        "type": {
          "tuple": [
            {
              "name": "btc-hash",
              "type": {
                "buffer": {
                  "length": 32
                }
              }
            },
            {
              "name": "burn-block-time",
              "type": "uint128"
            },
            {
              "name": "id-hash",
              "type": {
                "buffer": {
                  "length": 32
                }
              }
            },
            {
              "name": "stacks-hash",
              "type": {
                "buffer": {
                  "length": 32
                }
              }
            },
            {
              "name": "stacks-miner",
              "type": "principal"
            },
            {
              "name": "vrf-seed",
              "type": {
                "buffer": {
                  "length": 32
                }
              }
            }
          ]
        }
      }
    }
  ],
  "variables": [],
  "maps": [
    {
      "name": "block-data",
      "key": [
        {
          "name": "height",
          "type": "uint128"
        }
      ],
      "value": [
        {
          "name": "btc-hash",
          "type": {
            "buffer": {
              "length": 32
            }
          }
        },
        {
          "name": "burn-block-time",
          "type": "uint128"
        },
        {
          "name": "id-hash",
          "type": {
            "buffer": {
              "length": 32
            }
          }
        },
        {
          "name": "stacks-hash",
          "type": {
            "buffer": {
              "length": 32
            }
          }
        },
        {
          "name": "stacks-miner",
          "type": "principal"
        },
        {
          "name": "vrf-seed",
          "type": {
            "buffer": {
              "length": 32
            }
          }
        }
      ]
    }
  ],
  "fungible_tokens": [],
  "non_fungible_tokens": []
}
```

### GET /v2/contracts/source/[Stacks Address]/[Contract Name]

Fetch the source for a smart contract, along with the block height it was
published in, and the MARF proof for the data.

```json
{
 "source": "(define-private ...",
 "publish_height": 1,
 "proof": "0x00213..."
}
```

This endpoint also accepts a querystring parameter `?proof=` which
when supplied `0`, will return the JSON object _without_ the `proof`
field.

### POST /v2/contracts/call-read/[Stacks Address]/[Contract Name]/[Function Name]

Call a read-only public function on a given smart contract.

The smart contract and function are specified using the URL path. The arguments and
the simulated `tx-sender` are supplied via the POST body in the following JSON format:

```json
{
  "sender": "SP31DA6FTSJX2WGTZ69SFY11BH51NZMB0ZW97B5P0.get-info",
  "arguments": [ "0x0011...", "0x00231..." ]
}
```

Where sender is either a Contract identifier or a normal Stacks address, and arguments
is an array of hex serialized Clarity values.

This endpoint returns a JSON object of the following form:

```json
{
  "okay": true,
  "result": "0x0011..."
}
```

Where `"okay"` is `true` if the function executed successfully, and result contains the
hex serialization of the Clarity return value.

If an error occurs in processing the function call, this endpoint returns a 200 response with a JSON
object of the following form:

```json
{
  "okay": false,
  "cause": "Unchecked(PublicFunctionNotReadOnly(..."
}
```

### GET /v2/traits/[Stacks Address]/[Contract Name]/[Trait Stacks Address]/[Trait Contract Name]/[Trait Name]

Determine whether a given trait is implemented within the specified contract (either explicitly or implicitly).

See OpenAPI [spec](./rpc/openapi.yaml) for details.

### POST /v2/block_proposal

Used by miner to validate a proposed Stacks block using JSON encoding.

**This endpoint will only accept requests over the local loopback network interface.**

This endpoint takes as input the following struct from `chainstate/stacks/miner.rs`:

```rust
pub struct NakamotoBlockProposal {
    /// Proposed block
    pub block: NakamotoBlock,
    /// Identifies which chain block is for (Mainnet, Testnet, etc.)
    pub chain_id: u32,
}
```

#### Responses over the Event Observer Interface

This endpoint returns asynchronous results to the caller via the event observer interface.
A caller must have registered an event observer using the `block_proposal` key in the stacks-node
config file.

The result is issued via POSTing the response JSON over the `/proposal_response` endpoint on the
registered observer.

Ok response example:

```json
{
    "result": "Ok",
    "block": "00000000000000001f00000000000927c08fb5ae5bf80e39e4168f6a3fddb0407a069d21ee68465e6856393254d2a66194f44bb01070666d5effcfb2436e209a75878fe80a04b4258a8cd34ab97c38a8dde331a2a509dd7e4b90590726866172cc138c18e80567737667f55d3f9817ce4714c91d1adfd36101141829dc0b5ea0c4944668c0005ddb6f9e2718f60014f21932a42a36ffaf58e88e77b217b2af366c15dd59e6b136ca773729832dcfc5875ec0830d04012dd5a4fa77a196646ea2b356289116fd02558c034b62d63f8a65bdd20d7ffc3fec6c266cd974be776a9e92759b90f288dcc2525b6b6bd5622c5f02e0922440e9ad1095c19b4467fd94566caa9755669d8e0000000180800000000400f64081ae6209dce9245753a4f764d6f168aae1af00000000000000000000000000000064000041dbcc7391991c1a18371eb49b879240247a3ec7f281328f53976c1218ffd65421dbb101e59370e2c972b29f48dc674b2de5e1b65acbd41d5d2689124d42c16c01010000000000051a346048df62be3a52bb6236e11394e8600229e27b000000000000271000000000000000000000000000000000000000000000000000000000000000000000",
    "cost": {
        "read_count": 8,
        "read_length":133954,
        "runtime":139720,
        "write_count":2,
        "write_length":114
    },
    "size": 180
}
```

Error examples:

```json
{
  "result": "Reject",
  "reason": "Chainstate Error: No sortition for block's consensus hash",
  "reason_code": "ChainstateError"
}
```

```json
{
  "result": "Reject",
  "reason": "Wrong network/chain_id",
  "reason_code": "InvalidBlock"
}
```

```json
{
  "result": "Reject",
  "reason": "Chainstate Error: Invalid miner signature",
  "reason_code": "ChainstateError"
}
```
